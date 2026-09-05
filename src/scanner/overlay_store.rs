//! Loading and selection of curated tool overlays.
//!
//! This is the OS-facing half of the overlay mechanism: it reads overlay
//! documents off disk (or out of the binary, for the curated built-ins) and
//! decides which one applies to a scanned binary. The data model, the match
//! rules and the merge semantics live in [`crate::adapter::overlay`] and stay
//! free of any filesystem or subprocess dependency.
//!
//! # Unified override path
//!
//! `--overlay <PATH>` is the single operator-facing override. It replaces the
//! former `ParserPipeline::parse(.., user_override)` hook, which sat below the
//! layer where variant information exists and was never wired to the CLI.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::debug;

use crate::adapter::overlay::{
    validate_overlay, MatchContext, MatchStrength, OverlayDefect, ToolOverlay,
    OVERLAY_SCHEMA_VERSION,
};

/// Failures while loading overlay documents.
#[derive(Debug, Error)]
pub enum OverlayError {
    #[error("Failed to read overlay '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Overlay '{path}' is not valid JSON or YAML: {message}")]
    Malformed { path: String, message: String },

    #[error(
        "Overlay '{path}' declares schema_version '{found}', but this build supports '{expected}'"
    )]
    UnsupportedVersion {
        path: String,
        found: String,
        expected: String,
    },

    /// The document parsed, but asserts something it is not allowed to assert —
    /// most importantly `confidence: verified` with no provenance to back it.
    #[error("Overlay '{path}' is invalid:\n{}", format_defects(.defects))]
    Invalid {
        path: String,
        defects: Vec<OverlayDefect>,
    },
}

/// Render a defect list as one indented bullet per line.
fn format_defects(defects: &[OverlayDefect]) -> String {
    defects
        .iter()
        .map(|defect| format!("  - {defect}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where an overlay came from, which sets its baseline precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OverlayOrigin {
    /// Compiled into this build.
    Builtin,
    /// Discovered in an overlay directory.
    UserDir,
    /// Named on the command line by the operator.
    Explicit,
}

/// One loaded overlay plus its provenance.
#[derive(Debug, Clone)]
struct StoredOverlay {
    overlay: ToolOverlay,
    origin: OverlayOrigin,
}

/// A resolved overlay selection.
#[derive(Debug, Clone)]
pub struct OverlaySelection<'a> {
    pub overlay: &'a ToolOverlay,
    pub strength: MatchStrength,
}

/// Collection of overlays available to a scan.
#[derive(Debug, Default)]
pub struct OverlayStore {
    entries: Vec<StoredOverlay>,
}

impl OverlayStore {
    /// An empty store, for tests and for callers that opt out of overlays.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Add every `*.json` / `*.yaml` overlay found directly under `dir`.
    ///
    /// A missing directory is not an error: most installs never create one.
    /// A malformed file *is* reported, so a typo in a hand-written overlay does
    /// not silently degrade the scan back to heuristics.
    pub fn load_dir(&mut self, dir: &Path) -> Result<usize, OverlayError> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(0);
        };
        let mut loaded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !has_overlay_extension(&path) {
                continue;
            }
            self.push_from_path(&path, OverlayOrigin::UserDir)?;
            loaded += 1;
        }
        Ok(loaded)
    }

    /// Add an overlay named explicitly by the operator.
    ///
    /// Explicit overlays outrank everything else and skip variant, platform and
    /// probe conditions: naming the file is the operator's own assertion that it
    /// applies. Only the command name still has to agree, so a mistyped path
    /// cannot silently reshape an unrelated tool.
    pub fn load_explicit(&mut self, path: &Path) -> Result<(), OverlayError> {
        self.push_from_path(path, OverlayOrigin::Explicit)
    }

    fn push_from_path(&mut self, path: &Path, origin: OverlayOrigin) -> Result<(), OverlayError> {
        let path_text = path.display().to_string();
        let document = std::fs::read_to_string(path).map_err(|source| OverlayError::Read {
            path: path_text.clone(),
            source,
        })?;
        let overlay = parse_overlay(&path_text, &document)?;
        debug!(overlay = %overlay.id(), path = %path_text, "Loaded overlay");
        self.entries.push(StoredOverlay { overlay, origin });
        Ok(())
    }

    /// Distinct probe argument sets any overlay for `command` may need run.
    ///
    /// The scanner runs these once per binary and hands the outcomes back in a
    /// [`MatchContext`], so probing stays outside the pure matching code.
    pub fn probe_arg_sets(&self, command: &str) -> Vec<Vec<String>> {
        let mut sets: Vec<Vec<String>> = vec![super::variant::version_probe_args()];
        for entry in &self.entries {
            if entry.overlay.command != command {
                continue;
            }
            let Some(ref probe) = entry.overlay.match_rules.probe else {
                continue;
            };
            if !sets.contains(&probe.args) {
                sets.push(probe.args.clone());
            }
        }
        sets
    }

    /// Pick the overlay that best describes `context`, if any.
    ///
    /// Explicit overlays win outright. Otherwise the strongest match wins:
    /// probe > platform + binary_globs > platform alone. Ties are broken toward
    /// user-supplied overlays so a local file can shadow a built-in.
    pub fn select(&self, context: &MatchContext) -> Option<OverlaySelection<'_>> {
        self.entries
            .iter()
            .filter_map(|entry| self.evaluate(entry, context))
            .max_by_key(|(strength, prefer_user, _)| (*strength, *prefer_user))
            .map(|(strength, _, overlay)| OverlaySelection { overlay, strength })
    }

    fn evaluate<'a>(
        &self,
        entry: &'a StoredOverlay,
        context: &MatchContext,
    ) -> Option<(MatchStrength, u8, &'a ToolOverlay)> {
        let prefer_user = u8::from(entry.origin != OverlayOrigin::Builtin);
        if entry.origin == OverlayOrigin::Explicit {
            return (entry.overlay.command == context.command).then_some((
                MatchStrength::Explicit,
                prefer_user,
                &entry.overlay,
            ));
        }
        entry
            .overlay
            .evaluate(context)
            .map(|strength| (strength, prefer_user, &entry.overlay))
    }

    /// Number of loaded overlays.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no overlay is loaded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Whether `path` looks like an overlay document.
fn has_overlay_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "json" | "yaml" | "yml"))
}

/// Parse an overlay document, accepting JSON or YAML.
///
/// `serde_yaml` parses JSON too, so one pass covers both; the JSON error is
/// reported when the text looks like JSON, because it points at the real
/// mistake far better than a YAML indentation complaint would.
fn parse_overlay(path: &str, document: &str) -> Result<ToolOverlay, OverlayError> {
    let overlay: ToolOverlay = if document.trim_start().starts_with('{') {
        serde_json::from_str(document).map_err(|e| OverlayError::Malformed {
            path: path.to_string(),
            message: e.to_string(),
        })?
    } else {
        serde_yaml::from_str(document).map_err(|e| OverlayError::Malformed {
            path: path.to_string(),
            message: e.to_string(),
        })?
    };

    if !overlay.is_supported_version() {
        return Err(OverlayError::UnsupportedVersion {
            path: path.to_string(),
            found: overlay.schema_version.clone(),
            expected: OVERLAY_SCHEMA_VERSION.to_string(),
        });
    }

    // Content invariants are enforced here rather than only in `overlay verify`
    // so a `verified` claim can never reach a scan without provenance backing
    // it: an authoritative overlay replaces the scan result wholesale, and an
    // unbacked one would launder a guess into a fact an agent then acts on.
    let defects = validate_overlay(&overlay);
    if !defects.is_empty() {
        return Err(OverlayError::Invalid {
            path: path.to_string(),
            defects,
        });
    }
    Ok(overlay)
}

/// Default directory an install keeps hand-written overlays in.
pub fn user_overlay_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("overlays")
}

/// Well-known locations a packaged corpus may be installed to.
///
/// apexe ships no overlays of its own: the corpus lives in
/// [cli-permissions](https://github.com/aiperceivable/cli-permissions) and is
/// installed separately. These paths exist so that installing it through a
/// package manager is enough — without them every user would have to discover
/// `overlay_dirs` and write a config file before a package they already
/// installed did anything.
///
/// Searched in order and all of them read, before `<config_dir>/overlays` and
/// before anything in `overlay_dirs`, so a locally-installed or
/// operator-configured corpus outranks a system one. A directory that is not
/// there is silently skipped: absence is the normal case, unlike a path an
/// operator typed.
pub fn packaged_overlay_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data).join("cli-permissions/overlays"));
    } else if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/cli-permissions/overlays"));
    }
    dirs.push(PathBuf::from("/usr/local/share/cli-permissions/overlays"));
    dirs.push(PathBuf::from("/usr/share/cli-permissions/overlays"));
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from(
            "/opt/homebrew/share/cli-permissions/overlays",
        ));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::overlay::{Platform, ProbeOutcome};
    use crate::models::ToolVariant;
    use tempfile::TempDir;

    const MINIMAL: &str = r#"{
      "schema_version": "1.0",
      "command": "widget",
      "variant": "bsd",
      "match": { "platform": ["macos"] },
      "mode": "merge",
      "confidence": "verified",
      "provenance": {
        "platform": "macos",
        "tool_version": "test",
        "source": "man-page",
        "checked_on": "2026-07-27"
      },
      "flags": []
    }"#;

    /// The corpus these tests read, or `None` when this checkout has none.
    ///
    /// apexe ships no overlays: the corpus is a separate repository, so a test
    /// that needs real entries needs it on disk. `APEXE_TEST_CORPUS` names it —
    /// CI sets that — and otherwise a sibling `cli-permissions` checkout is
    /// found, which is the ordinary local layout.
    ///
    /// **Set but missing panics.** A test that silently skips forever is worse
    /// than one that fails: it reports green while covering nothing, and these
    /// tests exist because overlays carry facts no scan can recover. Unset and
    /// absent is a plain skip — a developer without the corpus, not a broken
    /// pipeline.
    fn corpus_dir() -> Option<PathBuf> {
        if let Some(configured) = std::env::var_os("APEXE_TEST_CORPUS") {
            let path = PathBuf::from(configured);
            assert!(
                path.is_dir(),
                "APEXE_TEST_CORPUS points at {}, which is not a directory",
                path.display()
            );
            return Some(path);
        }
        let sibling = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .join("cli-permissions/overlays");
        sibling.is_dir().then_some(sibling)
    }

    fn corpus_store() -> Option<OverlayStore> {
        let dir = corpus_dir()?;
        let mut store = OverlayStore::empty();
        store
            .load_dir(&dir)
            .unwrap_or_else(|e| panic!("corpus at {} is unreadable: {e}", dir.display()));
        Some(store)
    }

    fn context(command: &str, variant: ToolVariant, platform: Platform) -> MatchContext {
        MatchContext {
            command: command.to_string(),
            variant,
            platform: Some(platform),
            binary_path: format!("/bin/{command}"),
            ..Default::default()
        }
    }

    /// An Apple-variant overlay must be *expressible*, not merely nameable:
    /// before `ToolVariant::Apple` existed, macOS `sort` classified `unknown`,
    /// so an overlay for it could never match on anything but a path. No such
    /// overlay ships — this asserts the mechanism, not a curated file.
    #[test]
    fn test_apple_variant_overlay_is_selectable() {
        let document = MINIMAL
            .replace("\"widget\"", "\"sort\"")
            .replace("\"variant\": \"bsd\"", "\"variant\": \"apple\"");
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("sort.json"), document).unwrap();

        let mut store = OverlayStore::empty();
        store.load_dir(tmp.path()).unwrap();

        let ctx = context("sort", ToolVariant::Apple, Platform::new("macos"));
        let selected = store.select(&ctx).expect("apple overlay must match");
        assert_eq!(selected.overlay.id(), "sort@apple");
    }

    /// Every entry in the corpus parses.
    ///
    /// `load_dir` reports a malformed file rather than skipping it, so this
    /// fails loudly if one is broken. There is no shipped count to compare
    /// against any more — apexe carries no overlays of its own — so the
    /// assertion is that the corpus is non-empty and every file in it loaded.
    #[test]
    fn test_corpus_overlays_all_parse() {
        let Some(dir) = corpus_dir() else { return };
        let files = std::fs::read_dir(&dir)
            .expect("corpus directory must be readable")
            .filter_map(Result::ok)
            .filter(|e| has_overlay_extension(&e.path()))
            .count();
        let store = corpus_store().expect("corpus_dir returned Some");
        assert!(
            !store.is_empty(),
            "the corpus at {} is empty",
            dir.display()
        );
        assert_eq!(store.len(), files, "every overlay file must have loaded");
    }

    #[test]
    fn test_builtin_ls_bsd_selected_for_bsd_probe() {
        let Some(store) = corpus_store() else { return };
        let mut ctx = context("ls", ToolVariant::Bsd, Platform::new("macos"));
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: false,
            output: "ls: unrecognized option `--version'".to_string(),
        }];
        let selected = store.select(&ctx).expect("bsd overlay must match");
        assert_eq!(selected.overlay.id(), "ls@bsd");
        assert_eq!(selected.strength, MatchStrength::Probe);
    }

    #[test]
    fn test_builtin_ls_gnu_selected_on_macos_when_probe_says_gnu() {
        // Homebrew coreutils on macOS: path heuristics would say BSD; the probe
        // is the only thing that gets this right.
        let Some(store) = corpus_store() else { return };
        let mut ctx = context("ls", ToolVariant::Gnu, Platform::new("macos"));
        ctx.binary_path = "/opt/homebrew/opt/coreutils/libexec/gnubin/ls".to_string();
        ctx.version = Some("9.4".to_string());
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: true,
            output: "ls (GNU coreutils) 9.4".to_string(),
        }];
        let selected = store.select(&ctx).expect("gnu overlay must match");
        assert_eq!(selected.overlay.id(), "ls@gnu");
    }

    #[test]
    fn test_builtin_rm_bsd_selected_for_bsd_probe() {
        let Some(store) = corpus_store() else { return };
        let mut ctx = context("rm", ToolVariant::Bsd, Platform::new("macos"));
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: false,
            output: "rm: illegal option -- -".to_string(),
        }];
        let selected = store.select(&ctx).expect("bsd rm overlay must match");
        assert_eq!(selected.overlay.id(), "rm@bsd");
        assert_eq!(selected.strength, MatchStrength::Probe);
    }

    #[test]
    fn test_builtin_rm_gnu_selected_when_probe_says_gnu() {
        let Some(store) = corpus_store() else { return };
        let mut ctx = context("rm", ToolVariant::Gnu, Platform::new("linux"));
        ctx.binary_path = "/usr/bin/rm".to_string();
        ctx.version = Some("9.7".to_string());
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: true,
            output: "rm (GNU coreutils) 9.7".to_string(),
        }];
        let selected = store.select(&ctx).expect("gnu rm overlay must match");
        assert_eq!(selected.overlay.id(), "rm@gnu");
    }

    /// An under-classified `rm` would let an agent delete files without ever
    /// reaching the approval gate, so both variants are pinned here.
    #[test]
    fn test_builtin_rm_overlays_assert_destructive_and_require_approval() {
        let Some(store) = corpus_store() else { return };
        let rm_overlays: Vec<_> = store
            .entries
            .iter()
            .filter(|entry| entry.overlay.command == "rm")
            .collect();
        assert_eq!(rm_overlays.len(), 2, "both rm variants must be registered");
        for entry in rm_overlays {
            let annotations = &entry.overlay.annotations;
            assert_eq!(annotations.destructive, Some(true));
            assert_eq!(annotations.requires_approval, Some(true));
            assert_eq!(annotations.readonly, Some(false));
        }
    }

    /// Every curated built-in claims `verified`, which the loader only accepts
    /// with provenance behind it. This asserts the intent rather than relying
    /// on a silent skip if someone strips a provenance block.
    #[test]
    fn test_builtin_overlays_are_verified_with_provenance() {
        let Some(store) = corpus_store() else { return };
        for entry in &store.entries {
            assert_eq!(
                entry.overlay.confidence,
                crate::models::Confidence::Verified,
                "{} must be verified",
                entry.overlay.id()
            );
            let provenance = entry
                .overlay
                .provenance
                .as_ref()
                .unwrap_or_else(|| panic!("{} must carry provenance", entry.overlay.id()));
            assert!(!provenance.tool_version.trim().is_empty());
            assert_eq!(provenance.checked_on.len(), 10);
        }
    }

    /// `ToolVariant::Gnu` is the whole GNU family, so the variant alone says
    /// nothing about the package. Package identity has to be enforced where it
    /// actually bites — the probe — or a GNU `tar` could pick up a coreutils
    /// overlay. `provenance.package` records the same fact for an auditor, and
    /// the two must agree, which is what this asserts: the probe string is
    /// `GNU <package>` for whichever package the provenance names. It is
    /// deliberately not a fixed count or a fixed string, because coreutils,
    /// diffutils, grep and findutils are all `gnu` and all curated here.
    #[test]
    fn test_builtin_gnu_overlays_pin_their_own_package() {
        let Some(store) = corpus_store() else { return };
        let gnu: Vec<_> = store
            .entries
            .iter()
            .filter(|entry| entry.overlay.variant == ToolVariant::Gnu)
            .collect();
        assert!(
            gnu.len() >= 14,
            "the curated GNU overlays must all be registered, found {}",
            gnu.len()
        );
        for entry in gnu {
            let provenance = entry
                .overlay
                .provenance
                .as_ref()
                .unwrap_or_else(|| panic!("{} must carry provenance", entry.overlay.id()));
            let package = provenance
                .package
                .as_deref()
                .unwrap_or_else(|| panic!("{} must name its upstream package", entry.overlay.id()));
            let probe = entry
                .overlay
                .match_rules
                .probe
                .as_ref()
                .unwrap_or_else(|| panic!("{} must declare a probe", entry.overlay.id()));
            assert_eq!(
                probe.output_contains.as_deref(),
                Some(format!("GNU {package}").as_str()),
                "{} must pin its package in the probe",
                entry.overlay.id()
            );
        }
    }

    /// The `gnu` variant covers the whole GNU family, so a non-coreutils
    /// package has to reach its own overlay. These three are the first ones
    /// that are not coreutils, and each is separated from the others only by
    /// `probe.output_contains`.
    #[test]
    fn test_builtin_non_coreutils_gnu_overlays_are_selected_by_their_banner() {
        let Some(store) = corpus_store() else { return };
        for (command, banner, expected) in [
            ("grep", "grep (GNU grep) 3.11", "grep@gnu"),
            ("find", "find (GNU findutils) 4.10.0", "find@gnu"),
            ("xargs", "xargs (GNU findutils) 4.10.0", "xargs@gnu"),
            ("diff", "diff (GNU diffutils) 3.10", "diff@gnu"),
        ] {
            let mut ctx = context(command, ToolVariant::Gnu, Platform::new("linux"));
            ctx.binary_path = format!("/usr/bin/{command}");
            ctx.probes = vec![ProbeOutcome {
                args: super::super::variant::version_probe_args(),
                succeeded: true,
                output: banner.to_string(),
            }];
            let selected = store
                .select(&ctx)
                .unwrap_or_else(|| panic!("{expected} must match"));
            assert_eq!(selected.overlay.id(), expected);

            // A coreutils banner must not satisfy a diffutils/grep/findutils
            // probe, which is the whole reason the package is pinned there.
            ctx.probes = vec![ProbeOutcome {
                args: super::super::variant::version_probe_args(),
                succeeded: true,
                output: format!("{command} (GNU coreutils) 9.7"),
            }];
            assert!(
                store.select(&ctx).is_none(),
                "{expected} must not match a coreutils banner"
            );
        }
    }

    /// The `apple` variant exists so that Apple's own ports, whose banner names
    /// Apple rather than BSD, can carry an overlay at all. `sort` is the first
    /// one, and its probe must key on the banner: Homebrew coreutils can put a
    /// GNU `sort` at the same path, where every platform and path signal still
    /// says macOS.
    #[test]
    fn test_builtin_sort_apple_selected_only_on_the_apple_banner() {
        let Some(store) = corpus_store() else { return };
        let mut ctx = context("sort", ToolVariant::Apple, Platform::new("macos"));
        ctx.binary_path = "/usr/bin/sort".to_string();
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: true,
            output: "2.3-Apple (197)".to_string(),
        }];
        let selected = store.select(&ctx).expect("apple overlay must match");
        assert_eq!(selected.overlay.id(), "sort@apple");
        assert_eq!(selected.strength, MatchStrength::Probe);

        // A GNU sort installed at the same path must not pick it up.
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: true,
            output: "sort (GNU coreutils) 9.7".to_string(),
        }];
        assert!(store.select(&ctx).is_none());
    }

    /// Prose in a description is not actionable; the boolean has to survive
    /// the trip from the shipped overlay into the scanned flag.
    #[test]
    fn test_builtin_tail_overlays_mark_follow_long_running() {
        let Some(store) = corpus_store() else { return };
        let tails: Vec<_> = store
            .entries
            .iter()
            .filter(|entry| entry.overlay.command == "tail")
            .collect();
        assert_eq!(tails.len(), 2, "both tail variants must be registered");
        for entry in tails {
            let marked: Vec<&str> = entry
                .overlay
                .flags
                .iter()
                .filter(|flag| flag.long_running)
                .filter_map(|flag| flag.short.as_deref())
                .collect();
            assert_eq!(
                marked,
                vec!["-f", "-F"],
                "{}: only the following flags may claim it",
                entry.overlay.id()
            );
        }
    }

    #[test]
    fn test_select_returns_none_for_unknown_command() {
        let Some(store) = corpus_store() else { return };
        let ctx = context(
            "definitely-not-a-real-tool",
            ToolVariant::Bsd,
            Platform::new("macos"),
        );
        assert!(store.select(&ctx).is_none());
    }

    #[test]
    fn test_probe_arg_sets_always_includes_version() {
        let Some(store) = corpus_store() else { return };
        let sets = store.probe_arg_sets("ls");
        assert!(sets.contains(&vec!["--version".to_string()]));
    }

    #[test]
    fn test_probe_arg_sets_deduplicates() {
        // Both built-in ls overlays probe `--version`; it must appear once.
        let Some(store) = corpus_store() else { return };
        let sets = store.probe_arg_sets("ls");
        let version_count = sets
            .iter()
            .filter(|args| *args == &vec!["--version".to_string()])
            .count();
        assert_eq!(version_count, 1);
    }

    #[test]
    fn test_load_dir_missing_directory_is_not_an_error() {
        let mut store = OverlayStore::empty();
        let loaded = store
            .load_dir(Path::new("/nonexistent/overlay/dir"))
            .unwrap();
        assert_eq!(loaded, 0);
    }

    #[test]
    fn test_load_dir_reads_json_overlay() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("widget.json"), MINIMAL).unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "ignored").unwrap();

        let mut store = OverlayStore::empty();
        let loaded = store.load_dir(tmp.path()).unwrap();
        assert_eq!(loaded, 1);
        assert!(store
            .select(&context("widget", ToolVariant::Bsd, Platform::new("macos")))
            .is_some());
    }

    #[test]
    fn test_load_dir_reads_yaml_overlay() {
        let tmp = TempDir::new().unwrap();
        let yaml = "schema_version: '1.0'\ncommand: widget\nvariant: bsd\nmode: merge\n";
        std::fs::write(tmp.path().join("widget.yaml"), yaml).unwrap();

        let mut store = OverlayStore::empty();
        assert_eq!(store.load_dir(tmp.path()).unwrap(), 1);
    }

    #[test]
    fn test_load_dir_surfaces_malformed_overlay() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bad.json"), "{ not json").unwrap();

        let mut store = OverlayStore::empty();
        let err = store.load_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, OverlayError::Malformed { .. }));
    }

    #[test]
    fn test_load_rejects_unsupported_schema_version() {
        let tmp = TempDir::new().unwrap();
        let document = MINIMAL.replace("\"1.0\"", "\"99.0\"");
        std::fs::write(tmp.path().join("widget.json"), document).unwrap();

        let mut store = OverlayStore::empty();
        let err = store.load_dir(tmp.path()).unwrap_err();
        match err {
            OverlayError::UnsupportedVersion {
                found, expected, ..
            } => {
                assert_eq!(found, "99.0");
                assert_eq!(expected, OVERLAY_SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn test_load_explicit_missing_file_errors() {
        let mut store = OverlayStore::empty();
        let err = store
            .load_explicit(Path::new("/nonexistent/overlay.json"))
            .unwrap_err();
        assert!(matches!(err, OverlayError::Read { .. }));
    }

    #[test]
    fn test_explicit_overlay_outranks_builtin_and_skips_variant_match() {
        // The operator named the file, so a variant the probe could not confirm
        // must not stop it from applying.
        let tmp = TempDir::new().unwrap();
        let document = MINIMAL.replace("\"widget\"", "\"ls\"");
        let path = tmp.path().join("mine.json");
        std::fs::write(&path, document).unwrap();

        let Some(mut store) = corpus_store() else {
            return;
        };
        store.load_explicit(&path).unwrap();

        let ctx = context("ls", ToolVariant::Unknown, Platform::new("linux"));
        let selected = store.select(&ctx).expect("explicit overlay must apply");
        assert_eq!(selected.strength, MatchStrength::Explicit);
    }

    #[test]
    fn test_explicit_overlay_still_requires_matching_command() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mine.json");
        std::fs::write(&path, MINIMAL).unwrap();

        let mut store = OverlayStore::empty();
        store.load_explicit(&path).unwrap();

        assert!(store
            .select(&context("cat", ToolVariant::Bsd, Platform::new("macos")))
            .is_none());
    }

    #[test]
    fn test_user_overlay_shadows_builtin_at_equal_strength() {
        let tmp = TempDir::new().unwrap();
        let document = MINIMAL
            .replace("\"widget\"", "\"ls\"")
            .replace("{ \"platform\": [\"macos\"] }", "{}");
        std::fs::write(tmp.path().join("ls.json"), document).unwrap();

        let Some(mut store) = corpus_store() else {
            return;
        };
        store.load_dir(tmp.path()).unwrap();

        // No probe outcomes recorded, so the built-in (probe-gated) overlays do
        // not match and only the user's platform-free overlay survives.
        let ctx = context("ls", ToolVariant::Bsd, Platform::new("macos"));
        let selected = store.select(&ctx).unwrap();
        assert_eq!(selected.strength, MatchStrength::Platform);
    }

    /// Two directories can describe the same (command, variant), and which one
    /// wins is decided by load order alone -- `select` keeps the last entry
    /// among equally-ranked ones. That is an implicit property of `max_by_key`
    /// rather than something the code says out loud, so it is pinned here:
    /// `ScanOrchestrator` relies on it to make the operator's own
    /// `<config_dir>/overlays` shadow a corpus listed in `overlay_dirs`, and a
    /// refactor to `min_by_key`, a sort, or a `HashMap` would silently invert
    /// that without failing anything else.
    #[test]
    fn test_last_loaded_dir_wins_among_equally_ranked_overlays() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let doc = |desc: &str| {
            MINIMAL.replace(
                "\"flags\": []",
                &format!("\"description\": \"{desc}\", \"flags\": []"),
            )
        };
        std::fs::write(first.path().join("widget.json"), doc("from-first")).unwrap();
        std::fs::write(second.path().join("widget.json"), doc("from-second")).unwrap();

        let mut store = OverlayStore::empty();
        store.load_dir(first.path()).unwrap();
        store.load_dir(second.path()).unwrap();

        let ctx = context("widget", ToolVariant::Bsd, Platform::new("macos"));
        let selected = store.select(&ctx).expect("one of the two must match");
        assert_eq!(
            selected.overlay.description, "from-second",
            "the directory loaded last must win, or overlay_dirs precedence is inverted"
        );
    }

    #[test]
    fn test_user_overlay_dir_is_under_config_dir() {
        let dir = user_overlay_dir(Path::new("/home/me/.apexe"));
        assert_eq!(dir, PathBuf::from("/home/me/.apexe/overlays"));
    }
}
