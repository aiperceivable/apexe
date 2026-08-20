use std::sync::LazyLock;

use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

// Precompiled once: the scan hot path parses help per subcommand recursively, so
// recompiling these on every call is wasteful. INVARIANT: every pattern is a
// compile-time constant valid regex, so `Regex::new` never fails.
static AVAILABLE_COMMANDS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Available Commands:").expect("valid static regex"));
static CMD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s{2,}([a-z][\w-]*)\s+\S").expect("valid static regex"));
static FLAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^\s{2,}(-([a-zA-Z]),?\s+)?(--([a-z][\w-]*))\s+(string|int|uint|float|bool|duration|stringSlice|stringArray)?\s*(.+)",
    )
    .expect("valid static regex")
});
static DEFAULT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(default\s+([^)]+)\)").expect("valid static regex"));

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
            help_format: crate::models::HelpFormat::Cobra,
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
    let mut names = Vec::new();

    if let Some(section_match) = AVAILABLE_COMMANDS_RE.find(help_text) {
        let after_section = &help_text[section_match.end()..];
        for line in after_section.lines() {
            if line.trim().is_empty() || (!line.starts_with(' ') && !line.is_empty()) {
                if !names.is_empty() {
                    break;
                }
                continue;
            }
            if let Some(cap) = CMD_RE.captures(line) {
                names.push(cap[1].to_string());
            }
        }
    }

    names
}

/// Slice out the text under `section_header`, up to the next unindented line.
///
/// The section header is caller-supplied ("Flags:" / "Global Flags:"), so this
/// small anchor regex stays dynamic; the expensive flag regex is shared.
/// Returns `None` when the tool prints no such section.
fn cobra_section_content<'a>(help_text: &'a str, section_header: &str) -> Option<&'a str> {
    // INVARIANT: regex::escape neutralises every metacharacter in the header,
    // so the anchored pattern always compiles. The file-header note covers the
    // compile-time-constant patterns and does not reach this dynamic one.
    let section_re = Regex::new(&format!(r"(?m)^{}$", regex::escape(section_header)))
        .expect("an escaped header is always a valid regex");
    let section_start = section_re.find(help_text)?.end();
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

    Some(&section_text[..section_end])
}

/// Map Cobra's own type words onto apexe's value types.
///
/// A flag with no type word is a boolean — that is how Cobra prints a switch.
/// Anything unrecognised is treated as a string, which is the safe direction:
/// it takes a value, and apexe does not have to know its shape to pass it on.
fn cobra_value_type(type_str: Option<&str>) -> ValueType {
    match type_str {
        Some("string") | Some("stringSlice") | Some("stringArray") => ValueType::String,
        Some("int") | Some("uint") => ValueType::Integer,
        Some("float") => ValueType::Float,
        Some("bool") => ValueType::Boolean,
        Some("duration") => ValueType::String,
        None => ValueType::Boolean,
        _ => ValueType::String,
    }
}

/// Extract flags from one Cobra help section.
///
/// Cobra prints `  -f, --flag type   Description`, or `      --flag type
/// Description` for an option with no short form.
fn extract_cobra_flags(help_text: &str, section_header: &str) -> Vec<ScannedFlag> {
    let Some(section_content) = cobra_section_content(help_text, section_header) else {
        return Vec::new();
    };

    let mut flags = Vec::new();
    for cap in FLAG_RE.captures_iter(section_content) {
        let type_str = cap.get(5).map(|m| m.as_str());
        let description = cap
            .get(6)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let default = DEFAULT_RE
            .captures(&description)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().trim_matches('"').to_string());

        flags.push(ScannedFlag {
            long_name: Some(format!("--{}", &cap[4])),
            short_name: cap.get(2).map(|m| format!("-{}", m.as_str())),
            description,
            value_type: cobra_value_type(type_str),
            required: false,
            default,
            enum_values: None,
            repeatable: false,
            value_name: type_str.map(|s| s.to_string()),
            ..Default::default()
        });
    }
    flags
}

fn extract_cobra_args(help_text: &str) -> Vec<ScannedArg> {
    let mut args = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") {
            // Skip the "Usage:" prefix line, check the next indented line
            continue;
        }
        // Cobra usage lines are often indented: "  tool [command] <arg>"
        if help_text.contains("Usage:") {
            args.extend(super::positional_args::extract_args_from_usage_line(
                trimmed,
            ));
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
