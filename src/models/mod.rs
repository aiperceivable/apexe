use serde::{Deserialize, Serialize};

/// Type classification for CLI flag/argument values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Integer,
    Float,
    Boolean,
    Path,
    Enum,
    Url,
    #[default]
    Unknown,
}

/// Where a flag definition came from.
///
/// Provenance is recorded per flag so a consumer can tell a curated fact from a
/// text-scraping guess, and so agreement between two independent heuristic
/// sources is distinguishable from a single unconfirmed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlagSource {
    /// Parsed from `--help` output by a format parser (Tier 1).
    Help,
    /// Parsed from the tool's man page (Tier 2).
    ManPage,
    /// Extracted from a shell completion script (Tier 3).
    Completion,
    /// Supplied by a curated tool overlay.
    Overlay,
}

/// How much a flag definition can be trusted.
///
/// Ordered worst-to-best so comparison and `max` behave as expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// A single heuristic source with nothing corroborating it.
    #[default]
    Low,
    /// Two or more independent heuristic sources agree.
    Medium,
    /// A machine-generated completion spec, which the tool itself ships.
    High,
    /// A curated overlay, reviewed against the tool's own documentation.
    Verified,
}

impl Confidence {
    /// Derive a confidence level from the sources backing a flag.
    ///
    /// Overlay outranks completion, which outranks any number of heuristic
    /// sources; two agreeing heuristic sources outrank one.
    pub fn from_sources(sources: &[FlagSource]) -> Self {
        if sources.contains(&FlagSource::Overlay) {
            Confidence::Verified
        } else if sources.contains(&FlagSource::Completion) {
            Confidence::High
        } else if sources.len() >= 2 {
            Confidence::Medium
        } else {
            Confidence::Low
        }
    }
}

/// Detected help output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelpFormat {
    Gnu,
    Click,
    Argparse,
    Cobra,
    Clap,
    #[default]
    Unknown,
}

/// A single CLI flag parsed from help output.
///
/// `Default` is derived so a producer can set only the fields it actually knows
/// and let provenance (`sources` / `confidence`) be stamped centrally by the
/// orchestrator rather than by every parser.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScannedFlag {
    /// Long form flag (e.g., "--message"). None if only short form exists.
    pub long_name: Option<String>,
    /// Short form flag (e.g., "-m"). None if only long form exists.
    pub short_name: Option<String>,
    /// Human-readable description from help text.
    pub description: String,
    /// Inferred type of the flag's value.
    pub value_type: ValueType,
    /// Whether the flag is required.
    pub required: bool,
    /// Default value as string, or None.
    pub default: Option<String>,
    /// Possible values for enum-type flags.
    pub enum_values: Option<Vec<String>>,
    /// Whether the flag can be specified multiple times.
    pub repeatable: bool,
    /// Placeholder name for the value (e.g., "FILE", "NUM").
    pub value_name: Option<String>,
    /// Whether this flag can make the command run until it is killed.
    ///
    /// The claim is "may not terminate on its own", not "always blocks":
    /// `tail -f` follows a regular file indefinitely, yet the BSD man page
    /// states it returns immediately when its input is a pipe. An executor
    /// should read this as "bound the timeout or refuse", never as a
    /// prediction that the process will hang.
    ///
    /// Only curated overlays populate this; no help or man page format states
    /// it machine-readably. Defaulted on deserialize so caches written before
    /// the field existed still load.
    #[serde(default)]
    pub long_running: bool,
    /// Flags that cannot be combined with this one (e.g. `-l` vs `-1`).
    ///
    /// Only curated overlays populate this: no help or man page format states
    /// mutual exclusion in a machine-readable way.
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    /// Every source that contributed this flag definition, in discovery order.
    #[serde(default)]
    pub sources: Vec<FlagSource>,
    /// Trust level derived from [`ScannedFlag::sources`].
    #[serde(default)]
    pub confidence: Confidence,
}

impl ScannedFlag {
    /// Return the preferred flag name for use as a schema property key.
    /// Long form preferred, stripped of leading dashes, hyphens replaced with underscores.
    pub fn canonical_name(&self) -> String {
        if let Some(ref long) = self.long_name {
            long.trim_start_matches('-').replace('-', "_")
        } else if let Some(ref short) = self.short_name {
            short.trim_start_matches('-').to_string()
        } else {
            "unknown".to_string()
        }
    }

    /// Record `source` as backing this flag and recompute its confidence.
    ///
    /// Re-adding a source already present is a no-op, so repeated enrichment
    /// passes cannot inflate a single source into apparent corroboration.
    pub fn add_source(&mut self, source: FlagSource) {
        if !self.sources.contains(&source) {
            self.sources.push(source);
        }
        self.confidence = Confidence::from_sources(&self.sources);
    }
}

/// A positional argument parsed from help output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedArg {
    /// Argument name (e.g., "FILE", "PATH").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Inferred type.
    pub value_type: ValueType,
    /// Whether the argument is required.
    pub required: bool,
    /// Whether the argument accepts multiple values.
    pub variadic: bool,
}

/// Information about a CLI tool's structured output capability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuredOutputInfo {
    /// Whether the tool supports structured output.
    pub supported: bool,
    /// The flag to enable structured output (e.g., "--format json").
    pub flag: Option<String>,
    /// The output format (e.g., "json", "csv", "xml").
    pub format: Option<String>,
}

/// A single CLI command or subcommand with its parsed metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedCommand {
    /// Command name (e.g., "commit").
    pub name: String,
    /// Full command path (e.g., "git commit").
    pub full_command: String,
    /// Human-readable description.
    pub description: String,
    /// Parsed flags/options.
    pub flags: Vec<ScannedFlag>,
    /// Parsed positional arguments.
    pub positional_args: Vec<ScannedArg>,
    /// Nested subcommands.
    pub subcommands: Vec<ScannedCommand>,
    /// Example invocations from help text.
    pub examples: Vec<String>,
    /// Detected format of the help output.
    pub help_format: HelpFormat,
    /// Structured output capability info.
    pub structured_output: StructuredOutputInfo,
    /// Original help text (preserved for debugging).
    pub raw_help: String,
}

/// Complete scan result for a single CLI tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScannedCLITool {
    /// Tool binary name (e.g., "git").
    pub name: String,
    /// What the tool does, usually taken from its man page DESCRIPTION.
    ///
    /// Defaulted on deserialize so scan caches written before this field
    /// existed still load.
    #[serde(default)]
    pub description: String,
    /// Absolute path to the binary.
    pub binary_path: String,
    /// Version string from --version, or None.
    pub version: Option<String>,
    /// Which implementation of the command this binary is.
    ///
    /// A command name does not identify a program: `/bin/ls` on macOS is BSD,
    /// `/usr/bin/ls` on a GNU/Linux box is coreutils, and Homebrew can put GNU
    /// `ls` on a macOS box. Determined by probe, not by path.
    #[serde(default)]
    pub variant: ToolVariant,
    /// Identifier of the curated overlay applied to this scan, if any.
    #[serde(default)]
    pub overlay: Option<String>,
    /// Tree of discovered commands.
    pub subcommands: Vec<ScannedCommand>,
    /// Flags available on all subcommands.
    pub global_flags: Vec<ScannedFlag>,
    /// Positional arguments the tool itself accepts (`ls [file ...]`).
    ///
    /// Distinct from a subcommand's positional args; only meaningful for tools
    /// invoked directly rather than through a subcommand.
    #[serde(default)]
    pub positional_args: Vec<ScannedArg>,
    /// Global structured output capability.
    pub structured_output: StructuredOutputInfo,
    /// Behavioral annotations asserted by an overlay, overriding inference.
    #[serde(default)]
    pub annotation_overrides: AnnotationOverrides,
    /// Highest tier used during scanning (1=help, 2=man, 3=completion).
    pub scan_tier: u32,
    /// Non-fatal issues encountered during scanning.
    pub warnings: Vec<String>,
}

/// Which implementation of a command a binary is.
///
/// `Unknown` is the honest default: it means the probe was inconclusive, not
/// that the tool is unusual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolVariant {
    /// BSD userland (macOS, FreeBSD).
    Bsd,
    /// The GNU project's build, whichever package ships it.
    ///
    /// Deliberately the whole family rather than one package: `coreutils`,
    /// `diffutils`, `tar`, `sed`, `grep` and `bash` are separate projects with
    /// separate release lines, and a variant per package would scale badly
    /// while telling an overlay author nothing it cannot state more precisely
    /// in `match.probe.output_contains` and `provenance.package`.
    Gnu,
    /// Apple's own port, whose banner names Apple rather than BSD.
    ///
    /// macOS `sort` reports `2.3-Apple (197)` and `git` reports
    /// `git version 2.50.1 (Apple Git-155)`. Neither is classifiable as BSD
    /// from its banner, and calling them GNU would be wrong.
    Apple,
    /// BusyBox multi-call binary (Alpine and most embedded images).
    Busybox,
    /// Probe was inconclusive.
    #[default]
    Unknown,
}

impl ToolVariant {
    /// Stable lowercase token used in cache keys and overlay identifiers.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolVariant::Bsd => "bsd",
            ToolVariant::Gnu => "gnu",
            ToolVariant::Apple => "apple",
            ToolVariant::Busybox => "busybox",
            ToolVariant::Unknown => "unknown",
        }
    }
}

/// Behavioral annotation assertions that override heuristic inference.
///
/// Every field is optional: an overlay states only what it actually knows, and
/// an unset field leaves the inferred value alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_approval: Option<bool>,
}

impl AnnotationOverrides {
    /// Whether any assertion is present.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // T1: ValueType serde round-trip
    #[test]
    fn test_value_type_serde_round_trip() {
        let types = vec![
            (ValueType::String, "\"string\""),
            (ValueType::Integer, "\"integer\""),
            (ValueType::Float, "\"float\""),
            (ValueType::Boolean, "\"boolean\""),
            (ValueType::Path, "\"path\""),
            (ValueType::Enum, "\"enum\""),
            (ValueType::Url, "\"url\""),
            (ValueType::Unknown, "\"unknown\""),
        ];
        for (variant, expected_json) in types {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "Serialize {variant:?}");
            let back: ValueType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "Deserialize {variant:?}");
        }
    }

    #[test]
    fn test_value_type_debug_clone() {
        let v = ValueType::Boolean;
        let cloned = v;
        assert_eq!(format!("{:?}", cloned), "Boolean");
    }

    // T1: HelpFormat serde round-trip
    #[test]
    fn test_help_format_serde_round_trip() {
        let formats = vec![
            (HelpFormat::Gnu, "\"gnu\""),
            (HelpFormat::Click, "\"click\""),
            (HelpFormat::Argparse, "\"argparse\""),
            (HelpFormat::Cobra, "\"cobra\""),
            (HelpFormat::Clap, "\"clap\""),
            (HelpFormat::Unknown, "\"unknown\""),
        ];
        for (variant, expected_json) in formats {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json, "Serialize {variant:?}");
            let back: HelpFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant, "Deserialize {variant:?}");
        }
    }

    // T2: ScannedFlag canonical_name tests
    #[test]
    fn test_confidence_from_sources_ranks_overlay_highest() {
        assert_eq!(
            Confidence::from_sources(&[FlagSource::Help, FlagSource::Overlay]),
            Confidence::Verified
        );
        assert_eq!(
            Confidence::from_sources(&[FlagSource::Help, FlagSource::Completion]),
            Confidence::High
        );
        assert_eq!(
            Confidence::from_sources(&[FlagSource::Help, FlagSource::ManPage]),
            Confidence::Medium
        );
        assert_eq!(
            Confidence::from_sources(&[FlagSource::Help]),
            Confidence::Low
        );
        assert_eq!(Confidence::from_sources(&[]), Confidence::Low);
    }

    #[test]
    fn test_confidence_is_ordered_worst_to_best() {
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
        assert!(Confidence::High < Confidence::Verified);
    }

    #[test]
    fn test_add_source_is_idempotent() {
        // Re-running an enrichment pass must not inflate one source into
        // apparent corroboration.
        let mut flag = ScannedFlag::default();
        flag.add_source(FlagSource::Help);
        flag.add_source(FlagSource::Help);
        assert_eq!(flag.sources, vec![FlagSource::Help]);
        assert_eq!(flag.confidence, Confidence::Low);

        flag.add_source(FlagSource::ManPage);
        assert_eq!(flag.confidence, Confidence::Medium);
    }

    #[test]
    fn test_tool_variant_tokens_are_stable() {
        // These strings appear in cache keys and overlay ids, so they are API.
        assert_eq!(ToolVariant::Bsd.as_str(), "bsd");
        assert_eq!(ToolVariant::Gnu.as_str(), "gnu");
        assert_eq!(ToolVariant::Apple.as_str(), "apple");
        assert_eq!(ToolVariant::Busybox.as_str(), "busybox");
        assert_eq!(ToolVariant::Unknown.as_str(), "unknown");
        assert_eq!(ToolVariant::default(), ToolVariant::Unknown);
    }

    #[test]
    fn test_tool_variant_serde_matches_tokens() {
        for variant in [
            ToolVariant::Bsd,
            ToolVariant::Gnu,
            ToolVariant::Apple,
            ToolVariant::Busybox,
            ToolVariant::Unknown,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{}\"", variant.as_str()));
            let back: ToolVariant = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn test_annotation_overrides_empty_by_default() {
        assert!(AnnotationOverrides::default().is_empty());
        assert!(!AnnotationOverrides {
            readonly: Some(true),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn test_scanned_flag_provenance_defaults_on_legacy_json() {
        // Scan caches written before provenance existed must still load.
        let legacy = r#"{"long_name":"--all","short_name":"-a","description":"",
            "value_type":"boolean","required":false,"default":null,
            "enum_values":null,"repeatable":false,"value_name":null}"#;
        let flag: ScannedFlag = serde_json::from_str(legacy).unwrap();
        assert!(flag.sources.is_empty());
        assert!(flag.conflicts_with.is_empty());
        assert_eq!(flag.confidence, Confidence::Low);
        assert!(!flag.long_running);
    }

    #[test]
    fn test_canonical_name_long() {
        let flag = ScannedFlag {
            long_name: Some("--message".into()),
            short_name: Some("-m".into()),
            description: String::new(),
            value_type: ValueType::String,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        };
        assert_eq!(flag.canonical_name(), "message");
    }

    #[test]
    fn test_canonical_name_dry_run() {
        let flag = ScannedFlag {
            long_name: Some("--dry-run".into()),
            short_name: None,
            description: String::new(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        };
        assert_eq!(flag.canonical_name(), "dry_run");
    }

    #[test]
    fn test_canonical_name_short_only() {
        let flag = ScannedFlag {
            long_name: None,
            short_name: Some("-m".into()),
            description: String::new(),
            value_type: ValueType::String,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        };
        assert_eq!(flag.canonical_name(), "m");
    }

    #[test]
    fn test_canonical_name_neither() {
        let flag = ScannedFlag {
            long_name: None,
            short_name: None,
            description: String::new(),
            value_type: ValueType::Unknown,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        };
        assert_eq!(flag.canonical_name(), "unknown");
    }

    // T2: ScannedFlag serde round-trip
    #[test]
    fn test_scanned_flag_serde_all_fields() {
        let flag = ScannedFlag {
            long_name: Some("--format".into()),
            short_name: Some("-f".into()),
            description: "Output format".into(),
            value_type: ValueType::Enum,
            required: true,
            default: Some("json".into()),
            enum_values: Some(vec!["json".into(), "text".into()]),
            repeatable: false,
            value_name: Some("FMT".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&flag).unwrap();
        let back: ScannedFlag = serde_json::from_str(&json).unwrap();
        assert_eq!(back.long_name, flag.long_name);
        assert_eq!(back.short_name, flag.short_name);
        assert_eq!(back.value_type, flag.value_type);
        assert_eq!(back.required, flag.required);
        assert_eq!(back.enum_values, flag.enum_values);
    }

    #[test]
    fn test_scanned_flag_serde_optional_none() {
        let flag = ScannedFlag {
            long_name: None,
            short_name: Some("-v".into()),
            description: "Verbose".into(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&flag).unwrap();
        let back: ScannedFlag = serde_json::from_str(&json).unwrap();
        assert!(back.long_name.is_none());
        assert!(back.default.is_none());
        assert!(back.enum_values.is_none());
    }

    // T3: ScannedArg serde round-trip
    #[test]
    fn test_scanned_arg_serde_round_trip() {
        let arg = ScannedArg {
            name: "file".into(),
            description: "Input file".into(),
            value_type: ValueType::Path,
            required: true,
            variadic: false,
        };
        let json = serde_json::to_string(&arg).unwrap();
        let back: ScannedArg = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "file");
        assert_eq!(back.value_type, ValueType::Path);
        assert!(back.required);
        assert!(!back.variadic);
    }

    #[test]
    fn test_scanned_arg_variadic() {
        let arg = ScannedArg {
            name: "files".into(),
            description: "Input files".into(),
            value_type: ValueType::Path,
            required: false,
            variadic: true,
        };
        let json = serde_json::to_string(&arg).unwrap();
        let back: ScannedArg = serde_json::from_str(&json).unwrap();
        assert!(back.variadic);
    }

    // T4: StructuredOutputInfo default and serde
    #[test]
    fn test_structured_output_info_default() {
        let info = StructuredOutputInfo::default();
        assert!(!info.supported);
        assert!(info.flag.is_none());
        assert!(info.format.is_none());
    }

    #[test]
    fn test_structured_output_info_serde() {
        let info = StructuredOutputInfo {
            supported: true,
            flag: Some("--json".into()),
            format: Some("json".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: StructuredOutputInfo = serde_json::from_str(&json).unwrap();
        assert!(back.supported);
        assert_eq!(back.flag.as_deref(), Some("--json"));
        assert_eq!(back.format.as_deref(), Some("json"));
    }

    // T5: ScannedCommand serde with nested subcommands
    #[test]
    fn test_scanned_command_nested_serde() {
        let inner = ScannedCommand {
            name: "ls".into(),
            full_command: "docker container ls".into(),
            description: "List containers".into(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Cobra,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        };
        let mid = ScannedCommand {
            name: "container".into(),
            full_command: "docker container".into(),
            description: "Manage containers".into(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![inner],
            examples: vec![],
            help_format: HelpFormat::Cobra,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        };
        let json = serde_json::to_string(&mid).unwrap();
        let back: ScannedCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "container");
        assert_eq!(back.subcommands.len(), 1);
        assert_eq!(back.subcommands[0].name, "ls");
    }

    #[test]
    fn test_scanned_command_empty_subcommands() {
        let cmd = ScannedCommand {
            name: "status".into(),
            full_command: "git status".into(),
            description: "Show status".into(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: ScannedCommand = serde_json::from_str(&json).unwrap();
        assert!(back.subcommands.is_empty());
    }

    // T6: ScannedCLITool serde
    #[test]
    fn test_scanned_cli_tool_serde_round_trip() {
        let tool = ScannedCLITool {
            name: "git".into(),
            description: String::new(),
            binary_path: "/usr/bin/git".into(),
            version: Some("2.43.0".into()),
            subcommands: vec![ScannedCommand {
                name: "commit".into(),
                full_command: "git commit".into(),
                description: "Record changes".into(),
                flags: vec![ScannedFlag {
                    long_name: Some("--message".into()),
                    short_name: Some("-m".into()),
                    description: "Commit message".into(),
                    value_type: ValueType::String,
                    required: false,
                    default: None,
                    enum_values: None,
                    repeatable: false,
                    value_name: Some("MSG".into()),
                    ..Default::default()
                }],
                positional_args: vec![],
                subcommands: vec![],
                examples: vec![],
                help_format: HelpFormat::Gnu,
                structured_output: StructuredOutputInfo::default(),
                raw_help: String::new(),
            }],
            global_flags: vec![ScannedFlag {
                long_name: Some("--version".into()),
                short_name: None,
                description: "Print version".into(),
                value_type: ValueType::Boolean,
                required: false,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: None,
                ..Default::default()
            }],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec!["some warning".into()],
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&tool).unwrap();
        let back: ScannedCLITool = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "git");
        assert_eq!(back.version, Some("2.43.0".into()));
        assert_eq!(back.subcommands.len(), 1);
        assert_eq!(back.global_flags.len(), 1);
        assert_eq!(back.warnings, vec!["some warning"]);
    }

    #[test]
    fn test_scanned_cli_tool_version_none() {
        let tool = ScannedCLITool {
            name: "mytool".into(),
            description: String::new(),
            binary_path: "/usr/local/bin/mytool".into(),
            version: None,
            subcommands: vec![],
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
            ..Default::default()
        };
        let json = serde_json::to_string(&tool).unwrap();
        let back: ScannedCLITool = serde_json::from_str(&json).unwrap();
        assert!(back.version.is_none());
    }
}
