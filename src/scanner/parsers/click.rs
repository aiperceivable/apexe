use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

/// Parser for Click/argparse-style help output.
///
/// Handles tools using Python Click or argparse:
/// - 'Usage: tool [OPTIONS] COMMAND [ARGS]...' header
/// - Options section with '  --flag TEXT  Description'
/// - Commands section with '  command  Description'
pub struct ClickHelpParser;

impl CliParser for ClickHelpParser {
    fn name(&self) -> &str {
        "click"
    }

    fn priority(&self) -> u32 {
        110
    }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        help_text.contains("[OPTIONS]")
            && help_text.contains("Options:")
            && !help_text.contains("Available Commands:")
            && !help_text.contains("SUBCOMMANDS:")
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let description = extract_click_description(help_text);
        let flags = extract_click_flags(help_text);
        let positional_args = extract_click_args(help_text);
        let subcommand_names = extract_click_subcommands(help_text);
        let structured_output = super::structured_output::StructuredOutputDetector.detect(&flags, help_text);

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

fn extract_click_description(help_text: &str) -> String {
    let mut desc_lines = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") || trimmed.starts_with("Options:") {
            break;
        }
        if !trimmed.is_empty() {
            desc_lines.push(trimmed);
        }
    }
    let desc = desc_lines.join(" ");
    desc.chars().take(200).collect()
}

fn extract_click_flags(help_text: &str) -> Vec<ScannedFlag> {
    // Click format: "  --flag TEXT  Description"
    // Or: "  -f, --flag TEXT  Description"
    // Also: "  --flag / --no-flag  Description" for boolean toggles
    let flag_re = Regex::new(
        r"(?m)^\s{2,}(-([a-zA-Z]),?\s+)?(--([a-z][\w-]*))\s+(?:\s*/\s*--no-[\w-]+\s+)?(TEXT|INTEGER|FLOAT|PATH|FILENAME|DIRECTORY|<[^>]+>)?\s*(.+)"
    ).unwrap();

    // Separate pattern for --flag/--no-flag boolean toggles
    let toggle_re = Regex::new(
        r"(?m)^\s{2,}(--([a-z][\w-]*))\s*/\s*(--no-[\w-]+)\s{2,}(.+)"
    ).unwrap();

    let default_re = Regex::new(r"\[default:\s*([^\]]+)\]").unwrap();
    let enum_re = Regex::new(r"\[([a-zA-Z0-9_]+(?:\|[a-zA-Z0-9_]+)+)\]").unwrap();
    let required_re = Regex::new(r"\[required\]").unwrap();

    let mut flags = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Handle toggle flags first
    for cap in toggle_re.captures_iter(help_text) {
        let long_name = format!("--{}", &cap[2]);
        if !seen.insert(long_name.clone()) {
            continue;
        }
        let description = cap[4].trim().to_string();
        flags.push(ScannedFlag {
            long_name: Some(long_name),
            short_name: None,
            description,
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
        });
    }

    for cap in flag_re.captures_iter(help_text) {
        let short_name = cap.get(2).map(|m| format!("-{}", m.as_str()));
        let long_name = Some(format!("--{}", &cap[4]));
        let type_str = cap.get(5).map(|m| m.as_str());
        let description = cap.get(6).map(|m| m.as_str().trim().to_string()).unwrap_or_default();

        if let Some(ref ln) = long_name {
            if !seen.insert(ln.clone()) {
                continue;
            }
        }

        let value_type = match type_str {
            Some("TEXT") | Some("text") => ValueType::String,
            Some("INTEGER") | Some("integer") | Some("INT") => ValueType::Integer,
            Some("FLOAT") | Some("float") => ValueType::Float,
            Some("PATH") | Some("FILENAME") | Some("DIRECTORY") => ValueType::Path,
            None => ValueType::Boolean,
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
                    .split('|')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            });

        let required = required_re.is_match(&description);

        let actual_type = if enum_values.is_some() {
            ValueType::Enum
        } else {
            value_type
        };

        let value_name = type_str.map(|s| s.trim_matches('<').trim_matches('>').to_string());

        flags.push(ScannedFlag {
            long_name,
            short_name,
            description,
            value_type: actual_type,
            required,
            default,
            enum_values,
            repeatable: false,
            value_name,
        });
    }

    flags
}

fn extract_click_args(help_text: &str) -> Vec<ScannedArg> {
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

fn extract_click_subcommands(help_text: &str) -> Vec<String> {
    let section_re = Regex::new(r"(?mi)^Commands:").unwrap();
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

    const CLICK_HELP: &str = r#"Usage: flask [OPTIONS] COMMAND [ARGS]...

  A general utility script for Flask applications.

Options:
  --version          Show the flask version.
  -e, --env TEXT     The environment to use. [default: production]
  --debug / --no-debug
                     Enable debug mode.
  --help             Show this message and exit.

Commands:
  routes  Show the routes for the app.
  run     Run a development server.
  shell   Run a shell in the app context.
"#;

    #[test]
    fn test_click_can_parse() {
        let parser = ClickHelpParser;
        assert!(parser.can_parse(CLICK_HELP, "flask"));
    }

    #[test]
    fn test_click_rejects_cobra() {
        let parser = ClickHelpParser;
        let cobra = "Available Commands:\n  apply  Apply\n\nOptions:\n  [OPTIONS]\n";
        assert!(!parser.can_parse(cobra, "tool"));
    }

    #[test]
    fn test_click_parse_subcommands() {
        let parser = ClickHelpParser;
        let result = parser.parse(CLICK_HELP, "flask").unwrap();
        assert!(result.subcommand_names.contains(&"routes".to_string()));
        assert!(result.subcommand_names.contains(&"run".to_string()));
        assert!(result.subcommand_names.contains(&"shell".to_string()));
    }

    #[test]
    fn test_click_parse_flags_text_type() {
        let parser = ClickHelpParser;
        let result = parser.parse(CLICK_HELP, "flask").unwrap();
        let env_flag = result.flags.iter().find(|f| f.long_name.as_deref() == Some("--env"));
        assert!(env_flag.is_some());
        let env_flag = env_flag.unwrap();
        assert_eq!(env_flag.value_type, ValueType::String);
        assert_eq!(env_flag.default.as_deref(), Some("production"));
    }

    #[test]
    fn test_click_parse_toggle_flag() {
        let parser = ClickHelpParser;
        let result = parser.parse(CLICK_HELP, "flask").unwrap();
        let debug_flag = result.flags.iter().find(|f| f.long_name.as_deref() == Some("--debug"));
        assert!(debug_flag.is_some());
        assert_eq!(debug_flag.unwrap().value_type, ValueType::Boolean);
    }

    #[test]
    fn test_click_integer_type() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  --count INTEGER  Number of items\n";
        let flags = extract_click_flags(help);
        let count = flags.iter().find(|f| f.long_name.as_deref() == Some("--count"));
        assert!(count.is_some());
        assert_eq!(count.unwrap().value_type, ValueType::Integer);
    }

    #[test]
    fn test_click_path_type() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  --input PATH  Input file\n";
        let flags = extract_click_flags(help);
        let input = flags.iter().find(|f| f.long_name.as_deref() == Some("--input"));
        assert!(input.is_some());
        assert_eq!(input.unwrap().value_type, ValueType::Path);
    }

    #[test]
    fn test_click_required_detection() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  --name TEXT  Your name [required]\n";
        let flags = extract_click_flags(help);
        let name = flags.iter().find(|f| f.long_name.as_deref() == Some("--name"));
        assert!(name.is_some());
        assert!(name.unwrap().required);
    }

    #[test]
    fn test_click_enum_detection() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  --format TEXT  Output format [json|text|csv]\n";
        let flags = extract_click_flags(help);
        let fmt = flags.iter().find(|f| f.long_name.as_deref() == Some("--format"));
        assert!(fmt.is_some());
        let fmt = fmt.unwrap();
        assert_eq!(fmt.value_type, ValueType::Enum);
        assert_eq!(
            fmt.enum_values,
            Some(vec!["json".into(), "text".into(), "csv".into()])
        );
    }
}
