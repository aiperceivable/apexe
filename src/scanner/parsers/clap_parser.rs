use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

/// Parser for Rust Clap-style help output.
///
/// Handles Rust tools like ripgrep, fd, bat:
/// - 'Usage: tool [OPTIONS] [ARGS]' header
/// - 'Options:' section with '  -f, --flag <VALUE>  Description'
/// - 'SUBCOMMANDS:' section (uppercase) or 'Commands:' in newer clap
pub struct ClapHelpParser;

impl CliParser for ClapHelpParser {
    fn name(&self) -> &str {
        "clap"
    }

    fn priority(&self) -> u32 {
        130
    }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        help_text.contains("SUBCOMMANDS:")
            || (help_text.contains('<')
                && help_text.contains('>')
                && help_text.contains("Options:"))
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let description = extract_clap_description(help_text);
        let flags = extract_clap_flags(help_text);
        let positional_args = extract_clap_args(help_text);
        let subcommand_names = extract_clap_subcommands(help_text);
        let structured_output =
            super::structured_output::StructuredOutputDetector.detect(&flags, help_text);

        Ok(ParsedHelp {
            description,
            flags,
            positional_args,
            subcommand_names,
            examples: vec![],
            structured_output,
        })
    }
}

fn extract_clap_description(help_text: &str) -> String {
    let mut desc_lines = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:")
            || trimmed.starts_with("Options:")
            || trimmed.starts_with("SUBCOMMANDS:")
            || trimmed.starts_with("Commands:")
        {
            break;
        }
        if !trimmed.is_empty() {
            desc_lines.push(trimmed);
        }
    }
    let desc = desc_lines.join(" ");
    desc.chars().take(200).collect()
}

fn extract_clap_flags(help_text: &str) -> Vec<ScannedFlag> {
    // Clap format: "  -f, --flag <VALUE>  Description"
    // Or: "      --flag <VALUE>  Description"
    let flag_re =
        Regex::new(r"(?m)^\s{2,}(-([a-zA-Z]),?\s+)?(--([a-z][\w-]*))(?:\s+<([^>]+)>)?\s{2,}(.+)")
            .unwrap();

    let default_re = Regex::new(r"\[default:\s*([^\]]+)\]").unwrap();
    let enum_re = Regex::new(r"\[possible values:\s*([^\]]+)\]").unwrap();

    let mut flags = Vec::new();

    for cap in flag_re.captures_iter(help_text) {
        let short_name = cap.get(2).map(|m| format!("-{}", m.as_str()));
        let long_name = Some(format!("--{}", &cap[4]));
        let value_name = cap.get(5).map(|m| m.as_str().to_string());
        let description = cap
            .get(6)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();

        let value_type = match value_name.as_deref() {
            None => ValueType::Boolean,
            Some("FILE" | "PATH" | "DIR" | "DIRECTORY") => ValueType::Path,
            Some("NUM" | "NUMBER" | "COUNT" | "N") => ValueType::Integer,
            Some("URL" | "URI") => ValueType::Url,
            _ => ValueType::String,
        };

        let default = default_re
            .captures(&description)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string());

        let enum_values = enum_re
            .captures(&description)
            .and_then(|c| c.get(1))
            .map(|m| {
                m.as_str()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            });

        let required = description.to_lowercase().contains("required");
        let repeatable = description.contains("...");

        let actual_type = if enum_values.is_some() {
            ValueType::Enum
        } else {
            value_type
        };

        flags.push(ScannedFlag {
            long_name,
            short_name,
            description,
            value_type: actual_type,
            required,
            default,
            enum_values,
            repeatable,
            value_name,
        });
    }

    flags
}

fn extract_clap_args(help_text: &str) -> Vec<ScannedArg> {
    let arg_re = Regex::new(r"<([a-zA-Z_][\w-]*)>(\.\.\.)?").unwrap();
    let mut args = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") {
            for cap in arg_re.captures_iter(trimmed) {
                let name = cap[1].to_string();
                let variadic = cap.get(2).is_some();
                args.push(ScannedArg {
                    name,
                    description: String::new(),
                    value_type: ValueType::String,
                    required: true,
                    variadic,
                });
            }
        }
    }

    args
}

fn extract_clap_subcommands(help_text: &str) -> Vec<String> {
    let section_re = Regex::new(r"(?mi)^(SUBCOMMANDS|Commands):").unwrap();
    let cmd_re = Regex::new(r"(?m)^\s{2,}([a-z][\w-]*)\s+\S").unwrap();
    let mut names = Vec::new();

    if let Some(section_match) = section_re.find(help_text) {
        let after_section = &help_text[section_match.end()..];
        for line in after_section.lines() {
            if line.trim().is_empty() || (!line.starts_with(' ') && !line.is_empty()) {
                if !names.is_empty() {
                    break;
                }
                continue;
            }
            if let Some(cap) = cmd_re.captures(line) {
                names.push(cap[1].to_string());
            }
        }
    }

    names
}

// Structured output detection delegated to shared StructuredOutputDetector

#[cfg(test)]
mod tests {
    use super::*;

    const CLAP_HELP: &str = r#"ripgrep 14.1.0
Andrew Gallant <jamslam@gmail.com>
Recursively search the current directory for lines matching a pattern.

Usage: rg [OPTIONS] <PATTERN> [PATH]...

Options:
  -e, --regexp <PATTERN>  A pattern to search for
  -t, --type <TYPE>       Only search files matching TYPE
  -g, --glob <GLOB>       Include or exclude files
      --json              Show results in JSON format
  -c, --count             Show count of matching lines
  -h, --help              Print help information
  -V, --version           Print version information

SUBCOMMANDS:
  pcre2   Use PCRE2 regex engine
  help    Print this message or the help of the given subcommand
"#;

    #[test]
    fn test_clap_can_parse_subcommands() {
        let parser = ClapHelpParser;
        assert!(parser.can_parse(CLAP_HELP, "rg"));
    }

    #[test]
    fn test_clap_can_parse_angle_brackets() {
        let parser = ClapHelpParser;
        let help = "Usage: tool [OPTIONS] <FILE>\n\nOptions:\n  -v, --verbose  Verbose\n";
        assert!(parser.can_parse(help, "tool"));
    }

    #[test]
    fn test_clap_parse_subcommands() {
        let parser = ClapHelpParser;
        let result = parser.parse(CLAP_HELP, "rg").unwrap();
        assert!(result.subcommand_names.contains(&"pcre2".to_string()));
        assert!(result.subcommand_names.contains(&"help".to_string()));
    }

    #[test]
    fn test_clap_parse_flags_with_value() {
        let parser = ClapHelpParser;
        let result = parser.parse(CLAP_HELP, "rg").unwrap();
        let regexp = result
            .flags
            .iter()
            .find(|f| f.long_name.as_deref() == Some("--regexp"));
        assert!(regexp.is_some());
        let regexp = regexp.unwrap();
        assert_eq!(regexp.short_name.as_deref(), Some("-e"));
        assert_eq!(regexp.value_type, ValueType::String);
        assert_eq!(regexp.value_name.as_deref(), Some("PATTERN"));
    }

    #[test]
    fn test_clap_parse_boolean_flag() {
        let parser = ClapHelpParser;
        let result = parser.parse(CLAP_HELP, "rg").unwrap();
        let count = result
            .flags
            .iter()
            .find(|f| f.long_name.as_deref() == Some("--count"));
        assert!(count.is_some());
        assert_eq!(count.unwrap().value_type, ValueType::Boolean);
    }

    #[test]
    fn test_clap_structured_output() {
        let parser = ClapHelpParser;
        let result = parser.parse(CLAP_HELP, "rg").unwrap();
        assert!(result.structured_output.supported);
        assert_eq!(result.structured_output.flag.as_deref(), Some("--json"));
    }

    #[test]
    fn test_clap_description() {
        let parser = ClapHelpParser;
        let result = parser.parse(CLAP_HELP, "rg").unwrap();
        assert!(result.description.contains("ripgrep"));
    }

    #[test]
    fn test_clap_positional_args() {
        let parser = ClapHelpParser;
        let result = parser.parse(CLAP_HELP, "rg").unwrap();
        assert!(result.positional_args.iter().any(|a| a.name == "PATTERN"));
    }

    #[test]
    fn test_clap_possible_values() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  -f, --format <FMT>  Output format [possible values: json, yaml, toml]\n";
        let flags = extract_clap_flags(help);
        let fmt = flags
            .iter()
            .find(|f| f.long_name.as_deref() == Some("--format"));
        assert!(fmt.is_some());
        let fmt = fmt.unwrap();
        assert_eq!(fmt.value_type, ValueType::Enum);
        assert_eq!(
            fmt.enum_values,
            Some(vec!["json".into(), "yaml".into(), "toml".into()])
        );
    }
}
