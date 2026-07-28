//! Tool variant detection.
//!
//! A command name does not identify a program. `ls` is BSD on macOS and
//! FreeBSD, GNU coreutils on most Linux distributions, BusyBox on Alpine — and
//! Homebrew happily installs GNU coreutils onto a macOS box, so the binary's
//! *path* is not a reliable discriminator either. The only deterministic signal
//! is asking the binary itself:
//!
//! ```text
//! ls --version -> "ls (GNU coreutils) 9.4"      => gnu
//!              -> "ls: unrecognized option ..." => bsd (with platform corroboration)
//!              -> "BusyBox v1.36.1"             => busybox
//! sort --version -> "2.3-Apple (197)"           => apple
//! ```
//!
//! # Classification order
//!
//! [`classify_variant`] applies six rules in a fixed order. The order is load
//! bearing, not stylistic:
//!
//! 1. `busybox` anywhere in the output.
//! 2. A **BSD** marker in a successful banner.
//! 3. A **`GNU <package>`** pair in a successful banner.
//! 4. An **Apple** marker in a successful banner, after target triples are
//!    stripped.
//! 5. A *rejected* `--version` on a BSD-family platform.
//! 6. Otherwise `unknown`.
//!
//! **BSD must be tested before GNU.** macOS `grep` answers the probe with
//! `grep (BSD grep, GNU compatible) 2.6.0-FreeBSD`, which names both families:
//! it is a BSD tool advertising GNU compatibility, not a GNU tool. Reversing
//! rules 2 and 3 classifies it `gnu` and hands it the wrong overlay.
//!
//! **The Apple rule must strip target triples first.** `curl 8.7.1
//! (x86_64-apple-darwin25.0)` and `GNU bash, version 3.2.57(1)-release
//! (arm64-apple-darwin25)` both carry the token `apple` without being Apple
//! ports, and position is no help — "Apple" is the third token of `sort`'s
//! banner and the sixth of `curl`'s.
//!
//! This module owns the subprocess execution; the classification rule itself is
//! a pure function so it can be tested without a filesystem.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::adapter::overlay::{Platform, ProbeOutcome};
use crate::models::ToolVariant;

/// The probe every scan runs, and the one overlays reference by default.
pub const VERSION_PROBE_ARGS: &[&str] = &["--version"];

use super::OPTION_REJECTION_MARKERS;

/// Token *prefixes* that, in a successful `--version` banner, identify a BSD
/// userland outright.
///
/// Prefixes rather than whole tokens because libarchive's tools announce
/// themselves as `bsdtar 3.5.3 - libarchive 3.7.4` — the marker is fused into
/// the program name, and a whole-token test classifies that `unknown`.
const BSD_BANNER_TOKEN_PREFIXES: &[&str] = &["bsd", "freebsd", "openbsd", "netbsd"];

/// Platform names, as [`std::env::consts::OS`] spells them, whose userland is
/// BSD-derived and therefore corroborates a rejected `--version` (rule 5).
///
/// A named list rather than an enum test because [`Platform`] is an open
/// string: the previous closed enum could only name macOS and FreeBSD, so on
/// OpenBSD or NetBSD — where `ls` rejects `--version` exactly as it does on
/// macOS — the platform fell into an `Other` bucket and the tool classified
/// `unknown`. Adding a name here is now the whole change.
///
/// Darwin is included because macOS ships a BSD userland; `dragonfly` is the
/// name Rust uses for DragonFly BSD.
const BSD_FAMILY_PLATFORMS: &[&str] = &["macos", "freebsd", "openbsd", "netbsd", "dragonfly"];

/// Words after `GNU` that belong to licence boilerplate, not to a package name.
///
/// Every GNU tool prints `License GPLv3+: GNU GPL version 3 or later
/// <https://gnu.org/licenses/gpl.html>`, and so does any unrelated GPL tool
/// that quotes the licence. Without this list, "mentions the GPL" would read
/// as "is a GNU tool".
const GNU_BOILERPLATE_WORDS: &[&str] = &[
    "general", "public", "license", "licenses", "gpl", "lgpl", "agpl", "lesser", "affero", "org",
    "project",
];

/// An `<arch>-apple-<os>` target triple, which names Apple without the tool
/// being an Apple port.
// INVARIANT: a compile-time constant, valid regex.
static APPLE_TARGET_TRIPLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z0-9_.]+-apple-[a-z0-9_.]+").expect("valid static regex"));

/// The host platform this build is running on.
///
/// Reports [`std::env::consts::OS`] verbatim (normalised to lower case by
/// [`Platform::new`]). There is deliberately no translation table: every entry
/// in one would be a chance to lose a platform Rust already named correctly,
/// which is exactly what the old `_ => Other` arm did to OpenBSD, NetBSD,
/// DragonFly, Solaris and illumos.
///
/// This is the only platform-detection entry point, and it lives in the scanner
/// because `crate::adapter` is kept free of `std::env`.
pub fn current_platform() -> Platform {
    Platform::new(std::env::consts::OS)
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
/// Rules are applied in the order documented at the top of this module, and
/// that order is the whole design: see in particular why BSD is tested before
/// GNU. A BSD verdict from a *rejected* probe additionally requires platform
/// corroboration, because "the binary rejected `--version`" is on its own only
/// evidence that the tool is not GNU, and plenty of non-BSD tools reject it.
pub fn classify_variant(
    outcome: Option<&ProbeOutcome>,
    platform: Option<&Platform>,
) -> ToolVariant {
    let Some(outcome) = outcome else {
        return ToolVariant::Unknown;
    };
    let output = outcome.output.to_lowercase();

    if output.contains("busybox") {
        return ToolVariant::Busybox;
    }
    // Not every BSD tool rejects `--version`. macOS `grep` answers it with
    // "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD" — a self-declared banner,
    // which is stronger evidence than the rejection heuristic and needs no
    // platform corroboration. Tested before GNU precisely because of that
    // banner: it names both families, and BSD is the truthful reading.
    if outcome.succeeded && mentions_bsd(&output) {
        return ToolVariant::Bsd;
    }
    if outcome.succeeded && mentions_gnu_package(&output) {
        return ToolVariant::Gnu;
    }
    if outcome.succeeded && mentions_apple(&output) {
        return ToolVariant::Apple;
    }
    let rejected = !outcome.succeeded
        && OPTION_REJECTION_MARKERS
            .iter()
            .any(|marker| output.contains(marker));
    if rejected && is_bsd_family_platform(platform) {
        return ToolVariant::Bsd;
    }
    ToolVariant::Unknown
}

/// Whether `platform` names a BSD-derived userland (see [`BSD_FAMILY_PLATFORMS`]).
///
/// `None` — an unstated platform — is not corroboration, so it answers `false`:
/// a rejected `--version` alone only proves the tool is not GNU.
fn is_bsd_family_platform(platform: Option<&Platform>) -> bool {
    platform.is_some_and(|current| BSD_FAMILY_PLATFORMS.contains(&current.as_str()))
}

/// Split a lowercased banner into alphanumeric tokens.
fn tokens(output: &str) -> impl Iterator<Item = &str> {
    output
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
}

/// Whether a lowercased banner names a BSD userland.
///
/// A token *prefix* test rather than a substring test: `bsdtar` must count and
/// a build path such as `/opt/notbsdish/src` must not.
fn mentions_bsd(output: &str) -> bool {
    tokens(output).any(|token| {
        BSD_BANNER_TOKEN_PREFIXES
            .iter()
            .any(|marker| token.starts_with(marker))
    })
}

/// Whether a lowercased banner carries a `GNU <package>` pair.
///
/// The pair, not the bare word, is the signal: GNU tools ship under many
/// package names (`coreutils`, `diffutils`, `tar`, `bash`), so matching one of
/// them classifies the rest `unknown` — but matching `gnu` alone would promote
/// every tool that quotes the GPL. An overlay that genuinely needs the package
/// pins it in `match.probe.output_contains` instead.
fn mentions_gnu_package(output: &str) -> bool {
    let mut tokens = tokens(output);
    while let Some(token) = tokens.next() {
        if token != "gnu" {
            continue;
        }
        match tokens.next() {
            Some(next) if !GNU_BOILERPLATE_WORDS.contains(&next) => return true,
            _ => continue,
        }
    }
    false
}

/// Whether a lowercased banner names Apple as the *vendor* of the tool.
///
/// Target triples are removed first: `x86_64-apple-darwin25.0` says where a
/// binary was built, not who ported it, and `curl` and GNU `bash` both carry
/// one on macOS.
fn mentions_apple(output: &str) -> bool {
    APPLE_TARGET_TRIPLE_RE
        .replace_all(output, " ")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == "apple")
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

    /// Every banner in this table was read off a real binary. It is the
    /// regression suite for the classification order: `curl` and GNU `bash`
    /// exist here only to keep target-triple stripping honest, and `grep` only
    /// to keep BSD ahead of GNU.
    #[test]
    fn test_classify_variant_matches_real_banners() {
        let cases: &[(&str, ToolVariant)] = &[
            ("2.3-Apple (197)", ToolVariant::Apple),
            ("git version 2.50.1 (Apple Git-155)", ToolVariant::Apple),
            ("Apple diff (based on FreeBSD diff)", ToolVariant::Bsd),
            (
                "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD",
                ToolVariant::Bsd,
            ),
            (
                "bsdtar 3.5.3 - libarchive 3.7.4 zlib/1.2.12",
                ToolVariant::Bsd,
            ),
            ("ls (GNU coreutils) 9.7", ToolVariant::Gnu),
            ("diff (GNU diffutils) 3.10", ToolVariant::Gnu),
            ("tar (GNU tar) 1.35", ToolVariant::Gnu),
            (
                "GNU bash, version 3.2.57(1)-release (arm64-apple-darwin25)",
                ToolVariant::Gnu,
            ),
            (
                "curl 8.7.1 (x86_64-apple-darwin25.0) libcurl/8.7.1",
                ToolVariant::Unknown,
            ),
            (
                "BusyBox v1.36.1 (2023-11-07) multi-call binary",
                ToolVariant::Busybox,
            ),
            ("Python 3.12.10", ToolVariant::Unknown),
            ("OpenSSL 3.5.4 30 Sep 2025", ToolVariant::Unknown),
        ];
        for (banner, expected) in cases {
            let probe = outcome(true, banner);
            assert_eq!(
                classify_variant(Some(&probe), Some(&Platform::new("macos"))),
                *expected,
                "banner: {banner}"
            );
        }
    }

    #[test]
    fn test_classify_variant_gnu_coreutils_banner() {
        let probe = outcome(true, "ls (GNU coreutils) 9.4\n");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("linux"))),
            ToolVariant::Gnu
        );
    }

    #[test]
    fn test_classify_variant_gnu_coreutils_on_macos() {
        // Homebrew coreutils on a macOS box: the platform says BSD, the probe
        // says GNU, and the probe must win.
        let probe = outcome(true, "ls (GNU coreutils) 9.5\n");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("macos"))),
            ToolVariant::Gnu
        );
    }

    #[test]
    fn test_classify_variant_gnu_beyond_coreutils() {
        // The reason the variant is `gnu` and not `gnu-coreutils`: these are
        // four separate GNU projects, and every one of them was `unknown`
        // while the probe matched the literal banner "GNU coreutils".
        for banner in [
            "diff (GNU diffutils) 3.10",
            "sed (GNU sed) 4.9",
            "tar (GNU tar) 1.35",
            "GNU Awk 5.3.1, API 4.0",
        ] {
            let probe = outcome(true, banner);
            assert_eq!(
                classify_variant(Some(&probe), Some(&Platform::new("linux"))),
                ToolVariant::Gnu,
                "banner: {banner}"
            );
        }
    }

    #[test]
    fn test_classify_variant_gpl_boilerplate_is_not_a_gnu_banner() {
        // Quoting the licence is not a claim of authorship: only a
        // `GNU <package>` pair counts.
        let probe = outcome(
            true,
            "widget 1.2.3\nLicense GPLv3+: GNU GPL version 3 or later \
             <https://gnu.org/licenses/gpl.html>",
        );
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("linux"))),
            ToolVariant::Unknown
        );
    }

    #[test]
    fn test_classify_variant_apple_port_from_banner() {
        // macOS `sort` and `git`: Apple's own builds, which name Apple rather
        // than BSD and were classified `unknown` before this rule existed.
        for banner in ["2.3-Apple (197)", "git version 2.50.1 (Apple Git-155)"] {
            let probe = outcome(true, banner);
            assert_eq!(
                classify_variant(Some(&probe), Some(&Platform::new("macos"))),
                ToolVariant::Apple,
                "banner: {banner}"
            );
        }
    }

    #[test]
    fn test_classify_variant_apple_target_triple_is_not_an_apple_port() {
        // The naive fix — treating the token `apple` as a marker — breaks on
        // these two, so both stay as regression tests.
        let curl = outcome(true, "curl 8.7.1 (x86_64-apple-darwin25.0) libcurl/8.7.1");
        assert_eq!(
            classify_variant(Some(&curl), Some(&Platform::new("macos"))),
            ToolVariant::Unknown
        );
        let bash = outcome(
            true,
            "GNU bash, version 3.2.57(1)-release (arm64-apple-darwin25)",
        );
        assert_eq!(
            classify_variant(Some(&bash), Some(&Platform::new("macos"))),
            ToolVariant::Gnu
        );
    }

    #[test]
    fn test_classify_variant_bsd_marker_outranks_apple_marker() {
        // `Apple diff (based on FreeBSD diff)` names both vendors; naming
        // FreeBSD is the more specific claim, and diff@bsd depends on it.
        let probe = outcome(true, "Apple diff (based on FreeBSD diff)");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("macos"))),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_classify_variant_busybox_banner() {
        let probe = outcome(true, "BusyBox v1.36.1 (2023-11-07) multi-call binary.");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("linux"))),
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
            classify_variant(Some(&probe), Some(&Platform::new("macos"))),
            ToolVariant::Bsd
        );
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("linux"))),
            ToolVariant::Unknown,
            "rejecting --version is not by itself evidence of BSD"
        );
    }

    #[test]
    fn test_classify_variant_bsd_on_openbsd_from_rejected_version() {
        // The bug the open platform string fixes. OpenBSD `ls` rejects
        // `--version` exactly as macOS `ls` does, but the closed enum had no
        // `Openbsd` member, so the platform arrived as `Other`, rule 5 failed
        // its `Macos | Freebsd` test and the tool came back `unknown` — which
        // then denied it the `ls@bsd` overlay.
        let probe = outcome(
            false,
            "ls: unknown option -- -\nusage: ls [-1AaCcdFfgHhikLlmnopqRrSsTtux]",
        );
        for platform in ["openbsd", "netbsd", "dragonfly"] {
            assert_eq!(
                classify_variant(Some(&probe), Some(&Platform::new(platform))),
                ToolVariant::Bsd,
                "{platform} must corroborate a rejected --version"
            );
        }
    }

    #[test]
    fn test_classify_variant_bsd_platform_match_is_case_insensitive() {
        // An overlay or caller spelling the platform `OpenBSD` must not
        // silently fail rule 5; `Platform::new` normalises at the boundary.
        let probe = outcome(false, "ls: illegal option -- -");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("OpenBSD"))),
            ToolVariant::Bsd
        );
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("MACOS"))),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_classify_variant_illegal_option_wording() {
        let probe = outcome(false, "ls: illegal option -- -");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("freebsd"))),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_classify_variant_unknown_without_probe() {
        assert_eq!(
            classify_variant(None, Some(&Platform::new("macos"))),
            ToolVariant::Unknown
        );
    }

    #[test]
    fn test_classify_variant_bsd_from_successful_banner() {
        // Verbatim macOS output: not every BSD tool rejects `--version`, so the
        // rejection heuristic alone would have called this "unknown".
        let probe = outcome(true, "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("macos"))),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_classify_variant_bsd_banner_outranks_gnu_banner() {
        // "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD" names both families
        // and is a BSD tool advertising GNU compatibility. Swapping rules 2
        // and 3 hands it the GNU overlay.
        let probe = outcome(true, "grep (BSD grep, GNU compatible) 2.6.0-FreeBSD");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("macos"))),
            ToolVariant::Bsd
        );
    }

    #[test]
    fn test_mentions_bsd_matches_token_prefixes() {
        assert!(mentions_bsd(
            "grep (bsd grep, gnu compatible) 2.6.0-freebsd"
        ));
        assert!(mentions_bsd("2.6.0-freebsd"));
        assert!(
            mentions_bsd("bsdtar 3.5.3 - libarchive 3.7.4"),
            "libarchive fuses the marker into the program name"
        );
        assert!(
            !mentions_bsd("tool 1.0 built from /opt/notbsdish/src"),
            "an embedded substring must not claim the whole tool"
        );
        assert!(!mentions_bsd("openssl 3.5.4 30 sep 2025"));
    }

    #[test]
    fn test_mentions_apple_ignores_target_triples() {
        assert!(mentions_apple("2.3-apple (197)"));
        assert!(mentions_apple("git version 2.50.1 (apple git-155)"));
        assert!(!mentions_apple(
            "curl 8.7.1 (x86_64-apple-darwin25.0) libcurl/8.7.1"
        ));
        assert!(!mentions_apple(
            "gnu bash, version 3.2.57(1)-release (arm64-apple-darwin25)"
        ));
    }

    #[test]
    fn test_classify_variant_unknown_for_ordinary_tool() {
        let probe = outcome(true, "git version 2.43.0");
        assert_eq!(
            classify_variant(Some(&probe), Some(&Platform::new("macos"))),
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
    fn test_current_platform_reports_rust_os_verbatim() {
        // No translation table: whatever Rust names the host is the platform,
        // which is what removed the lossy `_ => Other` arm.
        assert_eq!(
            current_platform().as_str(),
            std::env::consts::OS.to_ascii_lowercase()
        );
        assert!(!current_platform().as_str().is_empty());
    }

    #[test]
    fn test_is_bsd_family_platform_covers_the_named_list() {
        for name in ["macos", "freebsd", "openbsd", "netbsd", "dragonfly"] {
            assert!(
                is_bsd_family_platform(Some(&Platform::new(name))),
                "{name} is a BSD-family platform"
            );
        }
        for name in ["linux", "windows", "solaris", "illumos"] {
            assert!(
                !is_bsd_family_platform(Some(&Platform::new(name))),
                "{name} is not a BSD-family platform"
            );
        }
        assert!(
            !is_bsd_family_platform(None),
            "an unstated platform corroborates nothing"
        );
    }
}
