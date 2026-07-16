use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, StructuredOutputInfo, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

/// Parser for GNU-style --help output.
///
/// Handles tools like git, grep, curl, wget that follow GNU conventions:
/// - 'Usage: tool [OPTION]...' header
/// - Options formatted as '  -f, --flag=VALUE  Description'
/// - Sections separated by blank lines
pub struct GnuHelpParser;

impl CliParser for GnuHelpParser {
    fn name(&self) -> &str {
        "gnu"
    }

    fn priority(&self) -> u32 {
        100
    }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        if help_text.trim().is_empty() {
            return false;
        }
        let has_usage = help_text.contains("Usage:") || help_text.contains("usage:");
        let has_gnu_opts = Regex::new(r"(?m)^\s{1,}-\w,?\s+--\w")
            .map(|re| re.is_match(help_text))
            .unwrap_or(false);
        let not_cobra = !help_text.contains("Available Commands:");
        let not_clap = !help_text.contains("SUBCOMMANDS:");
        has_usage && (has_gnu_opts || !help_text.contains("Commands:")) && not_cobra && not_clap
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let description = extract_description(help_text);
        let flags = extract_flags(help_text);
        let positional_args = extract_positional_args(help_text);
        let subcommand_names = extract_subcommands(help_text);
        let examples = extract_examples(help_text);
        let structured_output = detect_structured_output(&flags, help_text);

        Ok(ParsedHelp {
            description,
            flags,
            positional_args,
            subcommand_names,
            examples,
            structured_output,
            help_format: crate::models::HelpFormat::Gnu,
        })
    }
}

/// Extract description from first paragraph before Usage/Options.
pub fn extract_description(help_text: &str) -> String {
    let mut desc_lines = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:")
            || trimmed.starts_with("usage:")
            || trimmed.starts_with("Options:")
            || trimmed.starts_with("options:")
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

/// Extract flags from OPTIONS section using regex patterns.
pub fn extract_flags(help_text: &str) -> Vec<ScannedFlag> {
    // Match flags like:
    //   -m, --message=MSG   Use the given message
    //   -m, --message MSG   Use the given message
    //   --all               Stage all
    //   -v                  Verbose
    let flag_re = Regex::new(
        r"(?m)^\s{1,}(-([a-zA-Z0-9]),?\s+)?(--([a-z][\w-]*))((?:[=\s])([A-Z_]+|<[^>]+>))?\s{2,}(.+)"
    ).unwrap();

    // Also match short-only flags: " -v  Verbose"
    let short_only_re = Regex::new(
        r"(?m)^\s{1,}-([a-zA-Z0-9])(?:,?\s+--[\w-]+)?(?:\s([A-Z_]+|<[^>]+>))?\s{2,}(.+)",
    )
    .unwrap();

    let default_re = Regex::new(r"\[default:\s*([^\]]+)\]").unwrap();
    let enum_re = Regex::new(r"\{([^}]+)\}").unwrap();

    let mut flags = Vec::new();
    let mut seen_long: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_short: std::collections::HashSet<String> = std::collections::HashSet::new();

    for cap in flag_re.captures_iter(help_text) {
        let short_name = cap.get(2).map(|m| format!("-{}", m.as_str()));
        let long_name = cap.get(4).map(|m| format!("--{}", m.as_str()));
        let value_name = cap
            .get(6)
            .map(|m| m.as_str().trim_matches('<').trim_matches('>').to_string());
        let description = cap
            .get(7)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();

        if let Some(ref ln) = long_name {
            if !seen_long.insert(ln.clone()) {
                continue;
            }
        }
        if let Some(ref sn) = short_name {
            seen_short.insert(sn.clone());
        }

        let flag = build_flag(
            long_name,
            short_name,
            description,
            value_name,
            &default_re,
            &enum_re,
        );
        flags.push(flag);
    }

    // Collect short-only flags not already captured
    for cap in short_only_re.captures_iter(help_text) {
        let short_char = cap[1].to_string();
        let short_name = format!("-{short_char}");
        if seen_short.contains(&short_name) {
            continue;
        }
        // Check that this line doesn't also have a long flag (already captured above)
        let full_match = cap.get(0).unwrap().as_str();
        if full_match.contains("--") {
            continue;
        }

        let value_name = cap.get(2).map(|m| m.as_str().to_string());
        let description = cap
            .get(3)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();

        seen_short.insert(short_name.clone());
        let flag = build_flag(
            None,
            Some(short_name),
            description,
            value_name,
            &default_re,
            &enum_re,
        );
        flags.push(flag);
    }

    flags
}

fn build_flag(
    long_name: Option<String>,
    short_name: Option<String>,
    description: String,
    value_name: Option<String>,
    default_re: &Regex,
    enum_re: &Regex,
) -> ScannedFlag {
    let value_type = match value_name.as_deref() {
        None => ValueType::Boolean,
        Some("FILE" | "PATH" | "DIR" | "DIRECTORY" | "FILENAME") => ValueType::Path,
        Some("NUM" | "NUMBER" | "COUNT" | "N" | "PORT" | "INT") => ValueType::Integer,
        Some("FLOAT" | "DECIMAL") => ValueType::Float,
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
    let repeatable = description.contains("can be repeated") || description.contains("...");

    let actual_type = if enum_values.is_some() {
        ValueType::Enum
    } else {
        value_type
    };

    ScannedFlag {
        long_name,
        short_name,
        description,
        value_type: actual_type,
        required,
        default,
        enum_values,
        repeatable,
        value_name,
    }
}

/// Extract positional arguments from Usage line.
pub fn extract_positional_args(help_text: &str) -> Vec<ScannedArg> {
    let arg_re = Regex::new(r"<([a-zA-Z_][\w-]*)>(\.\.\.)?").unwrap();
    let mut args = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") || trimmed.starts_with("usage:") {
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

/// Extract subcommand names from commands section.
pub fn extract_subcommands(help_text: &str) -> Vec<String> {
    let section_re = Regex::new(r"(?mi)^(commands|subcommands|available commands):").unwrap();
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

/// Extract example invocations from help text.
pub fn extract_examples(help_text: &str) -> Vec<String> {
    let example_re = Regex::new(r"(?mi)^(examples?|usage examples?):").unwrap();
    let mut examples = Vec::new();

    if let Some(m) = example_re.find(help_text) {
        for line in help_text[m.end()..].lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if !examples.is_empty() {
                    break;
                }
                continue;
            }
            if trimmed.starts_with('$') || trimmed.starts_with('#') {
                examples.push(trimmed.to_string());
            }
        }
    }

    examples
}

/// Detect structured output flags from parsed flags and help text.
pub fn detect_structured_output(flags: &[ScannedFlag], help_text: &str) -> StructuredOutputInfo {
    // Check parsed flags first
    for flag in flags {
        let long = flag.long_name.as_deref().unwrap_or("");
        if matches!(long, "--format" | "--output-format" | "--output") {
            if let Some(ref enums) = flag.enum_values {
                if enums.iter().any(|v| v == "json") {
                    return StructuredOutputInfo {
                        supported: true,
                        flag: Some(format!("{long} json")),
                        format: Some("json".to_string()),
                    };
                }
            }
        }
        if long == "--json" {
            return StructuredOutputInfo {
                supported: true,
                flag: Some("--json".to_string()),
                format: Some("json".to_string()),
            };
        }
    }

    // Regex fallback on help text
    let json_re = Regex::new(r"--json\b").unwrap();
    if json_re.is_match(help_text) {
        return StructuredOutputInfo {
            supported: true,
            flag: Some("--json".to_string()),
            format: Some("json".to_string()),
        };
    }

    StructuredOutputInfo::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // T13: GnuHelpParser can_parse
    #[test]
    fn test_can_parse_gnu_help() {
        let parser = GnuHelpParser;
        let gnu_help = "git commit - Record changes\n\nUsage: git commit [OPTIONS]\n\nOptions:\n  -m, --message MSG  Use the given message\n  -a, --all          Stage all\n";
        assert!(parser.can_parse(gnu_help, "git"));
    }

    #[test]
    fn test_can_parse_rejects_cobra() {
        let parser = GnuHelpParser;
        let cobra_help =
            "Usage:\n  kubectl [command]\n\nAvailable Commands:\n  apply  Apply config\n";
        assert!(!parser.can_parse(cobra_help, "kubectl"));
    }

    #[test]
    fn test_can_parse_rejects_clap() {
        let parser = GnuHelpParser;
        let clap_help = "rg 1.0\n\nSUBCOMMANDS:\n  search  Search files\n";
        assert!(!parser.can_parse(clap_help, "rg"));
    }

    #[test]
    fn test_can_parse_rejects_empty() {
        let parser = GnuHelpParser;
        assert!(!parser.can_parse("", "tool"));
    }

    // T14: extract_description
    #[test]
    fn test_extract_description_before_usage() {
        let help = "git commit - Record changes to the repository\n\nUsage: git commit [OPTIONS]\n";
        let desc = extract_description(help);
        assert_eq!(desc, "git commit - Record changes to the repository");
    }

    #[test]
    fn test_extract_description_starts_with_usage() {
        let help = "Usage: git commit [OPTIONS]\n\nOptions:\n  -m MSG  Message\n";
        let desc = extract_description(help);
        assert!(desc.is_empty());
    }

    #[test]
    fn test_extract_description_truncated() {
        let long_desc = "A".repeat(300);
        let help = format!("{long_desc}\nUsage: tool [OPTIONS]\n");
        let desc = extract_description(&help);
        assert_eq!(desc.len(), 200);
    }

    // T15: extract_flags basic
    #[test]
    fn test_extract_flags_short_and_long() {
        let help = "Options:\n  -m, --message MSG  Use the given message\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].short_name.as_deref(), Some("-m"));
        assert_eq!(flags[0].long_name.as_deref(), Some("--message"));
        assert_eq!(flags[0].value_type, ValueType::String);
    }

    #[test]
    fn test_extract_flags_boolean() {
        let help = "Options:\n  -a, --all          Stage all files\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].value_type, ValueType::Boolean);
    }

    #[test]
    fn test_extract_flags_path_type() {
        let help = "Options:\n      --config FILE  Config path\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].value_type, ValueType::Path);
    }

    #[test]
    fn test_extract_flags_integer_type() {
        let help = "Options:\n  -n, --count NUM    Number of items\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].value_type, ValueType::Integer);
    }

    // T16: enum, default, required, repeatable detection
    #[test]
    fn test_extract_flags_enum_values() {
        let help = "Options:\n  -f, --format FMT   Output format {json,text,csv}\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].value_type, ValueType::Enum);
        assert_eq!(
            flags[0].enum_values,
            Some(vec!["json".into(), "text".into(), "csv".into()])
        );
    }

    #[test]
    fn test_extract_flags_default_value() {
        let help = "Options:\n      --width NUM    Line width [default: 80]\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].default.as_deref(), Some("80"));
    }

    #[test]
    fn test_extract_flags_required() {
        let help = "Options:\n      --name MSG     Your name (required)\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].required);
    }

    #[test]
    fn test_extract_flags_repeatable() {
        let help = "Options:\n      --include PATTERN  Include pattern (can be repeated)\n";
        let flags = extract_flags(help);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].repeatable);
    }

    // T17: extract_positional_args
    #[test]
    fn test_extract_args_single() {
        let help = "Usage: tool <file>\n";
        let args = extract_positional_args(help);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "file");
        assert!(args[0].required);
        assert!(!args[0].variadic);
    }

    #[test]
    fn test_extract_args_two() {
        let help = "Usage: tool <src> <dst>\n";
        let args = extract_positional_args(help);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "src");
        assert_eq!(args[1].name, "dst");
    }

    #[test]
    fn test_extract_args_variadic() {
        let help = "Usage: tool <file>...\n";
        let args = extract_positional_args(help);
        assert_eq!(args.len(), 1);
        assert!(args[0].variadic);
    }

    #[test]
    fn test_extract_args_options_only() {
        let help = "Usage: tool [OPTIONS]\n";
        let args = extract_positional_args(help);
        assert!(args.is_empty());
    }

    // T18: extract_subcommands
    #[test]
    fn test_extract_subcommands() {
        let help =
            "Commands:\n  commit  Record changes\n  push    Upload changes\n\nSome other section\n";
        let subs = extract_subcommands(help);
        assert_eq!(subs, vec!["commit", "push"]);
    }

    #[test]
    fn test_extract_subcommands_none() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  -v  Verbose\n";
        let subs = extract_subcommands(help);
        assert!(subs.is_empty());
    }

    #[test]
    fn test_extract_subcommands_ends_at_blank() {
        let help = "Subcommands:\n  sub1  First\n  sub2  Second\n\nMore text\n";
        let subs = extract_subcommands(help);
        assert_eq!(subs, vec!["sub1", "sub2"]);
    }

    // T19: extract_examples
    #[test]
    fn test_extract_examples() {
        let help = "Examples:\n  $ git commit -m \"msg\"\n  $ git commit --amend\n\nSee also:\n";
        let examples = extract_examples(help);
        assert_eq!(examples.len(), 2);
        assert!(examples[0].starts_with('$'));
    }

    #[test]
    fn test_extract_examples_none() {
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  -v  Verbose\n";
        let examples = extract_examples(help);
        assert!(examples.is_empty());
    }

    #[test]
    fn test_extract_examples_hash_prefix() {
        let help = "Examples:\n  # Run the tool\n  $ tool run\n\n";
        let examples = extract_examples(help);
        assert_eq!(examples.len(), 2);
        assert!(examples[0].starts_with('#'));
    }

    // T20: full parse integration
    #[test]
    fn test_full_parse_gnu() {
        let help = r#"git commit - Record changes to the repository

Usage: git commit [OPTIONS] <file>...

Options:
  -m, --message MSG   Use the given message (required)
  -a, --all           Stage all modified files
      --amend         Amend the previous commit
      --format FMT    Output format {json,text}

Examples:
  $ git commit -m "fix bug"
  $ git commit --amend
"#;
        let parser = GnuHelpParser;
        assert!(parser.can_parse(help, "git"));
        let result = parser.parse(help, "git").unwrap();

        assert_eq!(
            result.description,
            "git commit - Record changes to the repository"
        );
        assert!(result.flags.len() >= 3);
        assert_eq!(result.positional_args.len(), 1);
        assert!(result.positional_args[0].variadic);
        assert_eq!(result.examples.len(), 2);

        // Check structured output detection
        assert!(result.structured_output.supported);
    }

    // Structured output detection tests
    #[test]
    fn test_detect_structured_output_format_enum() {
        let flags = vec![ScannedFlag {
            long_name: Some("--format".into()),
            short_name: None,
            description: "Output format".into(),
            value_type: ValueType::Enum,
            required: false,
            default: None,
            enum_values: Some(vec!["json".into(), "text".into()]),
            repeatable: false,
            value_name: None,
        }];
        let info = detect_structured_output(&flags, "");
        assert!(info.supported);
        assert_eq!(info.flag.as_deref(), Some("--format json"));
    }

    #[test]
    fn test_detect_structured_output_json_flag() {
        let flags = vec![ScannedFlag {
            long_name: Some("--json".into()),
            short_name: None,
            description: "JSON output".into(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
        }];
        let info = detect_structured_output(&flags, "");
        assert!(info.supported);
        assert_eq!(info.flag.as_deref(), Some("--json"));
    }

    #[test]
    fn test_detect_structured_output_none() {
        let flags = vec![ScannedFlag {
            long_name: Some("--verbose".into()),
            short_name: Some("-v".into()),
            description: "Be verbose".into(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
        }];
        let info = detect_structured_output(&flags, "some help text");
        assert!(!info.supported);
    }

    #[test]
    fn test_detect_structured_output_regex_fallback() {
        let flags: Vec<ScannedFlag> = vec![];
        let info = detect_structured_output(&flags, "Use --json for JSON output");
        assert!(info.supported);
    }

    #[test]
    fn test_extract_flags_curl_1space_indent() {
        let help = r#"Usage: curl [options...] <url>
 -d, --data <data>           HTTP POST data
 -f, --fail                  Fail fast with no output on HTTP errors
 -o, --output <file>         Write to file instead of stdout
 -v, --verbose               Make the operation more talkative
"#;
        let flags = extract_flags(help);
        assert!(
            flags.len() >= 4,
            "Expected >= 4 flags from curl-style help, got {}",
            flags.len()
        );
        let names: Vec<String> = flags.iter().filter_map(|f| f.long_name.clone()).collect();
        assert!(names.contains(&"--data".to_string()));
        assert!(names.contains(&"--fail".to_string()));
        assert!(names.contains(&"--output".to_string()));
        assert!(names.contains(&"--verbose".to_string()));
    }
}
