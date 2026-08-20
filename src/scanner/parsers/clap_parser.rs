use std::sync::LazyLock;

use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

// Precompiled once (parsers run per subcommand on the recursive scan hot path).
// INVARIANT: every pattern is a compile-time constant valid regex.
static FLAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s{2,}(-([a-zA-Z]),?\s+)?(--([a-z][\w-]*))(?:\s+<([^>]+)>)?\s{2,}(.+)")
        .expect("valid static regex")
});
static DEFAULT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[default:\s*([^\]]+)\]").expect("valid static regex"));
static ENUM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[possible values:\s*([^\]]+)\]").expect("valid static regex"));
static SUBCMD_SECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?mi)^(SUBCOMMANDS|Commands):").expect("valid static regex"));
static CMD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s{2,}([a-z][\w-]*)\s+\S").expect("valid static regex"));

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
            help_format: crate::models::HelpFormat::Clap,
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

/// Build the flag one `-f, --flag <VALUE>  Description` line describes.
///
/// Clap states the default, the choice list, whether the option is required and
/// whether it repeats inside the description prose rather than in the signature,
/// so all four are recovered from `description`.
fn clap_flag(
    short_name: Option<String>,
    long_name: Option<String>,
    value_name: Option<String>,
    description: String,
) -> ScannedFlag {
    let default = DEFAULT_RE
        .captures(&description)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());

    let enum_values = ENUM_RE
        .captures(&description)
        .and_then(|c| c.get(1))
        .map(|m| {
            m.as_str()
                .split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        });

    // A choice list is the stronger statement: it names the accepted values,
    // where the placeholder only names their shape.
    let value_type = if enum_values.is_some() {
        ValueType::Enum
    } else {
        // One table for the whole scanner; see `scanner::value_placeholder`.
        crate::scanner::value_placeholder::flag_value_type(value_name.as_deref())
    };

    ScannedFlag {
        long_name,
        short_name,
        required: description.to_lowercase().contains("required"),
        repeatable: description.contains("..."),
        description,
        value_type,
        default,
        enum_values,
        value_name,
        ..Default::default()
    }
}

/// Extract flags from a Clap-style help text.
fn extract_clap_flags(help_text: &str) -> Vec<ScannedFlag> {
    FLAG_RE
        .captures_iter(help_text)
        .map(|cap| {
            clap_flag(
                cap.get(2).map(|m| format!("-{}", m.as_str())),
                Some(format!("--{}", &cap[4])),
                cap.get(5).map(|m| m.as_str().to_string()),
                cap.get(6)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

fn extract_clap_args(help_text: &str) -> Vec<ScannedArg> {
    let mut args = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") {
            args.extend(super::positional_args::extract_args_from_usage_line(
                trimmed,
            ));
        }
    }

    args
}

fn extract_clap_subcommands(help_text: &str) -> Vec<String> {
    let mut names = Vec::new();

    if let Some(section_match) = SUBCMD_SECTION_RE.find(help_text) {
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
