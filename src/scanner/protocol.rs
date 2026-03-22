use serde::{Deserialize, Serialize};

use crate::models::{ScannedArg, ScannedFlag, StructuredOutputInfo};

/// Result of parsing a single help text block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedHelp {
    /// Extracted command description.
    pub description: String,
    /// Parsed flag definitions.
    pub flags: Vec<ScannedFlag>,
    /// Parsed positional arguments.
    pub positional_args: Vec<ScannedArg>,
    /// Discovered subcommand names (not yet recursively scanned).
    pub subcommand_names: Vec<String>,
    /// Example invocations from help text.
    pub examples: Vec<String>,
    /// Structured output capability.
    pub structured_output: StructuredOutputInfo,
}

/// Trait for CLI help format parser plugins.
///
/// Implementations are discovered via shared library loading
/// from `~/.apexe/plugins/` or registered programmatically.
pub trait CliParser: Send + Sync {
    /// Human-readable parser name.
    fn name(&self) -> &str;

    /// Parser priority (lower = higher priority).
    /// Built-in: 100-199. Plugin: 200-299. User override: 0-99.
    fn priority(&self) -> u32;

    /// Return true if this parser can handle the given help text.
    fn can_parse(&self, help_text: &str, tool_name: &str) -> bool;

    /// Parse help text into structured metadata.
    fn parse(&self, help_text: &str, tool_name: &str) -> anyhow::Result<ParsedHelp>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // T7: ParsedHelp Default
    #[test]
    fn test_parsed_help_default() {
        let ph = ParsedHelp::default();
        assert!(ph.description.is_empty());
        assert!(ph.flags.is_empty());
        assert!(ph.positional_args.is_empty());
        assert!(ph.subcommand_names.is_empty());
        assert!(ph.examples.is_empty());
        assert!(!ph.structured_output.supported);
        assert!(ph.structured_output.flag.is_none());
        assert!(ph.structured_output.format.is_none());
    }

    #[test]
    fn test_parsed_help_serde_round_trip() {
        let ph = ParsedHelp {
            description: "Some tool".into(),
            flags: vec![ScannedFlag {
                long_name: Some("--verbose".into()),
                short_name: Some("-v".into()),
                description: "Be verbose".into(),
                value_type: crate::models::ValueType::Boolean,
                required: false,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: None,
            }],
            positional_args: vec![ScannedArg {
                name: "file".into(),
                description: "Input file".into(),
                value_type: crate::models::ValueType::Path,
                required: true,
                variadic: false,
            }],
            subcommand_names: vec!["sub1".into()],
            examples: vec!["$ tool --verbose file.txt".into()],
            structured_output: StructuredOutputInfo {
                supported: true,
                flag: Some("--json".into()),
                format: Some("json".into()),
            },
        };
        let json = serde_json::to_string(&ph).unwrap();
        let back: ParsedHelp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.description, "Some tool");
        assert_eq!(back.flags.len(), 1);
        assert_eq!(back.positional_args.len(), 1);
        assert_eq!(back.subcommand_names, vec!["sub1"]);
        assert!(back.structured_output.supported);
    }

    // T8: CliParser trait - mock implementation
    struct MockParser {
        can: bool,
    }

    impl CliParser for MockParser {
        fn name(&self) -> &str {
            "mock"
        }
        fn priority(&self) -> u32 {
            50
        }
        fn can_parse(&self, _help_text: &str, _tool_name: &str) -> bool {
            self.can
        }
        fn parse(&self, _help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
            Ok(ParsedHelp {
                description: "mocked".into(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn test_mock_parser_implements_trait() {
        let parser = MockParser { can: true };
        assert_eq!(parser.name(), "mock");
        assert_eq!(parser.priority(), 50);
        assert!(parser.can_parse("anything", "tool"));
        let result = parser.parse("anything", "tool").unwrap();
        assert_eq!(result.description, "mocked");
    }

    #[test]
    fn test_cli_parser_object_safe() {
        let parser: Box<dyn CliParser> = Box::new(MockParser { can: false });
        assert_eq!(parser.name(), "mock");
        assert!(!parser.can_parse("text", "tool"));
    }
}
