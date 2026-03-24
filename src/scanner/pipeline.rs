use std::path::Path;

use tracing::{info, warn};

use super::protocol::{CliParser, ParsedHelp};

/// Selects the best parser for a given help text using priority-based routing.
pub struct ParserPipeline {
    parsers: Vec<Box<dyn CliParser>>,
}

impl ParserPipeline {
    /// Initialize with built-in parsers and optional plugins.
    ///
    /// If `plugins` is None, only built-in parsers are loaded.
    pub fn new(plugins: Option<Vec<Box<dyn CliParser>>>) -> Self {
        let mut parsers: Vec<Box<dyn CliParser>> = vec![
            Box::new(super::parsers::gnu::GnuHelpParser),
            Box::new(super::parsers::click::ClickHelpParser),
            Box::new(super::parsers::cobra::CobraHelpParser),
            Box::new(super::parsers::clap_parser::ClapHelpParser),
        ];

        // Add plugin parsers
        if let Some(ext_parsers) = plugins {
            parsers.extend(ext_parsers);
        }

        parsers.sort_by_key(|p| p.priority());

        Self { parsers }
    }

    /// Parse help text using the highest-priority matching parser.
    ///
    /// Priority resolution:
    /// 1. If `user_override` path exists, load YAML and convert to ParsedHelp.
    /// 2. Try each parser in priority order (ascending).
    /// 3. Fallback: return ParsedHelp with raw text as description.
    pub fn parse(
        &self,
        help_text: &str,
        tool_name: &str,
        user_override: Option<&Path>,
    ) -> ParsedHelp {
        // Check for user override
        if let Some(path) = user_override {
            if path.exists() {
                if let Ok(contents) = std::fs::read_to_string(path) {
                    if let Ok(parsed) = serde_yaml::from_str::<ParsedHelp>(&contents) {
                        info!(path = %path.display(), "Using user override");
                        return parsed;
                    }
                }
            }
        }

        // Try parsers in priority order
        for parser in &self.parsers {
            if parser.can_parse(help_text, tool_name) {
                match parser.parse(help_text, tool_name) {
                    Ok(result) => {
                        info!(parser = parser.name(), "Parsed help text");
                        return result;
                    }
                    Err(e) => {
                        warn!(parser = parser.name(), "Parser failed, trying next: {e}");
                    }
                }
            }
        }

        // Fallback
        warn!(tool = tool_name, "No parser matched, using raw help text");
        ParsedHelp {
            description: help_text.chars().take(500).collect(),
            ..Default::default()
        }
    }

    /// Return the number of registered parsers.
    pub fn parser_count(&self) -> usize {
        self.parsers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::protocol::CliParser;

    struct TestParser {
        parser_name: &'static str,
        prio: u32,
        can: bool,
        fail: bool,
    }

    impl CliParser for TestParser {
        fn name(&self) -> &str {
            self.parser_name
        }
        fn priority(&self) -> u32 {
            self.prio
        }
        fn can_parse(&self, _help_text: &str, _tool_name: &str) -> bool {
            self.can
        }
        fn parse(&self, _help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
            if self.fail {
                Err(anyhow::anyhow!("intentional failure"))
            } else {
                Ok(ParsedHelp {
                    description: format!("parsed by {}", self.parser_name),
                    ..Default::default()
                })
            }
        }
    }

    // T25: Pipeline initialization
    #[test]
    fn test_pipeline_new_builtin_count() {
        let pipeline = ParserPipeline::new(None);
        assert_eq!(pipeline.parser_count(), 4);
    }

    #[test]
    fn test_pipeline_new_with_plugin() {
        let plugin: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "plugin",
            prio: 200,
            can: true,
            fail: false,
        });
        let pipeline = ParserPipeline::new(Some(vec![plugin]));
        assert_eq!(pipeline.parser_count(), 5);
    }

    #[test]
    fn test_pipeline_parsers_sorted_by_priority() {
        let p1: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "low",
            prio: 50,
            can: true,
            fail: false,
        });
        let p2: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "high",
            prio: 300,
            can: true,
            fail: false,
        });
        let pipeline = ParserPipeline::new(Some(vec![p2, p1]));
        // first parser should have lowest priority number
        assert_eq!(pipeline.parsers[0].priority(), 50);
    }

    // T26: Pipeline parse routing
    #[test]
    fn test_pipeline_uses_gnu_for_gnu_help() {
        let pipeline = ParserPipeline::new(None);
        let gnu_help = "Description\n\nUsage: tool [OPTIONS]\n\nOptions:\n  -v, --verbose          Be verbose\n";
        let result = pipeline.parse(gnu_help, "tool", None);
        assert!(!result.description.is_empty());
    }

    #[test]
    fn test_pipeline_uses_cobra_for_cobra_help() {
        let pipeline = ParserPipeline::new(None);
        let cobra_help =
            "A tool\n\nAvailable Commands:\n  sub1  First\n\nFlags:\n  --verbose  Be verbose\n";
        let result = pipeline.parse(cobra_help, "tool", None);
        assert!(result.subcommand_names.contains(&"sub1".to_string()));
    }

    #[test]
    fn test_pipeline_fallback_on_no_match() {
        let pipeline = ParserPipeline::new(None);
        let result = pipeline.parse("random gibberish text", "tool", None);
        assert_eq!(result.description, "random gibberish text");
        assert!(result.flags.is_empty());
    }

    #[test]
    fn test_pipeline_tries_next_on_parse_failure() {
        let p1: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "failing",
            prio: 10,
            can: true,
            fail: true,
        });
        let p2: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "working",
            prio: 20,
            can: true,
            fail: false,
        });
        let pipeline = ParserPipeline::new(Some(vec![p1, p2]));
        let result = pipeline.parse("any text", "tool", None);
        assert_eq!(result.description, "parsed by working");
    }

    // T27: User override
    #[test]
    fn test_pipeline_user_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let override_path = tmp.path().join("override.yaml");
        let yaml_content = r#"
description: "overridden description"
flags: []
positional_args: []
subcommand_names:
  - custom_sub
examples: []
structured_output:
  supported: false
  flag: null
  format: null
"#;
        std::fs::write(&override_path, yaml_content).unwrap();

        let pipeline = ParserPipeline::new(None);
        let result = pipeline.parse("ignored help text", "tool", Some(&override_path));
        assert_eq!(result.description, "overridden description");
        assert!(result.subcommand_names.contains(&"custom_sub".to_string()));
    }

    #[test]
    fn test_pipeline_override_missing_file_falls_through() {
        let pipeline = ParserPipeline::new(None);
        let missing = std::path::PathBuf::from("/tmp/nonexistent_override.yaml");
        let result = pipeline.parse("random text", "tool", Some(&missing));
        // Should fall through to regular parsing (fallback since no parser matches)
        assert!(!result.description.is_empty());
    }

    #[test]
    fn test_pipeline_override_invalid_yaml_falls_through() {
        let tmp = tempfile::TempDir::new().unwrap();
        let override_path = tmp.path().join("bad.yaml");
        std::fs::write(&override_path, "not: [valid: yaml: for ParsedHelp").unwrap();

        let pipeline = ParserPipeline::new(None);
        let result = pipeline.parse("random text", "tool", Some(&override_path));
        assert!(!result.description.is_empty());
    }
}
