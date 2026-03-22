use serde::{Deserialize, Serialize};

/// Type classification for CLI flag/argument values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Integer,
    Float,
    Boolean,
    Path,
    Enum,
    Url,
    Unknown,
}

/// Detected help output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelpFormat {
    Gnu,
    Click,
    Argparse,
    Cobra,
    Clap,
    Unknown,
}

/// A single CLI flag parsed from help output.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedCLITool {
    /// Tool binary name (e.g., "git").
    pub name: String,
    /// Absolute path to the binary.
    pub binary_path: String,
    /// Version string from --version, or None.
    pub version: Option<String>,
    /// Tree of discovered commands.
    pub subcommands: Vec<ScannedCommand>,
    /// Flags available on all subcommands.
    pub global_flags: Vec<ScannedFlag>,
    /// Global structured output capability.
    pub structured_output: StructuredOutputInfo,
    /// Highest tier used during scanning (1=help, 2=man, 3=completion).
    pub scan_tier: u32,
    /// Non-fatal issues encountered during scanning.
    pub warnings: Vec<String>,
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
            }],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec!["some warning".into()],
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
            binary_path: "/usr/local/bin/mytool".into(),
            version: None,
            subcommands: vec![],
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
        };
        let json = serde_json::to_string(&tool).unwrap();
        let back: ScannedCLITool = serde_json::from_str(&json).unwrap();
        assert!(back.version.is_none());
    }
}
