//! Tool variant detection.
//!
//! A command name does not identify a program. `ls` is BSD on macOS and
//! FreeBSD, GNU coreutils on most Linux distributions, BusyBox on Alpine — and
//! Homebrew happily installs GNU coreutils onto a macOS box, so the binary's
//! *path* is not a reliable discriminator either. The only deterministic signal
//! is asking the binary itself:
//!
//! ```text
//! ls --version -> "ls (GNU coreutils) 9.4"      => gnu-coreutils
//!              -> "ls: unrecognized option ..." => bsd (with platform corroboration)
//!              -> "BusyBox v1.36.1"             => busybox
//! ```
//!
//! Rejecting `--version` is not the only BSD tell, and treating it as one
//! under-reports: macOS `grep` answers the probe successfully with
//! `grep (BSD grep, GNU compatible) 2.6.0-FreeBSD`. A self-declared banner is
//! stronger evidence than the rejection heuristic, so it is matched directly.
//!
//! This module owns the subprocess execution; the classification rule itself is
//! a pure function so it can be tested without a filesystem.

use std::time::Duration;

use crate::adapter::overlay::{Platform, ProbeOutcome};
use crate::models::ToolVariant;

/// The probe every scan runs, and the one overlays reference by default.
pub const VERSION_PROBE_ARGS: &[&str] = &["--version"];

/// Substrings a BSD userland emits when handed a GNU-style long option.
const BSD_REJECTION_MARKERS: &[&str] = &["unrecognized option", "illegal option", "invalid option"];

/// Words that, appearing as a token in a *successful* `--version` banner,
/// identify a BSD userland outright.
const BSD_BANNER_TOKENS: &[&str] = &["bsd", "freebsd", "openbsd", "netbsd"];

/// The host platform this build is running on.
///
/// Returns `None` for targets with no overlay vocabulary, so a match rule that
/// names platforms simply fails rather than matching something arbitrary.
pub fn current_platform() -> Option<Platform> {
    match std::env::consts::OS {
        "macos" => Some(Platform::Macos),
        "linux" => Some(Platform::Linux),
        "freebsd" => Some(Platform::Freebsd),
        "windows" => Some(Platform::Windows),
        _ => Some(Platform::Other),
    }
}

/// Run each argument set against `binary_path`, recording what happened.
///
/// A probe that cannot be spawned at all (missing binary, timeout) is recorded
/// as a failed outcome with empty output rather than dropped, so an overlay
/// expecting `failure` is not accidentally satisfied by a probe that never ran:
/// [`crate::adapter::overlay`] additionally requires any declared
/// `output_contains` to be present.
pub fn run_probes(
    binary_path: &str,
    arg_sets: &[Vec<String>],
    timeout: Duration,
) -> Vec<ProbeOutcome> {
    arg_sets
        .iter()
        .map(|args| run_probe(binary_path, args, timeout))
        .collect()
}

fn run_probe(binary_path: &str, args: &[String], timeout: Duration) -> ProbeOutcome {
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    match super::exec::run_with_timeout(binary_path, &borrowed, timeout) {
        Ok(output) => {
            // Variant banners land on either stream: GNU writes its version to
            // stdout, BSD writes its rejection to stderr.
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            ProbeOutcome {
                args: args.to_vec(),
                succeeded: output.status.success(),
                output: combined,
            }
        }
        Err(_) => ProbeOutcome {
            args: args.to_vec(),
            succeeded: false,
            output: String::new(),
        },
    }
}

/// Classify a binary from its `--version` probe outcome.
///
/// A BSD verdict requires platform corroboration: "the binary rejected
/// `--version`" is on its own only evidence that the tool is not GNU, and
/// plenty of non-BSD tools reject it too.
pub fn classify_variant(outcome: Option<&ProbeOutcome>, platform: Option<Platform>) -> ToolVariant {
    let Some(outcome) = outcome else {
        return ToolVariant::Unknown;
    };
    let output = outcome.output.to_lowercase();

    if output.contains("busybox") {
        return ToolVariant::Busybox;
    }
    if outcome.succeeded && output.contains("gnu coreutils") {
        return ToolVariant::GnuCoreutils;
    }
    // Not every BSD tool rejects `--version`. macOS `grep` answers it with
    // "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD" — a self-declared banner,
    // which is stronger evidence than the rejection heuristic and needs no
    // platform corroboration. Checked after the GNU test so a banner reading
    // "BSD grep, GNU compatible" is not mistaken for coreutils.
    if outcome.succeeded && mentions_bsd(&output) {
        return ToolVariant::Bsd;
    }
    let rejected = !outcome.succeeded
        && BSD_REJECTION_MARKERS
            .iter()
            .any(|marker| output.contains(marker));
    if rejected && matches!(platform, Some(Platform::Macos) | Some(Platform::Freebsd)) {
        return ToolVariant::Bsd;
    }
    ToolVariant::Unknown
}

/// Whether a lowercased banner names a BSD userland as a whole word.
///
/// Token matching rather than a substring test, so a version string that merely
/// embeds the letters (a build path, say) does not claim the whole tool.
fn mentions_bsd(output: &str) -> bool {
    output
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| BSD_BANNER_TOKENS.contains(&token))
}

/// Locate the `--version` outcome among a probe result set.
pub fn version_outcome(outcomes: &[ProbeOutcome]) -> Option<&ProbeOutcome> {
    outcomes.iter().find(|outcome| {
        outcome.args.len() == VERSION_PROBE_ARGS.len()
            && outcome
                .args
                .iter()
                .zip(VERSION_PROBE_ARGS)
                .all(|(actual, expected)| actual == expected)
    })
}

/// The default probe argument set as owned strings.
pub fn version_probe_args() -> Vec<String> {
    VERSION_PROBE_ARGS
        .iter()
        .map(|a| (*a).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(succeeded: bool, output: &str) -> ProbeOutcome {
        ProbeOutcome {
            args: version_probe_args(),
            succeeded,
            output: output.to_string(),
        }
    }

    #[test]
    fn test_classify_variant_gnu_coreutils_banner() {
        let probe = outcome(true, "ls (GNU coreutils) 9.4\n");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Linux)),
            ToolVariant::GnuCoreutils
        );
    }

    #[test]
    fn test_classify_variant_gnu_coreutils_on_macos() {
        // Homebrew coreutils on a macOS box: the platform says BSD, the probe
        // says GNU, and the probe must win.
        let probe = outcome(true, "ls (GNU coreutils) 9.5\n");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Macos)),
            ToolVariant::GnuCoreutils
        );
    }

    #[test]
    fn test_classify_variant_busybox_banner() {
        let probe = outcome(true, "BusyBox v1.36.1 (2023-11-07) multi-call binary.");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Linux)),
            ToolVariant::Busybox
        );
    }

    #[test]
    fn test_classify_variant_bsd_needs_platform_corroboration() {
        // Verbatim macOS output.
        let probe = outcome(
            false,
            "ls: unrecognized option `--version'\nusage: ls [-@ABC...]",
        );
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Macos)),
            ToolVariant::Bsd
        );
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Linux)),
            ToolVariant::Unknown,
            "rejecting --version is not by itself evidence of BSD"
        );
    }

    #[test]
    fn test_classify_variant_illegal_option_wording() {
        let probe = outcome(false, "ls: illegal option -- -");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Freebsd)),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_classify_variant_unknown_without_probe() {
        assert_eq!(
            classify_variant(None, Some(Platform::Macos)),
            ToolVariant::Unknown
        );
    }

    #[test]
    fn test_classify_variant_bsd_from_successful_banner() {
        // Verbatim macOS output: not every BSD tool rejects `--version`, so the
        // rejection heuristic alone would have called this "unknown".
        let probe = outcome(true, "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Macos)),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_classify_variant_bsd_banner_beaten_by_gnu_coreutils() {
        // "BSD grep, GNU compatible" must not read as coreutils, and a genuine
        // coreutils banner must not be derailed by an incidental "bsd".
        let probe = outcome(true, "ls (GNU coreutils) 9.4 built on freebsd");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Freebsd)),
            ToolVariant::GnuCoreutils
        );
    }

    #[test]
    fn test_mentions_bsd_matches_whole_tokens_only() {
        assert!(mentions_bsd(
            "grep (bsd grep, gnu compatible) 2.6.0-freebsd"
        ));
        assert!(mentions_bsd("2.6.0-freebsd"));
        assert!(
            !mentions_bsd("tool 1.0 built from /opt/bsdish/src"),
            "an embedded substring must not claim the whole tool"
        );
    }

    #[test]
    fn test_classify_variant_unknown_for_ordinary_tool() {
        let probe = outcome(true, "git version 2.43.0");
        assert_eq!(
            classify_variant(Some(&probe), Some(Platform::Macos)),
            ToolVariant::Unknown
        );
    }

    #[test]
    fn test_run_probes_records_failure_for_missing_binary() {
        let probes = run_probes(
            "zzz_no_such_binary_xyz",
            &[version_probe_args()],
            Duration::from_secs(2),
        );
        assert_eq!(probes.len(), 1);
        assert!(!probes[0].succeeded);
        assert!(probes[0].output.is_empty());
    }

    #[test]
    fn test_run_probes_captures_output_of_real_binary() {
        let probes = run_probes("echo", &[vec!["hello".to_string()]], Duration::from_secs(5));
        assert!(probes[0].succeeded);
        assert!(probes[0].output.contains("hello"));
    }

    #[test]
    fn test_version_outcome_finds_the_version_probe() {
        let outcomes = vec![
            ProbeOutcome {
                args: vec!["-V".to_string()],
                succeeded: true,
                output: "x".to_string(),
            },
            outcome(false, "nope"),
        ];
        assert!(version_outcome(&outcomes).is_some());
        assert_eq!(version_outcome(&outcomes).unwrap().output, "nope");
    }

    #[test]
    fn test_current_platform_is_resolvable() {
        assert!(current_platform().is_some());
    }
}
