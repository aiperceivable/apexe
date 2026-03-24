use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

/// Parser for Go Cobra-style help output.
///
/// Handles Go tools like kubectl, docker, gh:
/// - Description paragraph first
/// - 'Usage:\n  tool [command]' format
/// - 'Available Commands:' section
/// - 'Flags:' section with '  -f, --flag type   Description'
pub struct CobraHelpParser;

impl CliParser for CobraHelpParser {
    fn name(&self) -> &str {
        "cobra"
    }

    fn priority(&self) -> u32 {
        120
    }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        help_text.contains("Available Commands:")
            || (help_text.contains("Flags:") && !help_text.contains("Options:"))
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let description = extract_cobra_description(help_text);
        let subcommand_names = extract_available_commands(help_text);
        let mut flags = extract_cobra_flags(help_text, "Flags:");
        let global_flags = extract_cobra_flags(help_text, "Global Flags:");
        flags.extend(global_flags);
        let positional_args = extract_cobra_args(help_text);
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

fn extract_cobra_description(help_text: &str) -> String {
    let mut desc_lines = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:")
            || trimmed.starts_with("Available Commands:")
            || trimmed.starts_with("Flags:")
            || trimmed.starts_with("Global Flags:")
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

fn extract_available_commands(help_text: &str) -> Vec<String> {
    let section_re = Regex::new(r"(?m)^Available Commands:").unwrap();
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

fn extract_cobra_flags(help_text: &str, section_header: &str) -> Vec<ScannedFlag> {
    // Cobra format: "  -f, --flag type   Description"
    // Or: "      --flag type   Description"
    let section_re = Regex::new(&format!(r"(?m)^{}$", regex::escape(section_header))).unwrap();

    let flag_re = Regex::new(
        r"(?m)^\s{2,}(-([a-zA-Z]),?\s+)?(--([a-z][\w-]*))\s+(string|int|uint|float|bool|duration|stringSlice|stringArray)?\s*(.+)"
    ).unwrap();

    let mut flags = Vec::new();

    let section_start = match section_re.find(help_text) {
        Some(m) => m.end(),
        None => return flags,
    };

    let section_text = &help_text[section_start..];
    let section_end = section_text
        .lines()
        .skip(1) // skip the blank line after header
        .position(|line| !line.starts_with(' ') && !line.trim().is_empty())
        .map(|pos| {
            section_text
                .lines()
                .skip(1)
                .take(pos)
                .map(|l| l.len() + 1)
                .sum::<usize>()
                + section_text
                    .lines()
                    .next()
                    .map(|l| l.len() + 1)
                    .unwrap_or(0)
        })
        .unwrap_or(section_text.len());

    let section_content = &section_text[..section_end];
    let default_re = Regex::new(r"\(default\s+([^)]+)\)").unwrap();

    for cap in flag_re.captures_iter(section_content) {
        let short_name = cap.get(2).map(|m| format!("-{}", m.as_str()));
        let long_name = Some(format!("--{}", &cap[4]));
        let type_str = cap.get(5).map(|m| m.as_str());
        let description = cap
            .get(6)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();

        let value_type = match type_str {
            Some("string") | Some("stringSlice") | Some("stringArray") => ValueType::String,
            Some("int") | Some("uint") => ValueType::Integer,
            Some("float") => ValueType::Float,
            Some("bool") => ValueType::Boolean,
            Some("duration") => ValueType::String,
            None => ValueType::Boolean,
            _ => ValueType::String,
        };

        let default = default_re
            .captures(&description)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().trim_matches('"').to_string());

        let value_name = type_str.map(|s| s.to_string());

        flags.push(ScannedFlag {
            long_name,
            short_name,
            description,
            value_type,
            required: false,
            default,
            enum_values: None,
            repeatable: false,
            value_name,
        });
    }

    flags
}

fn extract_cobra_args(help_text: &str) -> Vec<ScannedArg> {
    let arg_re = Regex::new(r"<([a-zA-Z_][\w-]*)>(\.\.\.)?").unwrap();
    let mut args = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") {
            // Skip the "Usage:" prefix line, check the next indented line
            continue;
        }
        // Cobra usage lines are often indented: "  tool [command] <arg>"
        if help_text.contains("Usage:") {
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
            if !args.is_empty() {
                break;
            }
        }
    }

    args
}

// Structured output detection delegated to shared StructuredOutputDetector

#[cfg(test)]
mod tests {
    use super::*;

    const COBRA_HELP: &str = r#"kubectl controls the Kubernetes cluster manager.

Usage:
  kubectl [command]

Available Commands:
  apply       Apply a configuration to a resource
  get         Display one or many resources
  describe    Show details of a specific resource
  delete      Delete resources
  logs        Print the logs for a container

Flags:
  -n, --namespace string   If present, the namespace scope
      --context string     The name of the kubeconfig context
  -h, --help               help for kubectl

Global Flags:
      --kubeconfig string   Path to the kubeconfig file (default "~/.kube/config")
      --output string       Output format. One of: json|yaml|wide
"#;

    #[test]
    fn test_cobra_can_parse() {
        let parser = CobraHelpParser;
        assert!(parser.can_parse(COBRA_HELP, "kubectl"));
    }

    #[test]
    fn test_cobra_rejects_gnu() {
        let parser = CobraHelpParser;
        let gnu = "Usage: tool [OPTIONS]\n\nOptions:\n  -v  Verbose\n";
        assert!(!parser.can_parse(gnu, "tool"));
    }

    #[test]
    fn test_cobra_parse_available_commands() {
        let parser = CobraHelpParser;
        let result = parser.parse(COBRA_HELP, "kubectl").unwrap();
        assert!(result.subcommand_names.contains(&"apply".to_string()));
        assert!(result.subcommand_names.contains(&"get".to_string()));
        assert!(result.subcommand_names.contains(&"describe".to_string()));
        assert!(result.subcommand_names.len() >= 5);
    }

    #[test]
    fn test_cobra_parse_flags_with_type() {
        let parser = CobraHelpParser;
        let result = parser.parse(COBRA_HELP, "kubectl").unwrap();
        let ns_flag = result
            .flags
            .iter()
            .find(|f| f.long_name.as_deref() == Some("--namespace"));
        assert!(ns_flag.is_some());
        let ns_flag = ns_flag.unwrap();
        assert_eq!(ns_flag.short_name.as_deref(), Some("-n"));
        assert_eq!(ns_flag.value_type, ValueType::String);
    }

    #[test]
    fn test_cobra_parse_global_flags() {
        let parser = CobraHelpParser;
        let result = parser.parse(COBRA_HELP, "kubectl").unwrap();
        let kubeconfig = result
            .flags
            .iter()
            .find(|f| f.long_name.as_deref() == Some("--kubeconfig"));
        assert!(kubeconfig.is_some());
        let kubeconfig = kubeconfig.unwrap();
        assert_eq!(kubeconfig.default.as_deref(), Some("~/.kube/config"));
    }

    #[test]
    fn test_cobra_description() {
        let parser = CobraHelpParser;
        let result = parser.parse(COBRA_HELP, "kubectl").unwrap();
        assert!(result.description.contains("Kubernetes"));
    }

    #[test]
    fn test_cobra_structured_output_detection() {
        let parser = CobraHelpParser;
        let result = parser.parse(COBRA_HELP, "kubectl").unwrap();
        assert!(result.structured_output.supported);
    }
}
