use std::sync::LazyLock;

use regex::Regex;

use crate::models::{ScannedArg, ValueType};

// INVARIANT: every pattern is a compile-time constant valid regex.

/// A positional-argument placeholder in a usage line, e.g. `<file>` or `<file>...`.
static ARG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-zA-Z_][\w-]*)>(\.\.\.)?").expect("valid static regex"));

/// An option together with the value placeholder(s) it takes, e.g. `-C <path>`,
/// `-c <name>=<value>`, `--git-dir=<path>`, or `--exec-path[=<path>]`.
///
/// Usage lines often spell a global option's value inline using the same
/// `<name>` angle-bracket syntax as real positional arguments — this is what
/// git's `usage: git ... [-C <path>] [-c <name>=<value>] ...` does. Such
/// placeholders describe an option's argument, not a standalone operand, and
/// must not be reported as one.
static OPTION_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-{1,2}[A-Za-z][\w-]*\[?=?\s*<[^>]+>(?:=<[^>]+>)*\]?").expect("valid static regex")
});

/// Extract `<name>` positional-argument placeholders from a single usage line.
///
/// Skips placeholders that belong to an option's value (see [`OPTION_VALUE_RE`]),
/// including repeatable ones such as `[--include=<path>...]`, and marks a
/// placeholder as optional (`required: false`) when it appears inside an
/// unclosed `[...]` group, e.g. the trailing `<args>` in
/// `git [--version] <command> [<args>]`.
pub fn extract_args_from_usage_line(line: &str) -> Vec<ScannedArg> {
    let option_value_ranges: Vec<(usize, usize)> = OPTION_VALUE_RE
        .find_iter(line)
        .map(|m| (m.start(), m.end()))
        .collect();

    let mut args = Vec::new();
    for cap in ARG_RE.captures_iter(line) {
        // INVARIANT: group 0 is the whole match, always present on a capture.
        let Some(whole) = cap.get(0) else { continue };
        // Attribution is decided by where the placeholder *opens*: a trailing
        // `...` or `]` belongs to the placeholder's match but not to the
        // option's, so comparing whole ranges would let `[--include=<path>...]`
        // escape and be reported as a positional argument.
        let is_option_value = option_value_ranges
            .iter()
            .any(|&(start, end)| whole.start() >= start && whole.start() < end);
        if is_option_value {
            continue;
        }

        args.push(ScannedArg {
            name: cap[1].to_string(),
            description: String::new(),
            value_type: ValueType::String,
            required: bracket_depth_before(line, whole.start()) <= 0,
            variadic: cap.get(2).is_some(),
        });
    }
    args
}

/// Count unmatched `[` occurring before `pos`, to tell whether a token sits
/// inside an optional `[...]` group.
fn bracket_depth_before(line: &str, pos: usize) -> i32 {
    line[..pos].chars().fold(0, |depth, ch| match ch {
        '[' => depth + 1,
        ']' => depth - 1,
        _ => depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extracts_bare_required_arg() {
        let args = extract_args_from_usage_line("Usage: tool <file>");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "file");
        assert!(args[0].required);
        assert!(!args[0].variadic);
    }

    #[test]
    fn test_extracts_variadic_arg() {
        let args = extract_args_from_usage_line("Usage: tool <file>...");
        assert_eq!(args.len(), 1);
        assert!(args[0].variadic);
    }

    #[test]
    fn test_optional_arg_in_brackets_is_not_required() {
        let args = extract_args_from_usage_line("usage: git [--version] <command> [<args>]");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "command");
        assert!(args[0].required);
        assert_eq!(args[1].name, "args");
        assert!(!args[1].required);
    }

    #[test]
    fn test_skips_short_option_value_placeholder() {
        let args =
            extract_args_from_usage_line("usage: git [-v | --version] [-h | --help] [-C <path>]");
        assert!(args.is_empty());
    }

    #[test]
    fn test_skips_chained_short_option_value_placeholders() {
        let args = extract_args_from_usage_line("usage: git [-c <name>=<value>]");
        assert!(args.is_empty());
    }

    #[test]
    fn test_skips_long_option_inline_value_placeholder() {
        let args =
            extract_args_from_usage_line("usage: git [--git-dir=<path>] [--exec-path[=<path>]]");
        assert!(args.is_empty());
    }

    #[test]
    fn test_skips_variadic_long_option_value_placeholder() {
        let args = extract_args_from_usage_line("Usage: tool [--include=<path>...]");
        assert!(args.is_empty(), "expected no positional args, got {args:?}");
    }

    #[test]
    fn test_skips_variadic_short_option_value_placeholder() {
        let args = extract_args_from_usage_line("Usage: tool [-I <dir>...]");
        assert!(args.is_empty(), "expected no positional args, got {args:?}");
    }

    #[test]
    fn test_keeps_operand_following_an_option_value() {
        // The skip must not swallow a real operand that trails an option value.
        let args = extract_args_from_usage_line("Usage: tool [-o <out>] <file>");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "file");
        assert!(args[0].required);
    }

    #[test]
    fn test_full_git_root_usage_line_yields_no_positional_args() {
        let line = "usage: git [-v | --version] [-h | --help] [-C <path>] [-c <name>=<value>]";
        assert!(extract_args_from_usage_line(line).is_empty());
    }
}
