use std::sync::LazyLock;

use regex::Regex;

use crate::models::{HelpFormat, ScannedArg, ScannedFlag, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

// INVARIANT: every pattern is a compile-time constant valid regex.

/// A bundled short-option group, e.g. `[-belnstuv]` or `[-@ABC1%,]`.
///
/// Requires at least three option characters and no whitespace, so grouped
/// alternatives like `[-v | --version]` and valued options like `[-C <path>]`
/// do not match.
static BUNDLED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[-([A-Za-z0-9@%,]{3,})\]").expect("valid static regex"));

/// A short option taking a value, e.g. `[-D format]` or `[-o FILE]`.
static SHORT_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[-([A-Za-z0-9])\s+([A-Za-z_][\w-]*)\]").expect("valid static regex")
});

/// A long option, optionally with an inline value: `[--color=when]`, `[--all]`.
static LONG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(--[a-z][\w-]*)(?:=([A-Za-z_][\w-]*))?\]").expect("valid static regex")
});

/// A positional operand, e.g. `[file ...]` or `file ...`.
static OPERAND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([a-z][\w-]*)\s*\.\.\.\]").expect("valid static regex"));

/// Error lines emitted by a tool that rejects `--help`; never a description.
const ERROR_MARKERS: &[&str] = &[
    "unrecognized option",
    "illegal option",
    "invalid option",
    "unknown option",
    "unrecognised option",
];

/// Parser for BSD-style single-line usage output.
///
/// Most BSD/macOS built-ins (`ls`, `cp`, `cat`, `mv`) reject `--help` and print
/// only a bundled usage line:
///
/// ```text
/// usage: ls [-@ABCFGHILOPRSTUWXabcdefghiklmnopqrstuvwxy1%,] [--color=when] [-D format] [file ...]
/// ```
///
/// The GNU parser matches this text (it contains `usage:`) but extracts nothing,
/// because it expects one option per line with an inline description. This
/// parser runs first and only claims text that actually contains a bundled
/// group, so GNU-style help is unaffected.
pub struct BsdUsageParser;

impl CliParser for BsdUsageParser {
    fn name(&self) -> &str {
        "bsd-usage"
    }

    fn priority(&self) -> u32 {
        // Ahead of the GNU parser (100): its `can_parse` also accepts bundled
        // usage text, but yields zero flags for it.
        95
    }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        usage_lines(help_text).any(|line| BUNDLED_RE.is_match(line))
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let mut flags: Vec<ScannedFlag> = Vec::new();
        let mut positional_args: Vec<ScannedArg> = Vec::new();

        for line in usage_lines(help_text) {
            collect_valued_short_options(line, &mut flags);
            collect_long_options(line, &mut flags);
            collect_bundled_options(line, &mut flags);
            collect_operands(line, &mut positional_args);
        }

        Ok(ParsedHelp {
            description: extract_description(help_text),
            flags,
            positional_args,
            help_format: HelpFormat::Gnu,
            ..Default::default()
        })
    }
}

/// Iterate the `usage:` lines of a help text.
fn usage_lines(help_text: &str) -> impl Iterator<Item = &str> {
    help_text.lines().filter(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("usage:") || trimmed.starts_with("Usage:")
    })
}

/// Description text preceding the usage line, excluding option-rejection errors.
///
/// A tool that rejects `--help` prints its error first; treating that as the
/// description is worse than having none.
fn extract_description(help_text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("usage:") || trimmed.starts_with("Usage:") {
            break;
        }
        if trimmed.is_empty() || is_error_line(trimmed) {
            continue;
        }
        lines.push(trimmed);
    }
    lines.join(" ").chars().take(200).collect()
}

/// Whether a line is a tool's complaint about an unsupported option.
fn is_error_line(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    ERROR_MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Expand `[-abc]` into one boolean flag per character.
fn collect_bundled_options(line: &str, flags: &mut Vec<ScannedFlag>) {
    for cap in BUNDLED_RE.captures_iter(line) {
        for ch in cap[1].chars() {
            if ch == ',' {
                continue;
            }
            push_flag(flags, boolean_flag(&format!("-{ch}")));
        }
    }
}

/// Collect `[-D format]`-style options, which take a value.
fn collect_valued_short_options(line: &str, flags: &mut Vec<ScannedFlag>) {
    for cap in SHORT_VALUE_RE.captures_iter(line) {
        let mut flag = boolean_flag(&format!("-{}", &cap[1]));
        flag.value_type = ValueType::String;
        flag.value_name = Some(cap[2].to_string());
        push_flag(flags, flag);
    }
}

/// Collect `[--color=when]` and `[--all]`-style long options.
fn collect_long_options(line: &str, flags: &mut Vec<ScannedFlag>) {
    for cap in LONG_RE.captures_iter(line) {
        let mut flag = ScannedFlag {
            long_name: Some(cap[1].to_string()),
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
        if let Some(value) = cap.get(2) {
            flag.value_type = ValueType::String;
            flag.value_name = Some(value.as_str().to_string());
        }
        push_flag(flags, flag);
    }
}

/// Collect `[file ...]`-style variadic operands.
fn collect_operands(line: &str, args: &mut Vec<ScannedArg>) {
    for cap in OPERAND_RE.captures_iter(line) {
        let name = cap[1].to_string();
        if args.iter().any(|arg| arg.name == name) {
            continue;
        }
        args.push(ScannedArg {
            name,
            description: String::new(),
            value_type: ValueType::String,
            required: false,
            variadic: true,
        });
    }
}

/// Build a boolean switch with the given short name.
fn boolean_flag(short_name: &str) -> ScannedFlag {
    ScannedFlag {
        long_name: None,
        short_name: Some(short_name.to_string()),
        description: String::new(),
        value_type: ValueType::Boolean,
        required: false,
        default: None,
        enum_values: None,
        repeatable: false,
        value_name: None,
        ..Default::default()
    }
}

/// Append a flag unless one with the same name is already present.
///
/// A valued option often appears both in the bundled group and separately
/// (`ls` lists `D` in its group and `[-D format]` after it); the valued form is
/// collected first and must win.
fn push_flag(flags: &mut Vec<ScannedFlag>, flag: ScannedFlag) {
    let duplicate = flags.iter().any(|existing| {
        (existing.short_name.is_some() && existing.short_name == flag.short_name)
            || (existing.long_name.is_some() && existing.long_name == flag.long_name)
    });
    if !duplicate {
        flags.push(flag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LS_USAGE: &str = "ls: unrecognized option `--help'\nusage: ls [-@ABCFGHILOPRSTUWXabcdefghiklmnopqrstuvwxy1%,] [--color=when] [-D format] [file ...]\n";

    #[test]
    fn test_can_parse_bundled_usage() {
        assert!(BsdUsageParser.can_parse(LS_USAGE, "ls"));
    }

    #[test]
    fn test_can_parse_rejects_gnu_help() {
        let gnu = "Usage: tool [OPTIONS]\n\nOptions:\n  -v, --verbose   Be verbose\n";
        assert!(!BsdUsageParser.can_parse(gnu, "tool"));
    }

    #[test]
    fn test_can_parse_rejects_alternation_groups() {
        // `git`'s usage uses `[-v | --version]`, which is not a bundled group.
        let git = "usage: git [-v | --version] [-h | --help] [-C <path>]\n";
        assert!(!BsdUsageParser.can_parse(git, "git"));
    }

    #[test]
    fn test_can_parse_rejects_empty() {
        assert!(!BsdUsageParser.can_parse("", "tool"));
    }

    #[test]
    fn test_parse_expands_bundled_group() {
        let parsed = BsdUsageParser
            .parse("usage: cat [-belnstuv] [file ...]\n", "cat")
            .unwrap();
        let shorts: Vec<&str> = parsed
            .flags
            .iter()
            .filter_map(|flag| flag.short_name.as_deref())
            .collect();
        assert_eq!(shorts, vec!["-b", "-e", "-l", "-n", "-s", "-t", "-u", "-v"]);
        assert!(parsed
            .flags
            .iter()
            .all(|f| f.value_type == ValueType::Boolean));
    }

    #[test]
    fn test_parse_ls_usage_end_to_end() {
        let parsed = BsdUsageParser.parse(LS_USAGE, "ls").unwrap();

        let long_names: Vec<&str> = parsed
            .flags
            .iter()
            .filter_map(|flag| flag.long_name.as_deref())
            .collect();
        assert_eq!(long_names, vec!["--color"]);

        let color = parsed
            .flags
            .iter()
            .find(|flag| flag.long_name.as_deref() == Some("--color"))
            .expect("--color parsed");
        assert_eq!(color.value_name.as_deref(), Some("when"));

        // `-D` appears both inside the bundled group and as `[-D format]`; the
        // valued form must win.
        let d_flag = parsed
            .flags
            .iter()
            .find(|flag| flag.short_name.as_deref() == Some("-D"))
            .expect("-D parsed");
        assert_eq!(d_flag.value_type, ValueType::String);
        assert_eq!(d_flag.value_name.as_deref(), Some("format"));

        assert_eq!(parsed.positional_args.len(), 1);
        assert_eq!(parsed.positional_args[0].name, "file");
        assert!(parsed.positional_args[0].variadic);
    }

    #[test]
    fn test_parse_skips_comma_in_bundled_group() {
        let parsed = BsdUsageParser.parse("usage: ls [-abc1%,]\n", "ls").unwrap();
        assert!(parsed
            .flags
            .iter()
            .all(|flag| flag.short_name.as_deref() != Some("-,")));
    }

    #[test]
    fn test_parse_omits_option_rejection_error_from_description() {
        // Regression: the error text was being used as the tool description.
        let parsed = BsdUsageParser.parse(LS_USAGE, "ls").unwrap();
        assert!(
            parsed.description.is_empty(),
            "error text leaked: {}",
            parsed.description
        );
    }

    #[test]
    fn test_parse_keeps_real_description() {
        let help = "cat -- concatenate files\nusage: cat [-belnstuv] [file ...]\n";
        let parsed = BsdUsageParser.parse(help, "cat").unwrap();
        assert_eq!(parsed.description, "cat -- concatenate files");
    }

    #[test]
    fn test_parse_deduplicates_repeated_flags() {
        let parsed = BsdUsageParser
            .parse("usage: tool [-abc]\nusage: tool [-abc]\n", "tool")
            .unwrap();
        assert_eq!(parsed.flags.len(), 3);
    }
}
