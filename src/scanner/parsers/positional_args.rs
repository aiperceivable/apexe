use std::sync::LazyLock;

use regex::Regex;

use crate::models::{ScannedArg, ValueType};

// INVARIANT: every pattern is a compile-time constant valid regex.

/// A positional-argument placeholder in a usage line, e.g. `<file>`,
/// `<file>...` or `<jq filter>`.
///
/// The name may contain spaces: `jq [options] <jq filter> [file...]` spells its
/// operand as two words, and requiring a single token dropped `jq`'s only
/// required argument — the filter — leaving a module that could not do any of
/// the tool's actual work.
static ARG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([a-zA-Z_][\w -]*)>(\.\.\.)?").expect("valid static regex"));

/// A bracketed variadic operand, e.g. `[file ...]` or `[file...]`.
///
/// The trailing `...` is required, and that is what stops this from swallowing
/// `[options]`, `[-l]` or `[--color=when]`: a bracketed group is only read as an
/// operand when it is explicitly repeatable. Mirrors the BSD parser's
/// `OPERAND_RE`, which has carried the same restriction since it was written.
static BRACKET_OPERAND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([a-z][\w-]*)\s*\.\.\.\]").expect("valid static regex"));

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

    // Collected with their offsets so both placeholder styles can be returned
    // in the order they appear on the line. Order is the argument's meaning:
    // `<jq filter> [file...]` and `[file...] <jq filter>` are different calls.
    let mut found: Vec<(usize, ScannedArg)> = Vec::new();

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

        found.push((
            whole.start(),
            ScannedArg {
                name: normalize_name(&cap[1]),
                description: String::new(),
                value_type: ValueType::String,
                required: bracket_depth_before(line, whole.start()) <= 0,
                variadic: cap.get(2).is_some(),
            },
        ));
    }

    for cap in BRACKET_OPERAND_RE.captures_iter(line) {
        // INVARIANT: group 0 is the whole match, always present on a capture.
        let Some(whole) = cap.get(0) else { continue };
        let name = normalize_name(&cap[1]);
        if found.iter().any(|(_, arg)| arg.name == name) {
            continue;
        }
        found.push((
            whole.start(),
            ScannedArg {
                name,
                description: String::new(),
                value_type: ValueType::String,
                // Bracketed by construction, hence optional.
                required: false,
                variadic: true,
            },
        ));
    }

    found.sort_by_key(|(offset, _)| *offset);
    found.into_iter().map(|(_, arg)| arg).collect()
}

/// Normalize a placeholder name into a schema-property-safe form.
///
/// Spaces become underscores so a two-word placeholder like `<jq filter>`
/// yields `jq_filter` rather than a key no caller could type.
fn normalize_name(raw: &str) -> String {
    raw.trim().replace(' ', "_")
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

    #[test]
    fn test_extracts_multi_word_placeholder_name() {
        // jq's only required argument is spelled as two words; requiring a
        // single token dropped it, leaving a module that could not filter.
        let args = extract_args_from_usage_line("Usage:\tjq [options] <jq filter> [file...]");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "jq_filter");
        assert!(args[0].required);
        assert!(!args[0].variadic);
    }

    #[test]
    fn test_extracts_bracketed_variadic_operand() {
        let args = extract_args_from_usage_line("Usage:\tjq [options] <jq filter> [file...]");
        assert_eq!(args[1].name, "file");
        assert!(!args[1].required);
        assert!(args[1].variadic);
    }

    #[test]
    fn test_bracketed_operand_requires_ellipsis() {
        // Without the `...` restriction this would read `[options]`,
        // `[-l]` and `[--color=when]` as operands.
        let args = extract_args_from_usage_line(
            "usage: ls [options] [-l] [--color=when] [--exclude=<pat>]",
        );
        assert!(
            args.is_empty(),
            "bracketed groups without `...` are not operands: {args:?}"
        );
    }

    #[test]
    fn test_operands_are_returned_in_usage_order() {
        // Order is the argument's meaning; the two styles must interleave
        // correctly rather than one being appended after the other.
        let args = extract_args_from_usage_line("usage: tool [src ...] <dest>");
        let names: Vec<&str> = args.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["src", "dest"]);
    }

    #[test]
    fn test_bracketed_operand_does_not_duplicate_angle_placeholder() {
        let args = extract_args_from_usage_line("usage: tool <file> [file ...]");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "file");
    }
}
