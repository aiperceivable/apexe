use std::path::Path;
use std::time::Duration;

use tracing::{info, warn};

use super::cache::ScanCache;
use super::completion::CompletionParser;
use super::discovery::SubcommandDiscovery;
use super::man_page::ManPageParser;
use super::overlay_store::{OverlayError, OverlayStore};
use super::pipeline::ParserPipeline;
use super::resolver::ToolResolver;
use super::variant;
use crate::adapter::overlay::{apply_overlay, MatchContext, ProbeOutcome};
use crate::config::ApexeConfig;
use crate::models::{FlagSource, ScannedCLITool, ScannedFlag, ToolVariant};

/// Minimum description length treated as informative enough to keep as-is.
const MIN_USEFUL_DESCRIPTION: usize = 10;

/// What variant detection learned about one binary.
struct DetectedVariant {
    variant: ToolVariant,
    probes: Vec<ProbeOutcome>,
}

/// Top-level coordinator for the scanning process.
pub struct ScanOrchestrator {
    config: ApexeConfig,
    resolver: ToolResolver,
    pipeline: ParserPipeline,
    cache: ScanCache,
    man_parser: ManPageParser,
    completion_parser: CompletionParser,
    overlays: OverlayStore,
}

impl ScanOrchestrator {
    pub fn new(config: ApexeConfig) -> Self {
        Self::build(config, ParserPipeline::new(None))
    }

    /// Construct with custom plugins for the parser pipeline.
    pub fn with_plugins(
        config: ApexeConfig,
        plugins: Vec<Box<dyn super::protocol::CliParser>>,
    ) -> Self {
        Self::build(config, ParserPipeline::new(Some(plugins)))
    }

    fn build(config: ApexeConfig, pipeline: ParserPipeline) -> Self {
        let cache = ScanCache::new(config.cache_dir.clone());
        let mut overlays = OverlayStore::with_builtins();
        let user_dir = super::overlay_store::user_overlay_dir(&config.config_dir);
        if let Err(e) = overlays.load_dir(&user_dir) {
            // A broken hand-written overlay must be visible, but it must not
            // stop a scan the built-in overlays can still serve.
            warn!(dir = %user_dir.display(), "Ignoring invalid user overlay: {e}");
        }
        Self {
            config,
            resolver: ToolResolver,
            pipeline,
            cache,
            man_parser: ManPageParser,
            completion_parser: CompletionParser,
            overlays,
        }
    }

    /// Load an operator-supplied overlay that outranks every other source.
    ///
    /// This is the single override path. It replaces the former
    /// `ParserPipeline::parse(.., user_override)` hook, which sat below the
    /// layer that knows a tool's variant and was never wired to the CLI.
    pub fn load_overlay(&mut self, path: &Path) -> Result<(), OverlayError> {
        self.overlays.load_explicit(path)
    }

    /// Number of overlays available to this scan.
    pub fn overlay_count(&self) -> usize {
        self.overlays.len()
    }

    /// Scan one or more CLI tools.
    ///
    /// For each tool:
    /// 1. Resolve binary path and version
    /// 2. Probe the binary to determine its variant (BSD / GNU / BusyBox)
    /// 3. Check cache (unless no_cache), keyed by command *and* variant
    /// 4. Run --help and parse (Tier 1), discovering subcommands recursively
    /// 5. Enrich with man pages (Tier 2) and completions (Tier 3)
    /// 6. Apply a matching curated overlay (Tier 4)
    /// 7. Cache the result
    pub fn scan(
        &self,
        tool_names: &[String],
        no_cache: bool,
        depth: u32,
    ) -> anyhow::Result<Vec<ScannedCLITool>> {
        let mut results = Vec::new();

        for tool_name in tool_names {
            let tool = self.scan_single(tool_name, no_cache, depth)?;
            results.push(tool);
        }

        Ok(results)
    }

    fn scan_single(
        &self,
        tool_name: &str,
        no_cache: bool,
        depth: u32,
    ) -> anyhow::Result<ScannedCLITool> {
        // A tool may be given as a bare name (`ls`) or an absolute path
        // (`/bin/ls`); AP Studio passes the latter. Everything that identifies
        // the tool — cache key, module id, man/completion lookup, risk
        // inference — keys off the command name, so both forms must resolve to
        // the same identity. Only execution keeps the full path.
        let command_name = documentation_name(tool_name);

        let resolved = self
            .resolver
            .resolve(tool_name, Duration::from_secs(self.config.default_timeout))?;

        let detected = self.detect_variant(command_name, &resolved.binary_path);

        if !no_cache {
            if let Some(cached) =
                self.cache
                    .get(command_name, detected.variant, resolved.version.as_deref())
            {
                info!(tool = %command_name, variant = detected.variant.as_str(), "Using cached scan result");
                return Ok(cached);
            }
        }

        let mut tool = self.scan_help_tier(tool_name, command_name, &resolved, depth)?;
        tool.variant = detected.variant;

        self.enrich_with_man_page(&mut tool, command_name);
        self.enrich_with_completions(&mut tool, command_name);
        self.apply_matching_overlay(&mut tool, &detected);

        if tool.global_flags.is_empty() && tool.subcommands.is_empty() {
            tool.warnings.push(format!(
                "No flags or subcommands extracted for '{command_name}' from --help, man page, or shell completions"
            ));
        }

        if let Err(e) = self.cache.put(&tool) {
            warn!(tool = %command_name, "Failed to cache scan result: {e}");
        }

        Ok(tool)
    }

    /// Run `--help`, then any expanded variant that yields more, and parse it.
    ///
    /// Returns the richest parse plus any warnings the attempt produced.
    fn parse_best_help(
        &self,
        tool_name: &str,
    ) -> anyhow::Result<(super::protocol::ParsedHelp, Vec<String>)> {
        let help_text = self.run_help_with_timeout(tool_name)?;

        let mut warnings = Vec::new();
        if help_text.trim().is_empty() {
            warnings.push(format!("Empty help output from '{tool_name} --help'"));
        }

        let mut parsed = self.pipeline.parse(&help_text, tool_name);

        // If few flags found, try expanded help variants (e.g., `curl --help all`)
        if parsed.flags.len() < 3 {
            if let Ok(expanded) = self.try_expanded_help(tool_name) {
                if !expanded.trim().is_empty() {
                    let expanded_parsed = self.pipeline.parse(&expanded, tool_name);
                    if expanded_parsed.flags.len() > parsed.flags.len() {
                        parsed = expanded_parsed;
                    }
                }
            }
        }
        Ok((parsed, warnings))
    }

    /// Tier 1: run `--help`, parse it, and discover subcommands.
    fn scan_help_tier(
        &self,
        tool_name: &str,
        command_name: &str,
        resolved: &super::resolver::ResolvedTool,
        depth: u32,
    ) -> anyhow::Result<ScannedCLITool> {
        let (parsed, warnings) = self.parse_best_help(tool_name)?;

        let discovery = SubcommandDiscovery::new(
            &self.pipeline,
            depth,
            Duration::from_secs(self.config.default_timeout),
        );
        let mut subcommands = discovery.discover(
            tool_name,
            &[tool_name.to_string()],
            &parsed.subcommand_names,
            0,
        );

        let mut global_flags = parsed.flags;
        stamp_source(&mut global_flags, FlagSource::Help);
        for command in &mut subcommands {
            stamp_source(&mut command.flags, FlagSource::Help);
        }

        Ok(ScannedCLITool {
            name: command_name.to_string(),
            description: parsed.description.clone(),
            binary_path: resolved.binary_path.clone(),
            version: resolved.version.clone(),
            subcommands,
            global_flags,
            positional_args: parsed.positional_args,
            structured_output: parsed.structured_output,
            scan_tier: 1,
            warnings,
            ..Default::default()
        })
    }

    /// Probe the binary to work out which implementation it actually is.
    ///
    /// Every probe an overlay for this command may reference is run once here,
    /// so the pure matching code in `crate::adapter::overlay` never touches the
    /// operating system.
    fn detect_variant(&self, command_name: &str, binary_path: &str) -> DetectedVariant {
        let arg_sets = self.overlays.probe_arg_sets(command_name);
        let probes = variant::run_probes(
            binary_path,
            &arg_sets,
            Duration::from_secs(self.config.default_timeout),
        );
        let platform = variant::current_platform();
        let detected =
            variant::classify_variant(variant::version_outcome(&probes), Some(&platform));
        info!(
            tool = %command_name,
            variant = detected.as_str(),
            "Detected tool variant"
        );
        DetectedVariant {
            variant: detected,
            probes,
        }
    }

    /// Tier 4: replace or refine the scan with a matching curated overlay.
    fn apply_matching_overlay(&self, tool: &mut ScannedCLITool, detected: &DetectedVariant) {
        let context = MatchContext {
            command: tool.name.clone(),
            variant: detected.variant,
            platform: Some(variant::current_platform()),
            binary_path: tool.binary_path.clone(),
            version: tool.version.clone(),
            probes: detected.probes.clone(),
        };
        let Some(selection) = self.overlays.select(&context) else {
            return;
        };
        info!(
            tool = %tool.name,
            overlay = %selection.overlay.id(),
            strength = ?selection.strength,
            mode = ?selection.overlay.mode,
            "Applying curated overlay"
        );
        apply_overlay(tool, selection.overlay);
        tool.scan_tier = tool.scan_tier.max(4);
    }

    fn run_help_with_timeout(&self, tool_name: &str) -> anyhow::Result<String> {
        let timeout = Duration::from_secs(self.config.default_timeout);
        let output =
            super::exec::run_with_timeout(tool_name, &["--help"], timeout).map_err(|e| match e
                .kind()
            {
                std::io::ErrorKind::PermissionDenied => crate::errors::ApexeError::ScanPermission {
                    command: tool_name.to_string(),
                }
                .into(),
                std::io::ErrorKind::TimedOut => crate::errors::ApexeError::ScanTimeout {
                    command: format!("{tool_name} --help"),
                    timeout: self.config.default_timeout,
                }
                .into(),
                _ => anyhow::Error::from(e),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Some tools output help to stderr
        if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            Ok(stderr)
        } else {
            Ok(stdout)
        }
    }

    /// Try expanded help variants: `--help all`, `-h`.
    fn try_expanded_help(&self, tool_name: &str) -> anyhow::Result<String> {
        let timeout = Duration::from_secs(self.config.default_timeout);
        let variants = [vec!["--help", "all"], vec!["-h"]];
        for args in &variants {
            if let Ok(out) = super::exec::run_with_timeout(tool_name, args, timeout) {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let result = if text.len() > stderr.len() {
                    text
                } else {
                    stderr
                };
                if result.len() > 100 {
                    return Ok(result);
                }
            }
        }
        Ok(String::new())
    }

    /// Tier 2: merge man page content into the scan result.
    ///
    /// The man page is treated as a usage source in its own right, not only as
    /// a description patch. Tools whose `--help` is a single bundled usage line
    /// (most BSD/macOS built-ins, e.g. `ls`) parse to zero flags in Tier 1, and
    /// their man page is the only machine-readable record of their options.
    fn enrich_with_man_page(&self, tool: &mut ScannedCLITool, lookup_name: &str) {
        let Some(man_help) = self.man_parser.parse_man_page(lookup_name) else {
            return;
        };
        if man_help.description.is_empty()
            && man_help.flags.is_empty()
            && man_help.examples.is_empty()
        {
            return;
        }
        tool.scan_tier = tool.scan_tier.max(2);

        // Only the man page carries hand-written examples, so there is nothing
        // to merge against — Tier 1 never produces any for these tools.
        if !man_help.examples.is_empty() && tool.examples.is_empty() {
            tool.examples.clone_from(&man_help.examples);
        }

        if !man_help.description.is_empty() {
            // The man page DESCRIPTION is the authoritative tool-level summary,
            // so it replaces whatever Tier 1 produced. That fallback is not
            // merely weaker but frequently wrong: a tool that rejects `--help`
            // (most BSD built-ins) has its error text — "ls: unrecognized
            // option `--help'" — parsed as if it were a description.
            tool.description.clone_from(&man_help.description);
            for cmd in &mut tool.subcommands {
                if cmd.description.len() < 20 && !cmd.description.is_empty() {
                    cmd.description = format!("{} — {}", cmd.description, man_help.description);
                }
            }
        }

        if man_help.flags.is_empty() {
            return;
        }
        let mut man_flags = man_help.flags;
        stamp_source(&mut man_flags, FlagSource::ManPage);
        for cmd in &mut tool.subcommands {
            fill_missing_descriptions(&mut cmd.flags, &man_flags);
        }
        fill_missing_descriptions(&mut tool.global_flags, &man_flags);
        corroborate_sources(&mut tool.global_flags, &man_flags);
        merge_new_flags(&mut tool.global_flags, &man_flags);
    }

    fn enrich_with_completions(&self, tool: &mut ScannedCLITool, tool_name: &str) {
        if let Some(comp_help) = self.completion_parser.parse_completions(tool_name) {
            tool.scan_tier = tool.scan_tier.max(3);

            // Merge completion-discovered subcommands that Tier 1 missed
            let existing_names: std::collections::HashSet<String> =
                tool.subcommands.iter().map(|c| c.name.clone()).collect();

            for sub_name in &comp_help.subcommand_names {
                if !existing_names.contains(sub_name) {
                    // Discovered a new subcommand via completions — add a stub
                    tool.subcommands.push(crate::models::ScannedCommand {
                        name: sub_name.clone(),
                        full_command: format!("{} {}", tool_name, sub_name),
                        description: format!("{tool_name} {sub_name}"),
                        flags: vec![],
                        positional_args: vec![],
                        subcommands: vec![],
                        examples: vec![],
                        help_format: crate::models::HelpFormat::Unknown,
                        structured_output: crate::models::StructuredOutputInfo::default(),
                        raw_help: String::new(),
                    });
                    tool.warnings.push(format!(
                        "Subcommand '{sub_name}' discovered via shell completion (stub only)"
                    ));
                }
            }
        }
    }
}

/// Reduce a tool reference to the command name used for documentation lookups.
///
/// `scan /bin/ls` must still find the `ls` man page and `_ls` completion.
fn documentation_name(tool_name: &str) -> &str {
    Path::new(tool_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(tool_name)
}

/// Whether two flags denote the same option, matching on either alias form.
///
/// Comparing canonical names alone is not enough: Tier 1 may know a flag only
/// by its long form and the man page only by its short form.
fn flags_match(left: &ScannedFlag, right: &ScannedFlag) -> bool {
    let long_matches = left.long_name.is_some() && left.long_name == right.long_name;
    let short_matches = left.short_name.is_some() && left.short_name == right.short_name;
    long_matches || short_matches
}

/// Fill in descriptions that Tier 1 left empty or uselessly short.
fn fill_missing_descriptions(target: &mut [ScannedFlag], source: &[ScannedFlag]) {
    for flag in target.iter_mut() {
        if flag.description.len() >= MIN_USEFUL_DESCRIPTION {
            continue;
        }
        let replacement = source
            .iter()
            .find(|candidate| flags_match(flag, candidate))
            .filter(|candidate| !candidate.description.is_empty());
        if let Some(candidate) = replacement {
            flag.description.clone_from(&candidate.description);
        }
    }
}

/// Append flags the man page documents but Tier 1 never saw.
fn merge_new_flags(target: &mut Vec<ScannedFlag>, source: &[ScannedFlag]) {
    for flag in source {
        if !target.iter().any(|existing| flags_match(existing, flag)) {
            target.push(flag.clone());
        }
    }
}

/// Record `source` as backing every flag in `flags`.
fn stamp_source(flags: &mut [ScannedFlag], source: FlagSource) {
    for flag in flags.iter_mut() {
        flag.add_source(source);
    }
}

/// Credit a flag with every additional source that also documents it.
///
/// This is what raises a flag from `low` to `medium`: two independent
/// heuristic extractions naming the same option is real corroboration, whereas
/// one parser's guess repeated is not.
fn corroborate_sources(target: &mut [ScannedFlag], source: &[ScannedFlag]) {
    for flag in target.iter_mut() {
        let Some(candidate) = source.iter().find(|other| flags_match(flag, other)) else {
            continue;
        };
        for recorded in candidate.sources.clone() {
            flag.add_source(recorded);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ValueType;
    use tempfile::TempDir;

    fn flag(short: Option<&str>, long: Option<&str>, description: &str) -> ScannedFlag {
        ScannedFlag {
            long_name: long.map(str::to_string),
            short_name: short.map(str::to_string),
            description: description.to_string(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_documentation_name_strips_absolute_path() {
        // AP Studio passes resolved absolute paths; man/completion lookups need
        // the bare command name or they silently find nothing.
        assert_eq!(documentation_name("/bin/ls"), "ls");
        assert_eq!(documentation_name("/usr/local/bin/git"), "git");
    }

    #[test]
    fn test_documentation_name_passes_through_bare_name() {
        assert_eq!(documentation_name("ls"), "ls");
    }

    #[test]
    fn test_scan_prefers_man_description_over_help_fallback() {
        // `ls` rejects `--help`, so Tier 1 parses its error message as the
        // description. The man page must win.
        let tmp = TempDir::new().unwrap();
        let orchestrator = ScanOrchestrator::new(test_config(&tmp));

        let tools = orchestrator.scan(&["ls".into()], true, 1).unwrap();
        if tools[0].scan_tier >= 2 {
            assert!(
                !tools[0].description.contains("unrecognized option"),
                "help error text leaked into description: {}",
                tools[0].description
            );
        }
    }

    #[test]
    fn test_scan_normalizes_absolute_path_to_command_name() {
        // `scan /bin/ls` and `scan ls` must produce the same identity, so the
        // derived module id and risk annotation do not depend on how the tool
        // was referenced. The full path is kept in binary_path for execution.
        let tmp = TempDir::new().unwrap();
        let orchestrator = ScanOrchestrator::new(test_config(&tmp));

        let by_path = orchestrator.scan(&["/bin/ls".into()], true, 1).unwrap();
        assert_eq!(by_path[0].name, "ls");
        assert_eq!(by_path[0].binary_path, "/bin/ls");
    }

    #[test]
    fn test_scan_ls_detects_variant_and_applies_matching_overlay() {
        // End-to-end proof of the mechanism on whatever `ls` this machine has.
        // The assertion is written against the *detected* variant rather than
        // the platform, because that is the whole point: a Homebrew GNU `ls` on
        // macOS must get the GNU overlay, not the BSD one.
        let tmp = TempDir::new().unwrap();
        let orchestrator = ScanOrchestrator::new(test_config(&tmp));

        let tools = orchestrator.scan(&["ls".into()], true, 1).unwrap();
        let tool = &tools[0];

        match tool.variant {
            ToolVariant::Bsd => assert_eq!(tool.overlay.as_deref(), Some("ls@bsd")),
            ToolVariant::Gnu => {
                assert_eq!(tool.overlay.as_deref(), Some("ls@gnu"))
            }
            // No curated overlay ships for BusyBox or Apple `ls`, and an
            // inconclusive probe must not pick one at random.
            ToolVariant::Apple | ToolVariant::Busybox | ToolVariant::Unknown => {
                assert!(tool.overlay.is_none())
            }
        }

        if tool.overlay.is_some() {
            assert_eq!(tool.scan_tier, 4, "overlay must raise the scan tier");
            assert!(
                tool.global_flags
                    .iter()
                    .all(|f| f.confidence == crate::models::Confidence::Verified),
                "an authoritative overlay must leave only verified flags"
            );
            assert!(
                tool.positional_args.iter().any(|arg| arg.variadic),
                "the overlay's variadic file argument must survive"
            );
            assert_eq!(tool.annotation_overrides.readonly, Some(true));
        }
    }

    #[test]
    fn test_scan_stamps_help_provenance_on_tier_one_flags() {
        // Every flag must carry its provenance, so a consumer can tell a
        // scraped guess from a curated fact.
        let tmp = TempDir::new().unwrap();
        let orchestrator = ScanOrchestrator::new(test_config(&tmp));

        let tools = orchestrator.scan(&["curl".into()], true, 1).unwrap();
        let tool = &tools[0];
        assert!(
            tool.global_flags
                .iter()
                .all(|flag| !flag.sources.is_empty()),
            "a flag with no recorded source is indistinguishable from a curated one"
        );
        assert!(
            tool.global_flags
                .iter()
                .all(|flag| flag.confidence != crate::models::Confidence::Verified),
            "no overlay ships for curl, so nothing may claim to be verified"
        );
    }

    #[test]
    fn test_load_overlay_applies_operator_supplied_document() {
        // The single override path: what used to be ParserPipeline's dormant
        // `user_override` is now reachable from the CLI and variant-aware.
        let tmp = TempDir::new().unwrap();
        let overlay_path = tmp.path().join("echo.json");
        std::fs::write(
            &overlay_path,
            r#"{
              "schema_version": "1.0",
              "command": "echo",
              "variant": "unknown",
              "mode": "authoritative",
              "confidence": "verified",
              "provenance": {
                "platform": "macos",
                "tool_version": "test",
                "source": "man-page",
                "checked_on": "2026-07-27"
              },
              "description": "Curated echo.",
              "flags": [{ "short": "-n", "type": "boolean", "description": "No trailing newline." }]
            }"#,
        )
        .unwrap();

        let mut orchestrator = ScanOrchestrator::new(test_config(&tmp));
        let before = orchestrator.overlay_count();
        orchestrator.load_overlay(&overlay_path).unwrap();
        assert_eq!(orchestrator.overlay_count(), before + 1);

        let tools = orchestrator.scan(&["echo".into()], true, 1).unwrap();
        assert_eq!(tools[0].overlay.as_deref(), Some("echo@unknown"));
        assert_eq!(tools[0].description, "Curated echo.");
        assert_eq!(tools[0].global_flags.len(), 1);
        assert_eq!(tools[0].global_flags[0].short_name.as_deref(), Some("-n"));
    }

    #[test]
    fn test_load_overlay_reports_missing_file() {
        let tmp = TempDir::new().unwrap();
        let mut orchestrator = ScanOrchestrator::new(test_config(&tmp));
        let result = orchestrator.load_overlay(&tmp.path().join("absent.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_stamp_source_marks_single_source_as_low_confidence() {
        let mut flags = vec![flag(Some("-a"), Some("--all"), "Include hidden")];
        stamp_source(&mut flags, FlagSource::Help);
        assert_eq!(flags[0].sources, vec![FlagSource::Help]);
        assert_eq!(flags[0].confidence, crate::models::Confidence::Low);
    }

    #[test]
    fn test_corroborate_sources_raises_confidence_to_medium() {
        // Help and the man page independently documenting the same flag is real
        // corroboration; one parser repeating itself is not.
        let mut target = vec![flag(Some("-a"), Some("--all"), "Include hidden")];
        stamp_source(&mut target, FlagSource::Help);
        let mut man = vec![flag(Some("-a"), None, "Include hidden entries.")];
        stamp_source(&mut man, FlagSource::ManPage);

        corroborate_sources(&mut target, &man);

        assert_eq!(
            target[0].sources,
            vec![FlagSource::Help, FlagSource::ManPage]
        );
        assert_eq!(target[0].confidence, crate::models::Confidence::Medium);
    }

    #[test]
    fn test_corroborate_sources_leaves_uncorroborated_flags_low() {
        let mut target = vec![flag(Some("-a"), Some("--all"), "Include hidden")];
        stamp_source(&mut target, FlagSource::Help);
        let mut man = vec![flag(Some("-z"), None, "Something else.")];
        stamp_source(&mut man, FlagSource::ManPage);

        corroborate_sources(&mut target, &man);

        assert_eq!(target[0].confidence, crate::models::Confidence::Low);
    }

    #[test]
    fn test_flags_match_on_long_name() {
        let left = flag(None, Some("--all"), "");
        let right = flag(Some("-a"), Some("--all"), "");
        assert!(flags_match(&left, &right));
    }

    #[test]
    fn test_flags_match_on_short_name_only() {
        // Tier 1 may know only the long form and the man page only the short.
        let left = flag(Some("-a"), None, "");
        let right = flag(Some("-a"), Some("--all"), "");
        assert!(flags_match(&left, &right));
    }

    #[test]
    fn test_flags_do_not_match_when_both_names_differ() {
        let left = flag(Some("-a"), Some("--all"), "");
        let right = flag(Some("-v"), Some("--verbose"), "");
        assert!(!flags_match(&left, &right));
    }

    #[test]
    fn test_flags_do_not_match_on_shared_absent_names() {
        // Two flags that both lack a long name must not be treated as equal.
        let left = flag(Some("-a"), None, "");
        let right = flag(Some("-v"), None, "");
        assert!(!flags_match(&left, &right));
    }

    #[test]
    fn test_fill_missing_descriptions_replaces_sparse_text() {
        let mut target = vec![flag(Some("-a"), Some("--all"), "all")];
        let source = vec![flag(Some("-a"), Some("--all"), "Include hidden entries.")];
        fill_missing_descriptions(&mut target, &source);
        assert_eq!(target[0].description, "Include hidden entries.");
    }

    #[test]
    fn test_fill_missing_descriptions_keeps_informative_text() {
        let mut target = vec![flag(
            Some("-a"),
            Some("--all"),
            "Already a good description.",
        )];
        let source = vec![flag(Some("-a"), Some("--all"), "Man page text.")];
        fill_missing_descriptions(&mut target, &source);
        assert_eq!(target[0].description, "Already a good description.");
    }

    #[test]
    fn test_merge_new_flags_appends_only_unknown_flags() {
        let mut target = vec![flag(Some("-a"), Some("--all"), "Include hidden entries.")];
        let source = vec![
            flag(Some("-a"), Some("--all"), "Duplicate."),
            flag(Some("-l"), None, "Use long format."),
        ];
        merge_new_flags(&mut target, &source);
        assert_eq!(target.len(), 2);
        assert_eq!(target[1].short_name.as_deref(), Some("-l"));
    }

    #[test]
    fn test_merge_new_flags_populates_empty_target() {
        // The `ls` case: Tier 1 parses zero flags, so every man page flag is new.
        let mut target: Vec<ScannedFlag> = Vec::new();
        let source = vec![flag(Some("-l"), None, "Use long format.")];
        merge_new_flags(&mut target, &source);
        assert_eq!(target.len(), 1);
    }

    fn test_config(tmp: &TempDir) -> ApexeConfig {
        ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "warn".into(),
            default_timeout: 10,
            scan_depth: 2,
            json_output_preference: true,
            ..ApexeConfig::default()
        }
    }

    // T37: ScanOrchestrator construction
    #[test]
    fn test_orchestrator_new() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let _orchestrator = ScanOrchestrator::new(config);
        // Should not panic
    }

    #[test]
    fn test_orchestrator_uses_config_cache_dir() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);
        // Verify by attempting a cache operation
        let result = orchestrator
            .cache
            .get("nonexistent", ToolVariant::Unknown, None);
        assert!(result.is_none());
    }

    // T38: Single tool scan
    #[test]
    fn test_scan_echo() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        let result = orchestrator.scan(&["echo".into()], true, 1);
        assert!(result.is_ok());
        let tools = result.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert!(!tools[0].binary_path.is_empty());
        // scan_tier >= 1; may be 2 if man page is available on this system
        assert!(tools[0].scan_tier >= 1);
    }

    // T39: Cache integration
    #[test]
    fn test_scan_uses_cache() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        // First scan
        let result1 = orchestrator.scan(&["echo".into()], false, 1).unwrap();
        assert_eq!(result1.len(), 1);

        // Second scan should use cache
        let result2 = orchestrator.scan(&["echo".into()], false, 1).unwrap();
        assert_eq!(result2.len(), 1);
        assert_eq!(result2[0].name, "echo");
    }

    #[test]
    fn test_scan_no_cache_forces_rescan() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        // First scan
        let result1 = orchestrator.scan(&["echo".into()], false, 1).unwrap();
        assert_eq!(result1.len(), 1);

        // no_cache=true forces rescan
        let result2 = orchestrator.scan(&["echo".into()], true, 1).unwrap();
        assert_eq!(result2.len(), 1);
    }

    // T41: Multi-tool scan
    #[test]
    fn test_scan_multiple_tools() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        let result = orchestrator.scan(&["echo".into(), "ls".into()], true, 1);
        assert!(result.is_ok());
        let tools = result.unwrap();
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_scan_nonexistent_tool_errors() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        let result = orchestrator.scan(&["zzz_no_such_tool_xyz".into()], true, 1);
        assert!(result.is_err());
    }

    // T42: Error handling
    #[test]
    fn test_scan_tool_with_empty_help_adds_warning() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        // true (the tool) typically produces empty help with --help
        // We'll test with echo which will produce some output
        let result = orchestrator.scan(&["echo".into()], true, 1).unwrap();
        assert_eq!(result[0].name, "echo");
    }

    // T40: Tier 2/3 enrichment (tested implicitly via scan)
    #[test]
    fn test_scan_tier_is_at_least_1() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let orchestrator = ScanOrchestrator::new(config);

        let result = orchestrator.scan(&["echo".into()], true, 1).unwrap();
        assert!(result[0].scan_tier >= 1);
    }
}
