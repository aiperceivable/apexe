//! Curated tool overlays: data model, matching, and merge semantics.
//!
//! An overlay is a hand-written, reviewed description of one *variant* of one
//! command — BSD `ls` as shipped by macOS, GNU coreutils `ls`, BusyBox `ls`.
//! It exists because heuristic scanning cannot be authoritative for system
//! built-ins: their `--help` is a single bundled usage line, their man pages
//! are prose, and neither states mutual exclusion or value types.
//!
//! # Layering
//!
//! This module is deliberately **pure data transformation**: no filesystem, no
//! subprocess, no `std::env`. Everything it needs about the running system
//! arrives in a [`MatchContext`] assembled by `crate::scanner`. That keeps the
//! adapter layer portable if it is ever extracted into a shared toolkit.

use serde::{Deserialize, Serialize};

use crate::models::{
    AnnotationOverrides, Confidence, FlagSource, ScannedArg, ScannedCLITool, ScannedFlag,
    ToolVariant, ValueType,
};

/// Overlay schema version this build understands.
pub const OVERLAY_SCHEMA_VERSION: &str = "1.0";

/// Host operating system, as named in an overlay's `match.platform`.
///
/// # Why this is a string and [`ToolVariant`] is not
///
/// The set of operating systems is **not enumerable**. OpenBSD, NetBSD,
/// DragonFly, Solaris, illumos, AIX and whatever ships next are all real hosts
/// an overlay may legitimately name, and a closed enum collapses every one of
/// them into a single `Other` bucket — which is both lossy and a code change
/// plus a release for every addition. The vocabulary is therefore borrowed
/// wholesale from [`std::env::consts::OS`]: the scanner reports what Rust
/// reports, and no translation table sits in between to lose information.
///
/// [`ToolVariant`] stays an enum for the opposite reason. Its members are
/// exhaustive because each one exists only if the classifier can *probe* for
/// it: `bsd`, `gnu`, `apple`, `busybox`, `unknown` each have detection logic
/// behind them, and adding a member means deciding how it is recognised. Were
/// it a string, an overlay could declare `variant: "gnu-ish"` and silently
/// never match, because no probe ever produces that token. Platform is an open
/// set that the host names; variant is a closed set that this crate decides.
///
/// # Normalisation
///
/// The wrapped value is lower-cased and trimmed on construction and on
/// deserialization, so `macos`, `macOS` and `MACOS` are the same platform and
/// a mis-cased overlay cannot silently fail to match. Comparison is therefore
/// plain `==` on the normalised form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Platform(String);

impl Platform {
    /// Normalise `raw` into a platform name.
    pub fn new(raw: &str) -> Self {
        Self(raw.trim().to_ascii_lowercase())
    }

    /// The normalised platform name, e.g. `"macos"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Platform {
    fn from(raw: String) -> Self {
        Platform::new(&raw)
    }
}

impl From<&str> for Platform {
    fn from(raw: &str) -> Self {
        Platform::new(raw)
    }
}

impl From<Platform> for String {
    fn from(platform: Platform) -> Self {
        platform.0
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How an overlay combines with the heuristic scan result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayMode {
    /// Replace the scan result's surface entirely. For system built-ins whose
    /// option set is fixed and fully enumerated by the overlay.
    Authoritative,
    /// Use the scan as the base; the overlay overrides matching flags and adds
    /// missing ones.
    #[default]
    Merge,
}

/// Whether a probe invocation is expected to succeed or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeExpectation {
    Success,
    Failure,
}

/// A probe the scanner must run for this overlay to be considered.
///
/// The probe is the only signal that separates "GNU coreutils installed on
/// macOS" from the system BSD binary, so it cannot be replaced by any path
/// heuristic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeMatcher {
    /// Arguments to pass to the binary (e.g. `["--version"]`).
    pub args: Vec<String>,
    /// Expected exit disposition.
    pub expect: ProbeExpectation,
    /// Substring that must appear in the probe's combined output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_contains: Option<String>,
}

/// The result of actually running a [`ProbeMatcher`]'s command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutcome {
    /// Arguments that were passed, used to pair an outcome with its matcher.
    pub args: Vec<String>,
    /// Whether the process exited successfully.
    pub succeeded: bool,
    /// stdout and stderr concatenated; both carry variant banners in practice.
    pub output: String,
}

/// Conditions under which an overlay applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayMatch {
    /// Host platforms this overlay is valid on, named as
    /// [`std::env::consts::OS`] names them. Empty means "any".
    #[serde(default)]
    pub platform: Vec<Platform>,
    /// Probe that must succeed for this overlay to apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeMatcher>,
    /// Binary path patterns (`*` and `?` wildcards). Empty means "any".
    #[serde(default)]
    pub binary_globs: Vec<String>,
    /// Version constraint, e.g. `">=9.0"` or `">=8.0,<10"`. Empty means "any".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_range: Option<String>,
}

/// How strongly an overlay matched, used to pick a winner between candidates.
///
/// Ordered so `max` selects the strongest evidence: a probe outranks a
/// platform-plus-path match, which outranks platform alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchStrength {
    /// Platform matched and nothing more specific was asserted.
    Platform,
    /// Platform matched and the binary path matched a declared glob.
    PlatformAndGlob,
    /// A probe was declared and its recorded outcome satisfied it.
    Probe,
    /// The operator named this overlay explicitly (`--overlay <path>`).
    Explicit,
}

/// A curated flag definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayFlag {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    #[serde(rename = "type", default)]
    pub value_type: ValueType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub repeatable: bool,
    /// Whether this flag can make the command run until it is killed.
    ///
    /// States possibility, not certainty. `tail -f` follows a regular file
    /// indefinitely, yet the BSD man page says it returns immediately when its
    /// input is a pipe — so the honest claim is "may not terminate on its
    /// own", and an executor should read it as a reason to bound the timeout
    /// or refuse, never as a prediction that the process will hang.
    #[serde(default)]
    pub long_running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
}

/// A curated positional argument definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayArg {
    pub name: String,
    #[serde(rename = "type", default)]
    pub value_type: ValueType,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub variadic: bool,
}

/// Which document an overlay's facts were read out of.
///
/// Deliberately narrow: these are the only three things a human can actually
/// have consulted. "I know this tool" is not a source, and there is no variant
/// of this enum that lets an author record it as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceSource {
    /// The installed tool's own man page.
    ManPage,
    /// The installed tool's own `--help` (or bundled usage) output.
    Help,
    /// Published upstream or vendor documentation for a stated release.
    VendorDocs,
}

/// The audit record of how an overlay's contents were checked.
///
/// An overlay is a human assertion about a program the agent will then act on.
/// Without this block a `verified` overlay is indistinguishable from a
/// confident guess, and the three required fields are exactly what decides
/// whether a past check still applies: *which* build was looked at
/// (`platform` + `tool_version`), *what* was read (`source`), and *when*
/// (`checked_on`) — because a verification against an older release cannot
/// know about flags added after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayProvenance {
    /// Platform the reference installation ran on.
    pub platform: Platform,
    /// Version of the reference installation, verbatim (e.g. `"9.7"`).
    pub tool_version: String,
    /// Upstream package the reference build came from (e.g. `"coreutils"`).
    ///
    /// This is where package identity lives now that [`ToolVariant::Gnu`]
    /// covers the whole GNU family: `coreutils` and `diffutils` are separate
    /// projects on separate release lines, so `tool_version` alone does not
    /// say what `9.7` is a version *of*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Which document the flag list was read from.
    pub source: EvidenceSource,
    /// ISO-8601 calendar date the check was performed (`YYYY-MM-DD`).
    pub checked_on: String,
    /// The exact command that produced the evidence, so it can be re-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Where the reference installation came from, e.g. a container image tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Citation for [`EvidenceSource::VendorDocs`], which has no command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Anything a later auditor needs that the fields above cannot carry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// A curated description of one command variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOverlay {
    /// Format version; only [`OVERLAY_SCHEMA_VERSION`] is accepted.
    pub schema_version: String,
    /// Command name this overlay describes (`ls`, not `/bin/ls`).
    pub command: String,
    /// Implementation variant this overlay describes.
    pub variant: ToolVariant,
    #[serde(rename = "match", default)]
    pub match_rules: OverlayMatch,
    #[serde(default)]
    pub mode: OverlayMode,
    /// Confidence stamped onto every flag this overlay contributes.
    #[serde(default)]
    pub confidence: Confidence,
    /// How the contents were checked. Mandatory at [`Confidence::Verified`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<OverlayProvenance>,
    /// Free-form authoring note; carries the "unreviewed" banner on scaffolds.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Replacement tool description. Empty leaves the scanned one alone.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub flags: Vec<OverlayFlag>,
    #[serde(default)]
    pub positional_args: Vec<OverlayArg>,
    #[serde(default)]
    pub annotations: AnnotationOverrides,
}

impl Default for ToolOverlay {
    fn default() -> Self {
        Self {
            schema_version: OVERLAY_SCHEMA_VERSION.to_string(),
            command: String::new(),
            variant: ToolVariant::default(),
            match_rules: OverlayMatch::default(),
            mode: OverlayMode::default(),
            confidence: Confidence::default(),
            provenance: None,
            notes: String::new(),
            description: String::new(),
            flags: Vec::new(),
            positional_args: Vec::new(),
            annotations: AnnotationOverrides::default(),
        }
    }
}

/// Everything about the running system an overlay may be matched against.
///
/// Assembled by the scanner (which owns the OS interaction) and consumed here.
#[derive(Debug, Clone, Default)]
pub struct MatchContext {
    /// Command name, normalized (`ls`, never `/bin/ls`).
    pub command: String,
    /// Detected implementation variant.
    pub variant: ToolVariant,
    /// Host platform.
    pub platform: Option<Platform>,
    /// Absolute path of the resolved binary.
    pub binary_path: String,
    /// Version string, when one could be extracted.
    pub version: Option<String>,
    /// Outcomes of every probe the scanner ran for this binary.
    pub probes: Vec<ProbeOutcome>,
}

impl ToolOverlay {
    /// A stable identifier for this overlay, e.g. `ls@bsd`.
    pub fn id(&self) -> String {
        format!("{}@{}", self.command, self.variant.as_str())
    }

    /// Whether this overlay's declared schema version is supported.
    pub fn is_supported_version(&self) -> bool {
        self.schema_version == OVERLAY_SCHEMA_VERSION
    }

    /// Decide whether this overlay applies to `context`, and how strongly.
    ///
    /// Precedence is expressed by the returned [`MatchStrength`]: `probe` beats
    /// `platform` + `binary_globs`, which beats `platform` alone. A declared
    /// condition that is *not* satisfied rejects the overlay outright — a
    /// stated probe never degrades into a weaker path-based match.
    pub fn evaluate(&self, context: &MatchContext) -> Option<MatchStrength> {
        if self.command != context.command || self.variant != context.variant {
            return None;
        }
        if !self.version_matches(context.version.as_deref()) {
            return None;
        }
        if !self.platform_matches(context.platform.as_ref()) {
            return None;
        }
        if let Some(ref probe) = self.match_rules.probe {
            return probe_satisfied(probe, &context.probes).then_some(MatchStrength::Probe);
        }
        if !self.match_rules.binary_globs.is_empty() {
            let matched = self
                .match_rules
                .binary_globs
                .iter()
                .any(|pattern| glob_matches(pattern, &context.binary_path));
            return matched.then_some(MatchStrength::PlatformAndGlob);
        }
        Some(MatchStrength::Platform)
    }

    fn platform_matches(&self, platform: Option<&Platform>) -> bool {
        if self.match_rules.platform.is_empty() {
            return true;
        }
        platform.is_some_and(|current| self.match_rules.platform.contains(current))
    }

    fn version_matches(&self, version: Option<&str>) -> bool {
        match self.match_rules.version_range.as_deref() {
            None => true,
            Some(range) => version_in_range(version, range),
        }
    }
}

/// A structural fault in an overlay document.
///
/// Separate from [`crate::scanner::OverlayError`] because these are statements
/// about the *content* of a well-formed document, and every one of them is
/// decidable without touching the filesystem or the described tool.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverlayDefect {
    #[error("schema_version is '{found}', but this build supports '{expected}'")]
    UnsupportedVersion { found: String, expected: String },

    #[error("command must not be empty")]
    EmptyCommand,

    #[error("command '{0}' looks like a path; overlays are keyed by bare command name")]
    CommandIsPath(String),

    #[error(
        "confidence 'verified' requires a 'provenance' block recording platform, \
         tool_version, source and checked_on"
    )]
    VerifiedWithoutProvenance,

    #[error("provenance.tool_version must not be empty")]
    EmptyToolVersion,

    #[error("provenance.checked_on '{0}' is not an ISO-8601 calendar date (YYYY-MM-DD)")]
    MalformedCheckedOn(String),

    #[error("provenance.source is 'vendor-docs', which requires a 'reference' citation")]
    VendorDocsWithoutReference,

    #[error("flag #{index} declares neither 'long' nor 'short'")]
    FlagWithoutName { index: usize },

    #[error("flag long form '{0}' must start with '--'")]
    MalformedLong(String),

    #[error("flag short form '{0}' must be a single dash followed by at least one character; a '--long' form belongs in the long field")]
    MalformedShort(String),

    #[error("flag '{0}' is declared more than once")]
    DuplicateFlag(String),

    #[error("flag '{0}' has type 'enum' but declares no enum_values")]
    EnumWithoutValues(String),

    #[error("flag '{flag}' conflicts_with '{other}', which this overlay never declares")]
    UnknownConflict { flag: String, other: String },

    #[error("positional arg #{0} has an empty name")]
    UnnamedArg(usize),
}

/// Check every invariant of `overlay` that needs no access to the real tool.
///
/// Returns all defects rather than the first, so an author fixes a file in one
/// pass. Completeness against the real tool is a separate, machine-checked
/// question — see `crate::scanner::overlay_verify`.
pub fn validate_overlay(overlay: &ToolOverlay) -> Vec<OverlayDefect> {
    let mut defects = Vec::new();
    if !overlay.is_supported_version() {
        defects.push(OverlayDefect::UnsupportedVersion {
            found: overlay.schema_version.clone(),
            expected: OVERLAY_SCHEMA_VERSION.to_string(),
        });
    }
    if overlay.command.trim().is_empty() {
        defects.push(OverlayDefect::EmptyCommand);
    } else if overlay.command.contains('/') || overlay.command.contains('\\') {
        defects.push(OverlayDefect::CommandIsPath(overlay.command.clone()));
    }
    validate_provenance(overlay, &mut defects);
    validate_flags(&overlay.flags, &mut defects);
    for (index, arg) in overlay.positional_args.iter().enumerate() {
        if arg.name.trim().is_empty() {
            defects.push(OverlayDefect::UnnamedArg(index));
        }
    }
    defects
}

/// Enforce the provenance contract: `verified` is a claim that must be backed.
fn validate_provenance(overlay: &ToolOverlay, defects: &mut Vec<OverlayDefect>) {
    let Some(ref provenance) = overlay.provenance else {
        if overlay.confidence == Confidence::Verified {
            defects.push(OverlayDefect::VerifiedWithoutProvenance);
        }
        return;
    };
    if provenance.tool_version.trim().is_empty() {
        defects.push(OverlayDefect::EmptyToolVersion);
    }
    if !is_iso_date(&provenance.checked_on) {
        defects.push(OverlayDefect::MalformedCheckedOn(
            provenance.checked_on.clone(),
        ));
    }
    let uncited = provenance
        .reference
        .as_deref()
        .is_none_or(|text| text.trim().is_empty());
    if provenance.source == EvidenceSource::VendorDocs && uncited {
        defects.push(OverlayDefect::VendorDocsWithoutReference);
    }
}

/// Check flag naming, uniqueness, enum completeness and conflict references.
fn validate_flags(flags: &[OverlayFlag], defects: &mut Vec<OverlayDefect>) {
    let mut seen: Vec<String> = Vec::new();
    for (index, flag) in flags.iter().enumerate() {
        if flag.long.is_none() && flag.short.is_none() {
            defects.push(OverlayDefect::FlagWithoutName { index });
        }
        // Validated per field rather than over the merged name list: a name is
        // only well-formed relative to the field it was declared in, and a
        // combined loop cannot tell `--verbose` written in `short` from the
        // same token legitimately written in `long`.
        if let Some(ref long) = flag.long {
            if !long.starts_with("--") || long.chars().count() < 3 {
                defects.push(OverlayDefect::MalformedLong(long.clone()));
            }
        }
        if let Some(ref short) = flag.short {
            // A single-dash form may be longer than one character. `find`'s
            // expression language is built entirely out of such tokens —
            // `-name`, `-delete`, `-maxdepth`, `-files0-from` — and they are
            // what a user types, so an overlay has to be able to state them.
            // Still rejected: a bare `-`, anything without a leading dash, and
            // a `--long` form misfiled here, which the JSON Schema's `^-[^-]`
            // pattern also refuses.
            if !short.starts_with('-') || short.starts_with("--") || short.chars().count() < 2 {
                defects.push(OverlayDefect::MalformedShort(short.clone()));
            }
        }
        for name in flag.names() {
            if seen.contains(&name) {
                defects.push(OverlayDefect::DuplicateFlag(name.clone()));
            }
            seen.push(name);
        }
        if flag.value_type == ValueType::Enum && flag.enum_values.as_ref().is_none_or(Vec::is_empty)
        {
            defects.push(OverlayDefect::EnumWithoutValues(flag.display_name()));
        }
    }
    for flag in flags {
        for other in &flag.conflicts_with {
            if !seen.iter().any(|name| name == other) {
                defects.push(OverlayDefect::UnknownConflict {
                    flag: flag.display_name(),
                    other: other.clone(),
                });
            }
        }
    }
}

/// Whether `text` is a `YYYY-MM-DD` calendar date with plausible components.
///
/// A hand-rolled check rather than a date crate: the only thing that matters is
/// that the field is a real, sortable date an auditor can compare against a
/// release timeline, and pulling in `chrono` for ten lines is not worth it.
fn is_iso_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    let [year, month, day] = parts[..] else {
        return false;
    };
    let numeric = |part: &str, width: usize| -> Option<u32> {
        (part.len() == width && part.chars().all(|c| c.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day)) =
        (numeric(year, 4), numeric(month, 2), numeric(day, 2))
    else {
        return false;
    };
    year >= 1970 && (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// Whether a recorded probe outcome satisfies a declared probe matcher.
fn probe_satisfied(matcher: &ProbeMatcher, outcomes: &[ProbeOutcome]) -> bool {
    let Some(outcome) = outcomes.iter().find(|o| o.args == matcher.args) else {
        return false;
    };
    let expectation_met = match matcher.expect {
        ProbeExpectation::Success => outcome.succeeded,
        ProbeExpectation::Failure => !outcome.succeeded,
    };
    if !expectation_met {
        return false;
    }
    match matcher.output_contains {
        None => true,
        Some(ref needle) => outcome.output.contains(needle.as_str()),
    }
}

/// Match `path` against a `*` / `?` wildcard `pattern`.
///
/// A dedicated matcher rather than a glob crate: overlays only ever need
/// literal paths with the odd wildcard, and the adapter layer stays
/// dependency-light on purpose.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    let (mut p, mut t) = (0_usize, 0_usize);
    let (mut star, mut backtrack) = (None, 0_usize);

    while t < path.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == path[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            backtrack = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            backtrack += 1;
            t = backtrack;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Whether `version` satisfies a comma-separated constraint list.
///
/// Supports `*`, `>=`, `>`, `<=`, `<` and `=` over dot-separated numeric
/// versions; every constraint must hold. An absent version satisfies only `*`.
fn version_in_range(version: Option<&str>, range: &str) -> bool {
    let range = range.trim();
    if range.is_empty() || range == "*" {
        return true;
    }
    let Some(version) = version else {
        return false;
    };
    range.split(',').all(|constraint| {
        let constraint = constraint.trim();
        let (op, bound) = split_constraint(constraint);
        match compare_versions(version, bound) {
            std::cmp::Ordering::Less => matches!(op, "<" | "<="),
            std::cmp::Ordering::Equal => matches!(op, "=" | "==" | ">=" | "<="),
            std::cmp::Ordering::Greater => matches!(op, ">" | ">="),
        }
    })
}

/// Split `">=9.0"` into `(">=", "9.0")`, defaulting to an equality comparison.
fn split_constraint(constraint: &str) -> (&str, &str) {
    for op in [">=", "<=", "==", ">", "<", "="] {
        if let Some(bound) = constraint.strip_prefix(op) {
            return (op, bound.trim());
        }
    }
    ("=", constraint)
}

/// Compare two dot-separated numeric versions component by component.
///
/// Non-numeric components compare as 0, which is deliberate: overlays are
/// keyed on release lines, not on pre-release tags.
fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |text: &str| -> Vec<u64> {
        text.split('.')
            .map(|part| {
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                digits.parse::<u64>().unwrap_or(0)
            })
            .collect()
    };
    let (left, right) = (parse(left), parse(right));
    let width = left.len().max(right.len());
    for index in 0..width {
        let l = left.get(index).copied().unwrap_or(0);
        let r = right.get(index).copied().unwrap_or(0);
        if l != r {
            return l.cmp(&r);
        }
    }
    std::cmp::Ordering::Equal
}

impl OverlayFlag {
    /// Every name this flag claims, long form first.
    ///
    /// Both alias forms count independently: an overlay that gets `--all` right
    /// and `-a` wrong is wrong about one flag name, and flattening to a single
    /// identifier would hide that.
    pub fn names(&self) -> Vec<String> {
        self.long.iter().chain(self.short.iter()).cloned().collect()
    }

    /// The name to show a human, preferring the long form.
    pub fn display_name(&self) -> String {
        self.names().first().cloned().unwrap_or_default()
    }

    /// Convert to the scanner's flag model, stamped with overlay provenance.
    fn to_scanned(&self, confidence: Confidence) -> ScannedFlag {
        ScannedFlag {
            long_name: self.long.clone(),
            short_name: self.short.clone(),
            description: self.description.clone(),
            value_type: self.value_type,
            required: self.required,
            default: self.default.clone(),
            enum_values: self.enum_values.clone(),
            repeatable: self.repeatable,
            value_name: self.value_name.clone(),
            long_running: self.long_running,
            conflicts_with: self.conflicts_with.clone(),
            sources: vec![FlagSource::Overlay],
            confidence,
        }
    }
}

impl OverlayArg {
    fn to_scanned(&self) -> ScannedArg {
        ScannedArg {
            name: self.name.clone(),
            description: self.description.clone(),
            value_type: self.value_type,
            required: self.required,
            variadic: self.variadic,
        }
    }
}

/// Whether two flags denote the same option, matching on either alias form.
fn same_flag(left: &ScannedFlag, right: &ScannedFlag) -> bool {
    let long_matches = left.long_name.is_some() && left.long_name == right.long_name;
    let short_matches = left.short_name.is_some() && left.short_name == right.short_name;
    long_matches || short_matches
}

/// Apply `overlay` to `tool` in place.
///
/// In [`OverlayMode::Authoritative`] the overlay *is* the answer: flags,
/// positional args and subcommands are replaced wholesale, because a curated
/// authoritative overlay enumerates the command's entire surface and anything
/// the heuristic scan added is noise (a `ls` "subcommand" scraped out of a
/// completion script, say). In [`OverlayMode::Merge`] the scan stays the base
/// and the overlay overrides matching flags and appends missing ones.
pub fn apply_overlay(tool: &mut ScannedCLITool, overlay: &ToolOverlay) {
    let confidence = overlay.confidence;
    let curated: Vec<ScannedFlag> = overlay
        .flags
        .iter()
        .map(|flag| flag.to_scanned(confidence))
        .collect();

    match overlay.mode {
        OverlayMode::Authoritative => {
            tool.global_flags = curated;
            tool.positional_args = overlay
                .positional_args
                .iter()
                .map(OverlayArg::to_scanned)
                .collect();
            tool.subcommands.clear();
        }
        OverlayMode::Merge => {
            merge_flags(&mut tool.global_flags, curated);
            merge_positional_args(&mut tool.positional_args, &overlay.positional_args);
        }
    }

    if !overlay.description.is_empty() {
        tool.description.clone_from(&overlay.description);
    }
    if !overlay.annotations.is_empty() {
        tool.annotation_overrides = overlay.annotations.clone();
    }
    tool.overlay = Some(overlay.id());
}

/// Override matching flags with their curated form and append the rest.
fn merge_flags(target: &mut Vec<ScannedFlag>, curated: Vec<ScannedFlag>) {
    for flag in curated {
        match target
            .iter_mut()
            .find(|existing| same_flag(existing, &flag))
        {
            Some(existing) => *existing = merged_flag(existing, flag),
            None => target.push(flag),
        }
    }
}

/// Combine a curated flag over a scanned one, keeping scanned aliases the
/// overlay did not restate and preserving the scanned flag's provenance trail.
fn merged_flag(existing: &ScannedFlag, curated: ScannedFlag) -> ScannedFlag {
    let mut merged = curated;
    merged.long_name = merged.long_name.or_else(|| existing.long_name.clone());
    merged.short_name = merged.short_name.or_else(|| existing.short_name.clone());
    if merged.description.is_empty() {
        merged.description.clone_from(&existing.description);
    }
    let mut sources = existing.sources.clone();
    if !sources.contains(&FlagSource::Overlay) {
        sources.push(FlagSource::Overlay);
    }
    merged.confidence = merged.confidence.max(Confidence::from_sources(&sources));
    merged.sources = sources;
    merged
}

/// Override positional args by name and append the rest.
fn merge_positional_args(target: &mut Vec<ScannedArg>, curated: &[OverlayArg]) {
    for arg in curated {
        let replacement = arg.to_scanned();
        match target.iter_mut().find(|existing| existing.name == arg.name) {
            Some(existing) => *existing = replacement,
            None => target.push(replacement),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(mode: OverlayMode) -> ToolOverlay {
        ToolOverlay {
            schema_version: OVERLAY_SCHEMA_VERSION.to_string(),
            command: "ls".to_string(),
            variant: ToolVariant::Bsd,
            match_rules: OverlayMatch::default(),
            mode,
            confidence: Confidence::Verified,
            description: "List directory contents.".to_string(),
            flags: vec![OverlayFlag {
                short: Some("-l".to_string()),
                value_type: ValueType::Boolean,
                description: "List in long format.".to_string(),
                conflicts_with: vec!["-1".to_string()],
                ..Default::default()
            }],
            positional_args: vec![OverlayArg {
                name: "file".to_string(),
                value_type: ValueType::Path,
                variadic: true,
                ..Default::default()
            }],
            annotations: AnnotationOverrides {
                readonly: Some(true),
                ..Default::default()
            },
            // The fixture claims `Verified`, so it must carry provenance for the
            // same reason a real overlay must: a verified claim nobody can
            // re-check is indistinguishable from a guess.
            provenance: Some(OverlayProvenance {
                platform: Platform::new("macos"),
                tool_version: "unknown".to_string(),
                package: None,
                source: EvidenceSource::ManPage,
                checked_on: "2026-07-27".to_string(),
                command: Some("man ls".to_string()),
                environment: None,
                reference: None,
                notes: String::new(),
            }),
            notes: String::new(),
        }
    }

    fn scanned_flag(short: Option<&str>, long: Option<&str>, description: &str) -> ScannedFlag {
        let mut flag = ScannedFlag {
            short_name: short.map(str::to_string),
            long_name: long.map(str::to_string),
            description: description.to_string(),
            ..Default::default()
        };
        flag.add_source(FlagSource::Help);
        flag
    }

    fn context(variant: ToolVariant, platform: Platform, path: &str) -> MatchContext {
        MatchContext {
            command: "ls".to_string(),
            variant,
            platform: Some(platform),
            binary_path: path.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_overlay_id_combines_command_and_variant() {
        assert_eq!(overlay(OverlayMode::Merge).id(), "ls@bsd");
    }

    #[test]
    fn test_evaluate_rejects_other_command() {
        let overlay = overlay(OverlayMode::Merge);
        let mut ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        ctx.command = "cat".to_string();
        assert_eq!(overlay.evaluate(&ctx), None);
    }

    #[test]
    fn test_evaluate_rejects_other_variant() {
        // The whole point of variant detection: a GNU binary must not pick up
        // the BSD overlay just because the command name agrees.
        let overlay = overlay(OverlayMode::Merge);
        let ctx = context(ToolVariant::Gnu, Platform::new("macos"), "/bin/ls");
        assert_eq!(overlay.evaluate(&ctx), None);
    }

    #[test]
    fn test_evaluate_platform_only_is_weakest_match() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.platform = vec![Platform::new("macos")];
        let ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        assert_eq!(overlay.evaluate(&ctx), Some(MatchStrength::Platform));
    }

    #[test]
    fn test_evaluate_rejects_wrong_platform() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.platform = vec![Platform::new("macos"), Platform::new("freebsd")];
        let ctx = context(ToolVariant::Bsd, Platform::new("linux"), "/bin/ls");
        assert_eq!(overlay.evaluate(&ctx), None);
    }

    #[test]
    fn test_evaluate_glob_outranks_platform_alone() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.platform = vec![Platform::new("macos")];
        overlay.match_rules.binary_globs = vec!["/bin/*".to_string()];
        let ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        assert_eq!(overlay.evaluate(&ctx), Some(MatchStrength::PlatformAndGlob));
        assert!(MatchStrength::PlatformAndGlob > MatchStrength::Platform);
    }

    #[test]
    fn test_evaluate_rejects_unmatched_glob() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.binary_globs = vec!["/bin/ls".to_string()];
        let ctx = context(
            ToolVariant::Bsd,
            Platform::new("macos"),
            "/opt/homebrew/bin/ls",
        );
        assert_eq!(overlay.evaluate(&ctx), None);
    }

    #[test]
    fn test_evaluate_probe_outranks_glob() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.binary_globs = vec!["/bin/ls".to_string()];
        overlay.match_rules.probe = Some(ProbeMatcher {
            args: vec!["--version".to_string()],
            expect: ProbeExpectation::Failure,
            output_contains: None,
        });
        let mut ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        ctx.probes = vec![ProbeOutcome {
            args: vec!["--version".to_string()],
            succeeded: false,
            output: "ls: unrecognized option `--version'".to_string(),
        }];
        assert_eq!(overlay.evaluate(&ctx), Some(MatchStrength::Probe));
        assert!(MatchStrength::Probe > MatchStrength::PlatformAndGlob);
    }

    #[test]
    fn test_evaluate_declared_probe_that_fails_rejects_outright() {
        // A stated probe must not silently degrade into a path match: that is
        // exactly the "GNU coreutils on macOS" case the probe exists to catch.
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.binary_globs = vec!["/bin/ls".to_string()];
        overlay.match_rules.probe = Some(ProbeMatcher {
            args: vec!["--version".to_string()],
            expect: ProbeExpectation::Failure,
            output_contains: None,
        });
        let mut ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        ctx.probes = vec![ProbeOutcome {
            args: vec!["--version".to_string()],
            succeeded: true,
            output: "ls (GNU coreutils) 9.4".to_string(),
        }];
        assert_eq!(overlay.evaluate(&ctx), None);
    }

    #[test]
    fn test_evaluate_probe_requires_output_substring() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.variant = ToolVariant::Gnu;
        overlay.match_rules.probe = Some(ProbeMatcher {
            args: vec!["--version".to_string()],
            expect: ProbeExpectation::Success,
            output_contains: Some("GNU coreutils".to_string()),
        });
        let mut ctx = context(ToolVariant::Gnu, Platform::new("linux"), "/usr/bin/ls");
        ctx.probes = vec![ProbeOutcome {
            args: vec!["--version".to_string()],
            succeeded: true,
            output: "ls 1.2.3".to_string(),
        }];
        assert_eq!(overlay.evaluate(&ctx), None);

        ctx.probes[0].output = "ls (GNU coreutils) 9.4".to_string();
        assert_eq!(overlay.evaluate(&ctx), Some(MatchStrength::Probe));
    }

    #[test]
    fn test_evaluate_probe_absent_outcome_rejects() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.probe = Some(ProbeMatcher {
            args: vec!["--version".to_string()],
            expect: ProbeExpectation::Failure,
            output_contains: None,
        });
        let ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        assert_eq!(overlay.evaluate(&ctx), None);
    }

    #[test]
    fn test_evaluate_respects_version_range() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.match_rules.version_range = Some(">=9.0".to_string());
        let mut ctx = context(ToolVariant::Bsd, Platform::new("macos"), "/bin/ls");
        ctx.version = Some("8.32".to_string());
        assert_eq!(overlay.evaluate(&ctx), None);
        ctx.version = Some("9.4".to_string());
        assert_eq!(overlay.evaluate(&ctx), Some(MatchStrength::Platform));
    }

    #[test]
    fn test_glob_matches_literal_and_wildcards() {
        assert!(glob_matches("/bin/ls", "/bin/ls"));
        assert!(glob_matches("/bin/*", "/bin/ls"));
        assert!(glob_matches("*/bin/ls", "/opt/homebrew/bin/ls"));
        assert!(glob_matches("/bin/l?", "/bin/ls"));
        assert!(!glob_matches("/bin/ls", "/usr/bin/ls"));
        assert!(!glob_matches("/bin/l?", "/bin/lsx"));
    }

    #[test]
    fn test_version_in_range_handles_operators() {
        assert!(version_in_range(Some("9.4"), "*"));
        assert!(version_in_range(Some("9.4"), ">=9.0"));
        assert!(version_in_range(Some("9.0"), ">=9.0"));
        assert!(!version_in_range(Some("8.32"), ">=9.0"));
        assert!(version_in_range(Some("9.4"), ">=8.0,<10"));
        assert!(!version_in_range(Some("10.1"), ">=8.0,<10"));
        assert!(version_in_range(Some("9.4"), "9.4"));
    }

    #[test]
    fn test_version_in_range_missing_version_only_matches_wildcard() {
        assert!(version_in_range(None, "*"));
        assert!(!version_in_range(None, ">=9.0"));
    }

    #[test]
    fn test_apply_overlay_authoritative_replaces_scan_surface() {
        let mut tool = ScannedCLITool {
            name: "ls".to_string(),
            description: "ls: unrecognized option".to_string(),
            global_flags: vec![scanned_flag(Some("-Z"), None, "bogus")],
            subcommands: vec![],
            ..Default::default()
        };
        apply_overlay(&mut tool, &overlay(OverlayMode::Authoritative));

        assert_eq!(tool.global_flags.len(), 1);
        assert_eq!(tool.global_flags[0].short_name.as_deref(), Some("-l"));
        assert_eq!(tool.global_flags[0].confidence, Confidence::Verified);
        assert_eq!(tool.global_flags[0].sources, vec![FlagSource::Overlay]);
        assert_eq!(tool.global_flags[0].conflicts_with, vec!["-1".to_string()]);
        assert_eq!(tool.positional_args.len(), 1);
        assert_eq!(tool.description, "List directory contents.");
        assert_eq!(tool.overlay.as_deref(), Some("ls@bsd"));
        assert_eq!(tool.annotation_overrides.readonly, Some(true));
    }

    #[test]
    fn test_apply_overlay_merge_keeps_scanned_flags() {
        let mut tool = ScannedCLITool {
            name: "ls".to_string(),
            global_flags: vec![scanned_flag(Some("-a"), Some("--all"), "Include hidden")],
            ..Default::default()
        };
        apply_overlay(&mut tool, &overlay(OverlayMode::Merge));

        assert_eq!(
            tool.global_flags.len(),
            2,
            "scanned flag must survive merge"
        );
        assert_eq!(tool.global_flags[0].short_name.as_deref(), Some("-a"));
        assert_eq!(tool.global_flags[1].short_name.as_deref(), Some("-l"));
    }

    #[test]
    fn test_apply_overlay_merge_overrides_matching_flag() {
        let mut tool = ScannedCLITool {
            name: "ls".to_string(),
            global_flags: vec![scanned_flag(Some("-l"), Some("--long"), "long")],
            ..Default::default()
        };
        apply_overlay(&mut tool, &overlay(OverlayMode::Merge));

        assert_eq!(tool.global_flags.len(), 1);
        let flag = &tool.global_flags[0];
        assert_eq!(flag.description, "List in long format.");
        assert_eq!(
            flag.long_name.as_deref(),
            Some("--long"),
            "an alias the overlay did not restate must be kept"
        );
        assert_eq!(flag.confidence, Confidence::Verified);
        assert_eq!(flag.sources, vec![FlagSource::Help, FlagSource::Overlay]);
    }

    #[test]
    fn test_apply_overlay_authoritative_clears_scanned_subcommands() {
        let mut tool = ScannedCLITool {
            name: "ls".to_string(),
            subcommands: vec![crate::models::ScannedCommand {
                name: "bogus".to_string(),
                full_command: "ls bogus".to_string(),
                description: String::new(),
                flags: vec![],
                positional_args: vec![],
                subcommands: vec![],
                examples: vec![],
                help_format: crate::models::HelpFormat::Unknown,
                structured_output: crate::models::StructuredOutputInfo::default(),
                raw_help: String::new(),
            }],
            ..Default::default()
        };
        apply_overlay(&mut tool, &overlay(OverlayMode::Authoritative));
        assert!(tool.subcommands.is_empty());
    }

    #[test]
    fn test_apply_overlay_empty_description_keeps_scanned_one() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.description = String::new();
        let mut tool = ScannedCLITool {
            name: "ls".to_string(),
            description: "Scanned description".to_string(),
            ..Default::default()
        };
        apply_overlay(&mut tool, &overlay);
        assert_eq!(tool.description, "Scanned description");
    }

    #[test]
    fn test_overlay_round_trips_through_json() {
        let original = overlay(OverlayMode::Authoritative);
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: ToolOverlay = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.id(), "ls@bsd");
        assert_eq!(decoded.mode, OverlayMode::Authoritative);
        assert!(decoded.is_supported_version());
    }

    #[test]
    fn test_overlay_flag_long_running_reaches_the_scanned_flag() {
        // The point of the field: prose in a description is not actionable, so
        // it has to survive the conversion an executor actually reads.
        let mut overlay = overlay(OverlayMode::Authoritative);
        overlay.command = "tail".to_string();
        overlay.flags = vec![
            OverlayFlag {
                short: Some("-f".to_string()),
                long_running: true,
                description: "Follow the file.".to_string(),
                ..Default::default()
            },
            OverlayFlag {
                short: Some("-n".to_string()),
                description: "Number of lines.".to_string(),
                ..Default::default()
            },
        ];
        let mut tool = ScannedCLITool {
            name: "tail".to_string(),
            ..Default::default()
        };
        apply_overlay(&mut tool, &overlay);

        assert!(tool.global_flags[0].long_running);
        assert!(
            !tool.global_flags[1].long_running,
            "an ordinary flag must not inherit the claim"
        );
    }

    #[test]
    fn test_overlay_flag_long_running_defaults_to_false_when_absent() {
        // Overlays written before the field existed must not start asserting
        // something their author never checked.
        let document = r#"{ "short": "-n", "type": "integer" }"#;
        let flag: OverlayFlag = serde_json::from_str(document).unwrap();
        assert!(!flag.long_running);
    }

    #[test]
    fn test_overlay_provenance_package_round_trips() {
        // `9.7` alone does not say what it is a version *of* once the variant
        // is the whole GNU family.
        let document = r#"{
          "platform": "linux",
          "tool_version": "9.7",
          "package": "coreutils",
          "source": "help",
          "checked_on": "2026-07-27"
        }"#;
        let provenance: OverlayProvenance = serde_json::from_str(document).unwrap();
        assert_eq!(provenance.package.as_deref(), Some("coreutils"));
        let encoded = serde_json::to_string(&provenance).unwrap();
        assert!(encoded.contains("\"package\":\"coreutils\""));
    }

    /// The standalone JSON Schema, which is the contract for overlay authors
    /// who never see the Rust types.
    const SCHEMA: &str = include_str!("../../schemas/tool-overlay.schema.json");

    fn schema_enum(pointer: &str) -> Vec<String> {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        schema
            .pointer(pointer)
            .unwrap_or_else(|| panic!("missing {pointer}"))
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn test_schema_file_is_valid_json_and_pins_the_supported_version() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            OVERLAY_SCHEMA_VERSION
        );
    }

    #[test]
    fn test_schema_variant_enum_matches_rust_variants() {
        // Guards against the standalone schema silently drifting from the
        // types it documents.
        let mut declared = schema_enum("/$defs/variant/enum");
        declared.sort();
        let mut actual: Vec<String> = [
            ToolVariant::Bsd,
            ToolVariant::Gnu,
            ToolVariant::Apple,
            ToolVariant::Busybox,
            ToolVariant::Unknown,
        ]
        .iter()
        .map(|v| v.as_str().to_string())
        .collect();
        actual.sort();
        assert_eq!(declared, actual);
    }

    #[test]
    fn test_schema_platform_is_an_open_string_with_examples() {
        // The point of the change: `enum` here would reimpose the closed set
        // the schema was freed from, so its absence is the assertion.
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        let platform = schema.pointer("/$defs/platform").expect("missing platform");
        assert_eq!(platform["type"], "string");
        assert!(
            platform.get("enum").is_none(),
            "platform must stay an open string; `enum` closes it again"
        );
        let examples = schema_enum("/$defs/platform/examples");
        for expected in ["macos", "linux", "freebsd", "openbsd", "netbsd"] {
            assert!(
                examples.iter().any(|value| value == expected),
                "schema examples should mention {expected}"
            );
        }
    }

    #[test]
    fn test_schema_platform_examples_are_all_normalized() {
        // A mis-cased example would document a value the loader silently
        // rewrites, which is exactly the confusion normalisation removes.
        for example in schema_enum("/$defs/platform/examples") {
            assert_eq!(Platform::new(&example).as_str(), example);
        }
    }

    #[test]
    fn test_schema_value_type_enum_matches_rust_value_types() {
        let mut declared = schema_enum("/$defs/valueType/enum");
        declared.sort();
        let mut actual: Vec<String> = [
            ValueType::String,
            ValueType::Integer,
            ValueType::Float,
            ValueType::Boolean,
            ValueType::Path,
            ValueType::Enum,
            ValueType::Url,
            ValueType::Unknown,
        ]
        .iter()
        .map(|t| {
            serde_json::to_value(t)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        actual.sort();
        assert_eq!(declared, actual);
    }

    #[test]
    fn test_schema_confidence_enum_matches_rust_confidence() {
        let mut declared = schema_enum("/properties/confidence/enum");
        declared.sort();
        let mut actual: Vec<String> = [
            Confidence::Low,
            Confidence::Medium,
            Confidence::High,
            Confidence::Verified,
        ]
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        actual.sort();
        assert_eq!(declared, actual);
    }

    #[test]
    fn test_overlay_rejects_unknown_schema_version() {
        let mut overlay = overlay(OverlayMode::Merge);
        overlay.schema_version = "99.0".to_string();
        assert!(!overlay.is_supported_version());
    }

    /// `find`'s entire expression language is single-dash multi-character
    /// tokens, so rejecting them would make the tool inexpressible.
    #[test]
    fn test_validate_overlay_accepts_multi_character_short_form() {
        let mut overlay = overlay(OverlayMode::Authoritative);
        overlay.flags = vec![OverlayFlag {
            short: Some("-maxdepth".to_string()),
            value_type: ValueType::Integer,
            description: "Descend at most n directory levels.".to_string(),
            ..Default::default()
        }];
        assert!(validate_overlay(&overlay).is_empty());
    }

    /// The relaxation above must not turn the check off: a bare dash names no
    /// option, and a name with no dash at all is the typo the check exists for.
    #[test]
    fn test_validate_overlay_rejects_bare_dash_and_undashed_short_form() {
        // `--verbose` is included because relaxing the short-form length rule
        // for `find`'s `-maxdepth` opened a hole: validation used to walk the
        // merged name list, so a `--long` token misfiled under `short` matched
        // the long branch and passed. The JSON Schema's `^-[^-]` always
        // refused it; the Rust check now agrees.
        for bad in ["-", "maxdepth", "--verbose"] {
            let mut overlay = overlay(OverlayMode::Authoritative);
            overlay.flags = vec![OverlayFlag {
                short: Some(bad.to_string()),
                value_type: ValueType::Boolean,
                ..Default::default()
            }];
            let defects = validate_overlay(&overlay);
            assert!(
                defects.iter().any(
                    |defect| matches!(defect, OverlayDefect::MalformedShort(name) if name == bad)
                ),
                "'{bad}' must be reported as a malformed short form, got {defects:?}"
            );
        }
    }
}
