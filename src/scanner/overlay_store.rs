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
use tracing::{debug, warn};

use crate::adapter::overlay::{
    validate_overlay, MatchContext, MatchStrength, OverlayDefect, ToolOverlay,
    OVERLAY_SCHEMA_VERSION,
};

/// Curated overlays compiled into the binary.
///
/// Deliberately a short list: it exists to prove the mechanism end to end, not
/// to be a library of every CLI in existence.
const BUILTIN_OVERLAYS: &[(&str, &str)] = &[
    ("cat@bsd", include_str!("../../overlays/cat@bsd.json")),
    (
        "cat@gnu-coreutils",
        include_str!("../../overlays/cat@gnu-coreutils.json"),
    ),
    ("cp@bsd", include_str!("../../overlays/cp@bsd.json")),
    (
        "cp@gnu-coreutils",
        include_str!("../../overlays/cp@gnu-coreutils.json"),
    ),
    ("cut@bsd", include_str!("../../overlays/cut@bsd.json")),
    (
        "cut@gnu-coreutils",
        include_str!("../../overlays/cut@gnu-coreutils.json"),
    ),
    ("diff@bsd", include_str!("../../overlays/diff@bsd.json")),
    ("head@bsd", include_str!("../../overlays/head@bsd.json")),
    (
        "head@gnu-coreutils",
        include_str!("../../overlays/head@gnu-coreutils.json"),
    ),
    ("ln@bsd", include_str!("../../overlays/ln@bsd.json")),
    (
        "ln@gnu-coreutils",
        include_str!("../../overlays/ln@gnu-coreutils.json"),
    ),
    ("ls@bsd", include_str!("../../overlays/ls@bsd.json")),
    (
        "ls@gnu-coreutils",
        include_str!("../../overlays/ls@gnu-coreutils.json"),
    ),
    ("mkdir@bsd", include_str!("../../overlays/mkdir@bsd.json")),
    (
        "mkdir@gnu-coreutils",
        include_str!("../../overlays/mkdir@gnu-coreutils.json"),
    ),
    ("mv@bsd", include_str!("../../overlays/mv@bsd.json")),
    (
        "mv@gnu-coreutils",
        include_str!("../../overlays/mv@gnu-coreutils.json"),
    ),
    ("rm@bsd", include_str!("../../overlays/rm@bsd.json")),
    (
        "rm@gnu-coreutils",
        include_str!("../../overlays/rm@gnu-coreutils.json"),
    ),
    (
        "sort@gnu-coreutils",
        include_str!("../../overlays/sort@gnu-coreutils.json"),
    ),
    ("tail@bsd", include_str!("../../overlays/tail@bsd.json")),
    (
        "tail@gnu-coreutils",
        include_str!("../../overlays/tail@gnu-coreutils.json"),
    ),
    ("touch@bsd", include_str!("../../overlays/touch@bsd.json")),
    (
        "touch@gnu-coreutils",
        include_str!("../../overlays/touch@gnu-coreutils.json"),
    ),
    ("uniq@bsd", include_str!("../../overlays/uniq@bsd.json")),
    (
        "uniq@gnu-coreutils",
        include_str!("../../overlays/uniq@gnu-coreutils.json"),
    ),
    ("wc@bsd", include_str!("../../overlays/wc@bsd.json")),
    (
        "wc@gnu-coreutils",
        include_str!("../../overlays/wc@gnu-coreutils.json"),
    ),
];

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

    /// Load the curated built-in overlays.
    ///
    /// A malformed built-in is a build defect, not a user problem, so it is
    /// logged and skipped rather than failing every scan.
    pub fn with_builtins() -> Self {
        let mut store = Self::default();
        for (id, document) in BUILTIN_OVERLAYS {
            match parse_overlay(id, document) {
                Ok(overlay) => store.entries.push(StoredOverlay {
                    overlay,
                    origin: OverlayOrigin::Builtin,
                }),
                Err(e) => warn!(overlay = id, "Built-in overlay is invalid, skipping: {e}"),
            }
        }
        store
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

    fn context(command: &str, variant: ToolVariant, platform: Platform) -> MatchContext {
        MatchContext {
            command: command.to_string(),
            variant,
            platform: Some(platform),
            binary_path: format!("/bin/{command}"),
            ..Default::default()
        }
    }

    #[test]
    fn test_builtin_overlays_all_parse() {
        // A malformed built-in would be silently skipped at runtime, so assert
        // the shipped count here instead.
        let store = OverlayStore::with_builtins();
        assert_eq!(store.len(), BUILTIN_OVERLAYS.len());
        assert!(!store.is_empty());
    }

    #[test]
    fn test_builtin_ls_bsd_selected_for_bsd_probe() {
        let store = OverlayStore::with_builtins();
        let mut ctx = context("ls", ToolVariant::Bsd, Platform::Macos);
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
        let store = OverlayStore::with_builtins();
        let mut ctx = context("ls", ToolVariant::GnuCoreutils, Platform::Macos);
        ctx.binary_path = "/opt/homebrew/opt/coreutils/libexec/gnubin/ls".to_string();
        ctx.version = Some("9.4".to_string());
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: true,
            output: "ls (GNU coreutils) 9.4".to_string(),
        }];
        let selected = store.select(&ctx).expect("gnu overlay must match");
        assert_eq!(selected.overlay.id(), "ls@gnu-coreutils");
    }

    #[test]
    fn test_builtin_rm_bsd_selected_for_bsd_probe() {
        let store = OverlayStore::with_builtins();
        let mut ctx = context("rm", ToolVariant::Bsd, Platform::Macos);
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
        let store = OverlayStore::with_builtins();
        let mut ctx = context("rm", ToolVariant::GnuCoreutils, Platform::Linux);
        ctx.binary_path = "/usr/bin/rm".to_string();
        ctx.version = Some("9.7".to_string());
        ctx.probes = vec![ProbeOutcome {
            args: super::super::variant::version_probe_args(),
            succeeded: true,
            output: "rm (GNU coreutils) 9.7".to_string(),
        }];
        let selected = store.select(&ctx).expect("gnu rm overlay must match");
        assert_eq!(selected.overlay.id(), "rm@gnu-coreutils");
    }

    /// An under-classified `rm` would let an agent delete files without ever
    /// reaching the approval gate, so both variants are pinned here.
    #[test]
    fn test_builtin_rm_overlays_assert_destructive_and_require_approval() {
        let store = OverlayStore::with_builtins();
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
        let store = OverlayStore::with_builtins();
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

    #[test]
    fn test_select_returns_none_for_unknown_command() {
        let store = OverlayStore::with_builtins();
        let ctx = context(
            "definitely-not-a-real-tool",
            ToolVariant::Bsd,
            Platform::Macos,
        );
        assert!(store.select(&ctx).is_none());
    }

    #[test]
    fn test_probe_arg_sets_always_includes_version() {
        let store = OverlayStore::with_builtins();
        let sets = store.probe_arg_sets("ls");
        assert!(sets.contains(&vec!["--version".to_string()]));
    }

    #[test]
    fn test_probe_arg_sets_deduplicates() {
        // Both built-in ls overlays probe `--version`; it must appear once.
        let store = OverlayStore::with_builtins();
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
            .select(&context("widget", ToolVariant::Bsd, Platform::Macos))
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

        let mut store = OverlayStore::with_builtins();
        store.load_explicit(&path).unwrap();

        let ctx = context("ls", ToolVariant::Unknown, Platform::Linux);
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
            .select(&context("cat", ToolVariant::Bsd, Platform::Macos))
            .is_none());
    }

    #[test]
    fn test_user_overlay_shadows_builtin_at_equal_strength() {
        let tmp = TempDir::new().unwrap();
        let document = MINIMAL
            .replace("\"widget\"", "\"ls\"")
            .replace("{ \"platform\": [\"macos\"] }", "{}");
        std::fs::write(tmp.path().join("ls.json"), document).unwrap();

        let mut store = OverlayStore::with_builtins();
        store.load_dir(tmp.path()).unwrap();

        // No probe outcomes recorded, so the built-in (probe-gated) overlays do
        // not match and only the user's platform-free overlay survives.
        let ctx = context("ls", ToolVariant::Bsd, Platform::Macos);
        let selected = store.select(&ctx).unwrap();
        assert_eq!(selected.strength, MatchStrength::Platform);
    }

    #[test]
    fn test_user_overlay_dir_is_under_config_dir() {
        let dir = user_overlay_dir(Path::new("/home/me/.apexe"));
        assert_eq!(dir, PathBuf::from("/home/me/.apexe/overlays"));
    }
}
