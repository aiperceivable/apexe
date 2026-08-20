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

/// A bracketed operand: `[file ...]`, `[FILE]...`, `[path...]` or `[expression]`.
///
/// Both ellipsis placements occur and mean the same thing. BSD writes it inside
/// the brackets (`ls ... [file ...]`); GNU writes it outside (`ls [OPTION]...
/// [FILE]...`), and every one of the 20 GNU tools with a built-in overlay uses
/// the outer form. Group 2 captures the inner ellipsis and group 3 the outer, so
/// either marks the operand variadic.
static BRACKET_OPERAND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[([A-Za-z][\w-]*)(\s*\.\.\.)?\](\.\.\.)?").expect("valid static regex")
});

/// A bare operand written in the GNU convention of an all-caps placeholder,
/// e.g. `SOURCE`, `DEST`, `DIRECTORY...`, `PATTERNS`, `LINK_NAME`.
///
/// GNU spells required operands without brackets — `cp [OPTION]... [-T] SOURCE
/// DEST` — so the bracketed pattern above never sees them. All-caps is what
/// separates a placeholder from the surrounding prose; a lowercase bare word in
/// a usage line is far more often descriptive text than an operand, so this
/// deliberately does not match one.
///
/// The trailing `\b` is load-bearing: without it the pattern matches the `U` of
/// `Usage:` — one capital followed by lowercase is not a placeholder, and the
/// word boundary is what rejects it.
static BARE_OPERAND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z][A-Z0-9_-]*)\b(\.\.\.)?").expect("valid static regex"));

/// Usage-line placeholders that name the *option* group rather than an operand.
///
/// `cut OPTION... [FILE]...` and `ls [OPTION]... [FILE]...` would otherwise
/// report an operand called `OPTION`, which no tool accepts.
const OPTION_GROUP_WORDS: &[&str] = &["option", "options", "opts", "flag", "flags"];

/// Whether `name` names the option group rather than an operand.
fn is_option_group_word(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    OPTION_GROUP_WORDS.contains(&lowered.as_str())
}

/// Whether the token starting at `start` sits inside a bracketed group that
/// begins with a dash, i.e. it belongs to an option rather than being an
/// operand.
///
/// Checking only the immediately preceding character is not enough: BSD bundles
/// every short option into one group, so `[-@ABCF]` puts `ABCF` behind a `@`
/// rather than behind the dash. What marks the whole group as options is the
/// dash right after the opening bracket, which also covers `[-T]`, `[-Olevel]`
/// and `[-D debugopts]`.
///
/// A leading `(` is skipped before that test. Man-page synopses spell an
/// alias set as a parenthesised alternation — git writes
/// `[(-m | --max-count) <num>]` and `[(-c | -C | --squash) <commit>]` — so the
/// character after `[` is `(`, not `-`, and the group read as operands. That
/// yielded `num`, `pager` and `o` as positionals on `cli.git.grep`, and on
/// `cli.git.commit` an operand named `c` that displaced git's real `-c` option.
/// Ordinary `--help` text never exposed this, because there git writes
/// `[-C <path>]` and the dash is already first.
fn inside_option_group(line: &str, start: usize) -> bool {
    let mut depth = 0i32;
    for (index, ch) in line[..start].char_indices().rev() {
        match ch {
            ']' => depth += 1,
            '[' => {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                // An enclosing group. Every one of them has to be examined, not
                // just the innermost: git writes
                // `[(-O | --open-files-in-pager) [<pager>]]`, where `<pager>`'s
                // own bracket says nothing and the option marker lives one level
                // out. Staying at depth 0 walks outward through the rest.
                // `[` is ASCII, so `index + 1` is a char boundary.
                let group = line[index + 1..].trim_start();
                let group = group.strip_prefix('(').map_or(group, str::trim_start);
                if group.starts_with('-') {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

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

/// Byte ranges on a usage line that belong to an option's value.
///
/// See [`OPTION_VALUE_RE`]: `git -C <path>` spells the option's argument with
/// the same angle-bracket syntax a real operand uses, so two of the three
/// passes below have to exclude anything opening inside one of these.
struct OptionValueRanges(Vec<(usize, usize)>);

impl OptionValueRanges {
    /// Collect every option-value span on `line`.
    fn of(line: &str) -> Self {
        Self(
            OPTION_VALUE_RE
                .find_iter(line)
                .map(|m| (m.start(), m.end()))
                .collect(),
        )
    }

    /// Whether a placeholder *opening* at `offset` belongs to an option.
    ///
    /// Attribution is decided by where the placeholder opens, not by its whole
    /// range: a trailing `...` or `]` belongs to the placeholder's match but
    /// not to the option's, so comparing whole ranges would let
    /// `[--include=<path>...]` escape and be reported as a positional argument.
    fn covers(&self, offset: usize) -> bool {
        self.0
            .iter()
            .any(|&(start, end)| offset >= start && offset < end)
    }
}

/// Build an operand with the fields every pass below fills the same way.
fn scanned_operand(name: String, required: bool, variadic: bool) -> ScannedArg {
    ScannedArg {
        name,
        description: String::new(),
        value_type: ValueType::String,
        required,
        variadic,
        before_flags: false,
    }
}

/// Whether `name` at `offset` is an option's own value rather than an operand.
///
/// A placeholder sitting inside an option group is that option's value. The
/// bracketed and bare passes have always checked this; the angle-bracket pass
/// did not, which only became visible once man-page SYNOPSIS lines started
/// reaching here — git's `[(-m | --max-count) <num>]` put `num` on
/// `cli.git.grep` as a positional, so `{"num": 3}` rendered `git grep 3` and
/// searched for the literal "3".
///
/// `is_option_group_word` catches the other spelling: `git log [<options>]`
/// names its option group in the same syntax as a real operand, and reporting
/// `options` as a positional puts a bare `options` token on the command line,
/// which no tool accepts.
fn belongs_to_an_option(line: &str, offset: usize, name: &str) -> bool {
    inside_option_group(line, offset) || is_option_group_word(name)
}

/// Angle-bracket placeholders: `<file>`, `[<args>]`, `<path>...`.
fn collect_angle_placeholders(
    line: &str,
    ranges: &OptionValueRanges,
    found: &mut Vec<(usize, ScannedArg)>,
) {
    for cap in ARG_RE.captures_iter(line) {
        // INVARIANT: group 0 is the whole match, always present on a capture.
        let Some(whole) = cap.get(0) else { continue };
        if ranges.covers(whole.start()) {
            continue;
        }
        let name = normalize_name(&cap[1]);
        if belongs_to_an_option(line, whole.start(), &name) {
            continue;
        }
        found.push((
            whole.start(),
            scanned_operand(
                name,
                bracket_depth_before(line, whole.start()) <= 0,
                cap.get(2).is_some(),
            ),
        ));
    }
}

/// Bracketed operands: `[FILE]`, `[file...]`.
///
/// A bracketed lowercase word with no ellipsis is not evidence enough. BSD
/// writes optional operands lowercase and its required ones bare
/// (`basename string [suffix]`), and bare lowercase is deliberately not matched
/// by [`BARE_OPERAND_RE`] — so accepting `[suffix]` here would yield a contract
/// holding only the optional half of the tool's arguments, which reads as "this
/// is all it takes" and is worse than reporting none. An all-caps placeholder
/// is GNU's explicit convention and carries no such doubt.
fn collect_bracketed_operands(line: &str, found: &mut Vec<(usize, ScannedArg)>) {
    for cap in BRACKET_OPERAND_RE.captures_iter(line) {
        // INVARIANT: groups 0 and 1 are present on every capture of this pattern.
        let (Some(whole), Some(name_match)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        let name = normalize_name(name_match.as_str());
        if belongs_to_an_option(line, name_match.start(), &name) {
            continue;
        }

        let variadic = cap.get(2).is_some() || cap.get(3).is_some();
        let is_placeholder_caps = !name.chars().any(|ch| ch.is_ascii_lowercase());
        if !is_placeholder_caps && !variadic {
            continue;
        }
        if found.iter().any(|(_, arg)| arg.name == name) {
            continue;
        }
        // Bracketed by construction, hence optional.
        found.push((whole.start(), scanned_operand(name, false, variadic)));
    }
}

/// Bare operands: `file`, `FILE...`.
fn collect_bare_operands(
    line: &str,
    ranges: &OptionValueRanges,
    found: &mut Vec<(usize, ScannedArg)>,
) {
    for cap in BARE_OPERAND_RE.captures_iter(line) {
        // INVARIANT: groups 0 and 1 are present on every capture of this pattern.
        let (Some(whole), Some(name_match)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        let name = normalize_name(name_match.as_str());
        if belongs_to_an_option(line, name_match.start(), &name) || ranges.covers(whole.start()) {
            continue;
        }
        // An all-caps word inside brackets was already taken by the bracketed
        // pass, which knows the ellipsis placement; the name check catches it.
        if found.iter().any(|(_, arg)| arg.name == name) {
            continue;
        }
        found.push((
            whole.start(),
            scanned_operand(
                name,
                bracket_depth_before(line, whole.start()) <= 0,
                cap.get(2).is_some(),
            ),
        ));
    }
}

/// Extract `<name>` positional-argument placeholders from a single usage line.
///
/// Three passes over the same line, one per placeholder spelling, each skipping
/// anything the earlier passes already claimed. Results are collected with their
/// offsets and sorted at the end because order is the argument's meaning:
/// `<jq filter> [file...]` and `[file...] <jq filter>` are different calls.
///
/// Placeholders belonging to an option's value are skipped throughout — see
/// [`OptionValueRanges`] — including repeatable ones such as
/// `[--include=<path>...]`. A placeholder inside an unclosed `[...]` group is
/// marked optional, e.g. the trailing `<args>` in
/// `git [--version] <command> [<args>]`.
pub fn extract_args_from_usage_line(line: &str) -> Vec<ScannedArg> {
    let ranges = OptionValueRanges::of(line);
    let mut found: Vec<(usize, ScannedArg)> = Vec::new();

    collect_angle_placeholders(line, &ranges, &mut found);
    collect_bracketed_operands(line, &mut found);
    collect_bare_operands(line, &ranges, &mut found);

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
    fn test_skips_an_unbracketed_option_value_placeholder() {
        // Every other option-value test brackets the option, and a bracketed
        // one is caught by `inside_option_group` rather than by the
        // option-value ranges. Removing the range check therefore broke nothing
        // in this suite while still reporting `file` as an operand of the
        // command — so `{"file": "x", "input": "y"}` rendered `tool x y`, the
        // option's value emitted bare with the option itself gone.
        let args = extract_args_from_usage_line("Usage: tool --output=<file> <input>");
        assert_eq!(
            args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["input"],
            "the option's value is not an operand: {args:?}"
        );
    }

    #[test]
    fn test_skips_an_unbracketed_option_value_written_with_a_space() {
        // `-m, --max-time <seconds>` is how help text usually spells it, and
        // the separated form is the one apexe renders (see `build_argv`).
        let args = extract_args_from_usage_line("Usage: tool --output <file> <input>");
        assert_eq!(
            args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["input"],
            "the option's value is not an operand: {args:?}"
        );
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

    /// One operand as extracted from a usage line.
    #[derive(Debug, PartialEq)]
    struct Operand {
        name: String,
        required: bool,
        variadic: bool,
    }

    /// One usage line and the operands it must yield.
    struct UsageCase {
        line: &'static str,
        operands: Vec<Operand>,
    }

    fn operand(name: &str, required: bool, variadic: bool) -> Operand {
        Operand {
            name: name.to_string(),
            required,
            variadic,
        }
    }

    fn extract(line: &str) -> Vec<Operand> {
        extract_args_from_usage_line(line)
            .into_iter()
            .map(|a| operand(&a.name, a.required, a.variadic))
            .collect()
    }

    fn names(line: &str) -> Vec<String> {
        extract(line).into_iter().map(|o| o.name).collect()
    }

    #[test]
    fn test_gnu_usage_lines_from_real_binaries() {
        // Every line here was read off GNU coreutils 9.7 / grep 3.11 /
        // diffutils 3.10 / findutils 4.10.0 in the digest-pinned image the
        // overlays' provenance names. GNU puts the ellipsis *outside* the
        // brackets and writes required operands bare and in caps -- neither
        // form was matched before, so every one of these yielded nothing.
        let cases = vec![
            UsageCase {
                line: "Usage: ls [OPTION]... [FILE]...",
                operands: vec![operand("FILE", false, true)],
            },
            UsageCase {
                line: "Usage: cp [OPTION]... [-T] SOURCE DEST",
                operands: vec![operand("SOURCE", true, false), operand("DEST", true, false)],
            },
            UsageCase {
                line: "Usage: cut OPTION... [FILE]...",
                operands: vec![operand("FILE", false, true)],
            },
            UsageCase {
                line: "Usage: uniq [OPTION]... [INPUT [OUTPUT]]",
                operands: vec![
                    operand("INPUT", false, false),
                    operand("OUTPUT", false, false),
                ],
            },
            UsageCase {
                line: "Usage: mkdir [OPTION]... DIRECTORY...",
                operands: vec![operand("DIRECTORY", true, true)],
            },
            UsageCase {
                line: "Usage: touch [OPTION]... FILE...",
                operands: vec![operand("FILE", true, true)],
            },
            UsageCase {
                line: "Usage: ln [OPTION]... [-T] TARGET LINK_NAME",
                operands: vec![
                    operand("TARGET", true, false),
                    operand("LINK_NAME", true, false),
                ],
            },
            UsageCase {
                line: "Usage: grep [OPTION]... PATTERNS [FILE]...",
                operands: vec![
                    operand("PATTERNS", true, false),
                    operand("FILE", false, true),
                ],
            },
            UsageCase {
                line: "Usage: diff [OPTION]... FILES",
                operands: vec![operand("FILES", true, false)],
            },
            UsageCase {
                line: "Usage: xargs [OPTION]... COMMAND [INITIAL-ARGS]...",
                operands: vec![
                    operand("COMMAND", true, false),
                    operand("INITIAL-ARGS", false, true),
                ],
            },
        ];

        for case in cases {
            assert_eq!(
                extract(case.line),
                case.operands,
                "mismatch for {:?}",
                case.line
            );
        }
    }

    #[test]
    fn test_gnu_option_group_placeholder_is_not_an_operand() {
        // `[OPTION]...` and the bare `OPTION...` that `cut` uses both name the
        // option group. Reporting an operand called OPTION would have an agent
        // pass a value no tool accepts.
        for line in [
            "Usage: ls [OPTION]... [FILE]...",
            "Usage: cut OPTION... [FILE]...",
        ] {
            let got = names(line);
            assert!(
                !got.iter().any(|n| n.eq_ignore_ascii_case("option")),
                "OPTION must not be reported as an operand in {line:?}: {got:?}"
            );
        }
    }

    #[test]
    fn test_dash_prefixed_bracket_group_is_not_an_operand() {
        // `[-T]` is an option; its tail is a capital letter the bare-operand
        // pattern would otherwise match.
        assert_eq!(
            names("Usage: cp [OPTION]... [-T] SOURCE DEST"),
            vec!["SOURCE", "DEST"]
        );
    }

    #[test]
    fn test_bsd_short_option_bundle_is_not_an_operand() {
        // BSD bundles every short option into one group, so the capitals sit
        // behind a `@` rather than behind the dash -- only the dash after the
        // opening bracket marks the whole group as options.
        assert_eq!(
            extract("usage: ls [-@ABCF] [--color=when] [-D format] [file ...]"),
            vec![operand("file", false, true)]
        );
    }

    #[test]
    fn test_gnu_find_usage_line_mixes_option_and_operand_brackets() {
        // `[-H] [-L] [-P] [-Olevel] [-D debugopts]` are all options and must not
        // appear. `[path...]` is an operand by its ellipsis. `[expression]` is
        // one too, but a bracketed lowercase word without an ellipsis is not
        // distinguishable from prose, so it is deliberately left out -- find's
        // predicate expression is not something this contract could express
        // anyway.
        assert_eq!(
            extract("Usage: find [-H] [-L] [-P] [-Olevel] [-D debugopts] [path...] [expression]"),
            vec![operand("path", false, true)]
        );
    }

    #[test]
    fn test_bracketed_lowercase_without_ellipsis_is_not_an_operand() {
        // `basename string [suffix]` spells its required operand as a bare
        // lowercase word, which is not matched. Taking `[suffix]` alone would
        // describe the tool as accepting only its optional argument -- a
        // contract that is confidently wrong rather than merely incomplete.
        assert!(extract("usage: basename string [suffix]").is_empty());
    }

    #[test]
    fn test_parenthesised_alias_group_is_not_an_operand() {
        // Regression: man-page SYNOPSIS lines spell an alias set as a
        // parenthesised alternation, so the character after `[` is `(` rather
        // than `-` and the whole group read as operands. Verbatim from
        // git-grep(1) and git-commit(1).
        let grep = extract(
            "git grep [(-O | --open-files-in-pager) [<pager>]] [(-m | --max-count) <num>] \
             [<pathspec>...]",
        );
        let names: Vec<&str> = grep.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["pathspec"],
            "option letters and option values must not become operands: {names:?}"
        );

        let commit = extract("git commit [(-c | -C | --squash) <commit>] [--] [<pathspec>...]");
        let names: Vec<&str> = commit.iter().map(|a| a.name.as_str()).collect();
        assert!(
            !names.contains(&"c") && !names.contains(&"commit"),
            "an operand named `c` displaces git's real -c option: {names:?}"
        );
        assert_eq!(names, vec!["pathspec"]);
    }

    #[test]
    fn test_optional_bracketed_operand_survives_the_option_group_check() {
        // The guard must not swallow a genuine operand that happens to be
        // bracketed: the group's content has no leading dash.
        let args = extract("usage: git [--version] <command> [<args>]");
        let names: Vec<&str> = args.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["command", "args"]);
    }
}
