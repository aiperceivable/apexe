use std::borrow::Cow;
use std::process::Command;

use crate::models::{ScannedFlag, ValueType};
use crate::scanner::protocol::ParsedHelp;

/// How a BSD `DESCRIPTION` section announces its option list, since those pages
/// have no dedicated `OPTIONS` header.
///
/// Matched as a pattern rather than a fixed list because the wording varies per
/// page and the variants kept arriving one bug at a time: `ls` says "The
/// following options are available", `cat` "The options are as follows", `sort`
/// "The command line options are as follows", `chmod` "The generic options are
/// as follows". Missing one is silent — the whole option block simply becomes
/// invisible to Tier 2 and the tool scans with fewer flags.
fn is_bsd_option_intro(lowered: &str) -> bool {
    lowered.starts_with("the ")
        && lowered.contains("options are")
        && (lowered.contains("as follows") || lowered.contains("available"))
}

/// A flag parsed from one man page option line, plus any description text that
/// shared that line.
struct FlagLine {
    flag: ScannedFlag,
    inline_description: String,
}

/// Tier 2 parser: extracts metadata from man pages.
///
/// Man pages are a first-class source of usage information, not just a
/// description patch: for tools whose `--help` is a single bundled usage line
/// (most BSD/macOS built-ins such as `ls`), the man page is the *only* place
/// where individual options are documented.
pub struct ManPageParser;

impl ManPageParser {
    /// Parse the man page for `tool_name`.
    ///
    /// Runs `man -P cat <tool>`, strips nroff overstrike formatting, then
    /// extracts the description and the option list. Returns `None` when no
    /// man page exists or nothing could be extracted from it.
    pub fn parse_man_page(&self, tool_name: &str) -> Option<ParsedHelp> {
        let output = Command::new("man")
            .args(["-P", "cat", tool_name])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let text = strip_overstrike(&raw);
        let description = extract_man_description(&text);
        let mut flags = extract_man_options(&text);
        // Same repair the parser pipeline applies to `--help` output: an option
        // whose value placeholder ended up at the head of its description takes
        // a value, and must not be typed boolean.
        crate::scanner::parsers::value_placeholder::recover_values_from_descriptions(&mut flags);
        let examples = extract_man_examples(&text, tool_name);

        if description.is_empty() && flags.is_empty() && examples.is_empty() {
            return None;
        }

        Some(ParsedHelp {
            description,
            flags,
            examples,
            ..Default::default()
        })
    }
}

/// Remove nroff overstrike sequences (`X\bX` for bold, `_\bX` for italic).
///
/// `man -P cat` emits these on both GNU and BSD systems, so raw output looks
/// like `DDEESSCCRRIIPPTTIIOONN`. Every section header and flag name is
/// unrecognizable until they are collapsed. Returns the input borrowed when it
/// contains no backspace, so already-clean text allocates nothing.
pub fn strip_overstrike(text: &str) -> Cow<'_, str> {
    if !text.contains('\u{8}') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\u{8}' {
            out.pop();
        } else {
            out.push(ch);
        }
    }
    Cow::Owned(out)
}

/// Extract the prose description from the `DESCRIPTION` section of a man page.
pub fn extract_man_description(text: &str) -> String {
    let mut in_description = false;
    let mut lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "DESCRIPTION" || trimmed == "Description" {
            in_description = true;
            continue;
        }
        if !in_description {
            continue;
        }
        if is_section_header(line) && !lines.is_empty() {
            break;
        }
        if trimmed.is_empty() {
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        lines.push(trimmed);
    }

    lines.join(" ").chars().take(200).collect()
}

/// Most examples kept from one page, and the longest single example kept.
///
/// A man page lists a handful of illustrative invocations, so a page offering
/// far more than this is being misread, and an example far longer than this is
/// a wrapped paragraph rather than a command.
const MAX_MAN_EXAMPLES: usize = 20;
const MAX_MAN_EXAMPLE_LEN: usize = 300;

/// Extract example invocations from a man page's `EXAMPLES` section.
///
/// These are the only hand-written, human-reviewed usages a scan can reach: the
/// help text lists what the flags *are*, while `EXAMPLES` shows which
/// combinations actually make sense together. That is what a caller needs to
/// use the tool, and no amount of flag parsing recovers it.
///
/// Two layouts appear, so both are matched:
///   * a shell prompt — `$ grep -w 'patricia' myfile` (`ls`, `grep`)
///   * a bare invocation — `tar -czf file.tar.gz source.c` (`tar`, `find`)
///
/// A bare invocation is only accepted when it starts with the tool's own name,
/// which is what separates a command from the prose describing it — every one
/// of these pages interleaves the two.
pub fn extract_man_examples(text: &str, tool_name: &str) -> Vec<String> {
    let Some(body) = section_body(text, "EXAMPLES") else {
        return Vec::new();
    };
    let mut examples: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        let candidate = if let Some(rest) = trimmed.strip_prefix("$ ") {
            rest.trim()
        } else if trimmed == tool_name
            || trimmed
                .strip_prefix(tool_name)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        {
            trimmed
        } else {
            continue;
        };
        if candidate.is_empty() || candidate.len() > MAX_MAN_EXAMPLE_LEN {
            continue;
        }
        let candidate = candidate.to_string();
        if !examples.contains(&candidate) {
            examples.push(candidate);
        }
        if examples.len() >= MAX_MAN_EXAMPLES {
            break;
        }
    }
    examples
}

/// Whether `text` is a man page rather than ordinary `--help` output.
///
/// Several tools delegate `--help` to their man page — `git log --help` runs
/// `man git-log` — so the text a scan captures for a subcommand can be either
/// shape, and the wrong parser extracts nothing at all from it.
///
/// Both `NAME` and `SYNOPSIS` are required, at column zero. Requiring both is
/// what keeps ordinary help text out: a `--help` listing may well contain the
/// word SYNOPSIS in prose, but the pair as unindented section headers is
/// specific to a roff page.
pub fn is_man_page(text: &str) -> bool {
    let mut has_name = false;
    let mut has_synopsis = false;
    for line in text.lines() {
        if !is_section_header(line) {
            continue;
        }
        match line.trim() {
            "NAME" => has_name = true,
            "SYNOPSIS" => has_synopsis = true,
            _ => {}
        }
        if has_name && has_synopsis {
            return true;
        }
    }
    false
}

/// The one-line summary a man page's `NAME` section states, without the
/// `command - ` prefix that section always carries.
///
/// `NAME` is where a page says what the command *is* in one sentence
/// ("git-log - Show commit logs"). `DESCRIPTION` opens with prose that reads
/// poorly as a module description, so this is preferred when present.
pub fn extract_man_summary(text: &str) -> String {
    let Some(body) = section_body(text, "NAME") else {
        return String::new();
    };
    let Some(line) = body.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    // The separator is an ASCII hyphen surrounded by spaces; a hyphen inside
    // the command name (`git-log`) has none, which is what distinguishes them.
    match line.split_once(" - ") {
        Some((_, summary)) => summary.trim().to_string(),
        None => line.to_string(),
    }
}

/// The first invocation form from a man page's `SYNOPSIS`, unwrapped onto one
/// line.
///
/// Only the first form is returned. A page listing several — `git checkout` has
/// six — describes alternatives, and merging their operands yields an argument
/// list that no single invocation accepts. The first form is the canonical one.
///
/// Wrapped continuations are joined: a form too long for the page width is
/// broken across lines indented further than the form's own first line.
pub fn extract_man_synopsis(text: &str) -> String {
    let Some(body) = section_body(text, "SYNOPSIS") else {
        return String::new();
    };
    let mut lines = body
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .peekable();
    let Some(first) = lines.next() else {
        return String::new();
    };
    let base_indent = indent_width(first);
    let mut form = first.trim().to_string();
    for line in lines {
        if line.trim().is_empty() || indent_width(line) <= base_indent {
            break;
        }
        form.push(' ');
        form.push_str(line.trim());
    }
    form
}

/// The body of a top-level man section, or `None` when the page has no such
/// section. Section headers sit at column zero; everything until the next one
/// belongs to the section.
fn section_body<'a>(text: &'a str, header: &str) -> Option<&'a str> {
    let mut start: Option<usize> = None;
    let mut offset = 0usize;
    for line in text.lines() {
        let line_start = offset;
        offset += line.len() + 1;
        match start {
            None => {
                if line.trim() == header {
                    start = Some(offset.min(text.len()));
                }
            }
            Some(begin) => {
                if is_section_header(line) {
                    return Some(&text[begin..line_start.min(text.len())]);
                }
            }
        }
    }
    start.map(|begin| &text[begin.min(text.len())..])
}

/// Extract flags from a man page's option list.
///
/// Handles both the GNU layout (a dedicated `OPTIONS` section) and the BSD
/// layout (options listed inside `DESCRIPTION` after an intro line such as
/// "The following options are available:"). Indentation width is inferred from
/// the first option line rather than hardcoded, because GNU pages indent
/// options by 7 columns and BSD pages by 5.
pub fn extract_man_options(text: &str) -> Vec<ScannedFlag> {
    let Some(body) = options_body(text) else {
        return Vec::new();
    };

    let mut flags: Vec<ScannedFlag> = Vec::new();
    let mut current: Option<ScannedFlag> = None;
    let mut description_lines: Vec<String> = Vec::new();
    let mut flag_indent: Option<usize> = None;

    for line in body {
        if is_section_header(line) {
            break;
        }
        let trimmed = line.trim();

        if is_flag_line(line, flag_indent) {
            flush_flag(&mut flags, current.take(), &mut description_lines);
            flag_indent.get_or_insert_with(|| indent_width(line));
            let parsed = parse_flag_line(trimmed);
            if !parsed.inline_description.is_empty() {
                description_lines.push(parsed.inline_description);
            }
            current = Some(parsed.flag);
        } else if trimmed.is_empty() {
            flush_flag(&mut flags, current.take(), &mut description_lines);
        } else if current.is_some() {
            description_lines.push(trimmed.to_string());
        }
    }

    flush_flag(&mut flags, current.take(), &mut description_lines);
    flags
}

/// Return the lines following the start of the option list, or `None` when the
/// page documents no options.
fn options_body(text: &str) -> Option<Vec<&str>> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|line| is_options_start(line))?;
    Some(lines[start + 1..].to_vec())
}

/// Whether a line starts the option list, in either the GNU or BSD layout.
fn is_options_start(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed == "OPTIONS" || trimmed == "Options" {
        return true;
    }
    is_bsd_option_intro(&trimmed.to_ascii_lowercase())
}

/// Whether a line is a top-level section header (all caps, not indented).
fn is_section_header(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && !line.starts_with(char::is_whitespace)
        && trimmed == trimmed.to_uppercase()
}

/// Whether a line introduces a new flag.
///
/// A flag line is indented and its first token looks like `-x` or `--name`.
/// Once the first flag's indentation is known, more deeply indented lines are
/// treated as description continuations even if they happen to start with `-`.
fn is_flag_line(line: &str, flag_indent: Option<usize>) -> bool {
    let trimmed = line.trim_start();
    if trimmed.len() == line.len() || !trimmed.starts_with('-') {
        return false;
    }
    if let Some(expected) = flag_indent {
        if indent_width(line) > expected {
            return false;
        }
    }
    trimmed
        .split_whitespace()
        .next()
        .is_some_and(|token| token.len() > 1)
}

/// Number of leading whitespace characters on a line.
fn indent_width(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Move the accumulated flag and its description into the result list.
fn flush_flag(
    flags: &mut Vec<ScannedFlag>,
    current: Option<ScannedFlag>,
    description_lines: &mut Vec<String>,
) {
    if let Some(mut flag) = current {
        flag.description = description_lines.join(" ").trim().to_string();
        flags.push(flag);
    }
    description_lines.clear();
}

/// Parse the flag names, value placeholder, and any same-line description from
/// one option line (already trimmed).
///
/// Handles alias lists (`-a, --all`), inline values (`--color=when`), separate
/// value placeholders (`-D format`, `-o FILE`), and the optional-value form
/// `--exec-path[=<path>]`.
fn parse_flag_line(trimmed: &str) -> FlagLine {
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let mut short_name: Option<String> = None;
    let mut long_name: Option<String> = None;
    let mut value_name: Option<String> = None;
    let mut value_optional = false;
    let mut consumed = 0;

    while consumed < tokens.len() {
        let raw = tokens[consumed];
        let token = raw.trim_end_matches(',');
        if !token.starts_with('-') || token.len() < 2 {
            // A detached value placeholder in the middle of an alias list:
            // `-n <number>, --max-count=<number>`. The trailing comma is what
            // says the list continues, and without consuming this token the
            // scan stopped at `-n` and lost every long alias spelled this way.
            // A placeholder *without* a comma is left to the post-loop check,
            // which requires it to be the whole remainder — otherwise `-l list
            // in long format` would read "list" as a value name.
            let continues = raw.ends_with(',');
            if continues && consumed > 0 && is_value_placeholder(token) {
                if value_name.is_none() {
                    value_name = Some(strip_placeholder_brackets(token));
                }
                consumed += 1;
                continue;
            }
            break;
        }
        let (name, inline_value) = match token.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (token, None),
        };
        // `--exec-path[=<path>]` puts the bracket on the *name* side of the
        // `=`. Left in place it produced a flag literally named `--exec-path[`
        // and a schema property key ending in `[` — a token git rejects, and an
        // awkward identifier for any consumer generating code from the schema.
        // The bracket is the only thing distinguishing an optional value from a
        // required one, so it is recorded rather than merely dropped.
        let name = match name.strip_suffix('[') {
            Some(stripped) => {
                value_optional = true;
                stripped
            }
            None => name,
        };
        if let Some(value) = inline_value.filter(|value| !value.is_empty()) {
            value_name = Some(strip_placeholder_brackets(value));
        }
        // A short option may carry its value attached with no separator at all:
        // git spells `-n<num>`, `-S<string>`, `-G<regex>`, `-O<orderfile>`.
        // Splitting at the placeholder is what keeps the property key `n`
        // rather than `n<num>`.
        let name = match name.split_once('<') {
            Some((head, tail)) if !head.is_empty() => {
                if value_name.is_none() {
                    value_name = Some(strip_placeholder_brackets(tail));
                }
                head
            }
            _ => name,
        };
        let name = strip_optional_group(name);
        if name.trim_start_matches('-').is_empty() {
            // `--` on its own is the end-of-options marker, not an option, and
            // it is documented in OPTIONS by several tools. `canonical_name`
            // strips the dashes, so it used to reach the schema as a property
            // whose key was the empty string.
            consumed += 1;
            continue;
        }
        if name.starts_with("--") {
            long_name = Some(name);
        } else if short_name.is_none() {
            short_name = Some(name);
        }
        consumed += 1;
    }

    let remainder = &tokens[consumed..];
    let mut inline_description = remainder.join(" ");
    if value_name.is_none() && remainder.len() == 1 && is_value_placeholder(remainder[0]) {
        value_name = Some(strip_placeholder_brackets(remainder[0]));
        inline_description = String::new();
    }

    // A placeholder states the value's shape as well as its existence:
    // `--max-count=<number>` typed `string` rejects the `3` a caller naturally
    // sends. Inference is shared with the pipeline's placeholder-recovery pass
    // so both paths type the same wording the same way.
    let value_type = match value_name.as_deref() {
        Some(placeholder) => {
            crate::scanner::parsers::value_placeholder::infer_value_type(placeholder)
        }
        None => ValueType::Boolean,
    };

    FlagLine {
        flag: ScannedFlag {
            long_name,
            short_name,
            description: String::new(),
            value_type,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name,
            value_optional,
            ..Default::default()
        },
        inline_description,
    }
}

/// Whether a lone trailing token names the flag's value rather than starting a
/// prose description.
///
/// Placeholders are written either all-uppercase (`FILE`), all-lowercase
/// (italic `format` once overstrike is stripped), or bracketed (`<path>`). A
/// capitalized word like `Display` begins a sentence instead.
fn is_value_placeholder(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if token.starts_with('<') && token.ends_with('>') {
        return true;
    }
    if token.contains(['.', ',', ';', ':', '(', ')']) {
        return false;
    }
    let has_lowercase = token.chars().any(char::is_lowercase);
    let has_uppercase = token.chars().any(char::is_uppercase);
    !(has_lowercase && has_uppercase)
}

/// Strip the brackets conventionally wrapping a value placeholder.
fn strip_placeholder_brackets(value: &str) -> String {
    value.trim_matches(['<', '>', '[', ']']).to_string()
}

/// Remove an optional segment written inside the option's own name, keeping the
/// affirmative spelling: `--[no-]verify` becomes `--verify`, and
/// `--reference[-if-able]` becomes `--reference`.
///
/// Left in place these produced property keys of `[no_]verify` and
/// `reference[_if_able]` — names no caller can render back to a flag the tool
/// accepts, and awkward identifiers for any consumer generating code from the
/// schema.
///
/// The negated spelling `--no-verify` is a real flag that this notation also
/// documents, and it is *not* emitted here: the parser produces one flag per
/// option line, and manufacturing a second one is a change to that contract
/// rather than a fix to a malformed name. It stays a known coverage gap.
fn strip_optional_group(name: &str) -> String {
    if !name.contains('[') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut depth = 0u32;
    for ch in name.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the overstrike encoding `man -P cat` produces for bold text.
    fn bold(text: &str) -> String {
        text.chars().map(|c| format!("{c}\u{8}{c}")).collect()
    }

    #[test]
    fn test_strip_overstrike_bold() {
        assert_eq!(strip_overstrike(&bold("DESCRIPTION")), "DESCRIPTION");
    }

    #[test]
    fn test_strip_overstrike_italic() {
        // Italic is encoded as `_\bX`.
        assert_eq!(strip_overstrike("_\u{8}f_\u{8}m_\u{8}t"), "fmt");
    }

    #[test]
    fn test_strip_overstrike_borrows_clean_text() {
        assert!(matches!(strip_overstrike("plain text"), Cow::Borrowed(_)));
    }

    #[test]
    fn test_extract_man_description_basic() {
        let man_text = r#"NAME
       git - the stupid content tracker

SYNOPSIS
       git [--version] [--help] <command> [<args>]

DESCRIPTION
       Git is a fast, scalable, distributed revision control system with
       an unusually rich command set.

OPTIONS
       --version
              Prints the Git suite version.
"#;
        let desc = extract_man_description(man_text);
        assert!(desc.contains("Git is a fast"));
    }

    #[test]
    fn test_extract_man_description_missing() {
        let man_text = "NAME\n       tool - does things\n\nOPTIONS\n       --help\n";
        assert!(extract_man_description(man_text).is_empty());
    }

    #[test]
    fn test_extract_man_description_truncated() {
        let long_desc = "A".repeat(300);
        let man_text = format!("DESCRIPTION\n       {long_desc}\n\nOPTIONS\n");
        assert_eq!(extract_man_description(&man_text).len(), 200);
    }

    #[test]
    fn test_extract_man_description_strips_overstrike() {
        // Regression: `man -P cat` output is overstriked, so the section header
        // reads `DDEESSCCRRIIPPTTIIOONN` and never matched the literal.
        let man_text = format!("{}\n       A useful tool.\n", bold("DESCRIPTION"));
        let cleaned = strip_overstrike(&man_text);
        assert_eq!(extract_man_description(&cleaned), "A useful tool.");
    }

    #[test]
    fn test_parse_man_page_nonexistent_tool() {
        let parser = ManPageParser;
        assert!(parser
            .parse_man_page("zzz_no_such_tool_xyz_12345")
            .is_none());
    }

    #[test]
    fn test_extract_man_examples_shell_prompt_layout() {
        // The `ls`/`grep` layout: prose, then the command behind a `$` prompt.
        let man_text = "\
EXAMPLES
     List the contents of the current working directory in long format:

           $ ls -l

     Show inode numbers as well:

           $ ls -lioF

SEE ALSO
     chflags(1)
";
        assert_eq!(
            extract_man_examples(man_text, "ls"),
            vec!["ls -l".to_string(), "ls -lioF".to_string()]
        );
    }

    #[test]
    fn test_extract_man_examples_bare_invocation_layout() {
        // The `tar`/`find` layout: no prompt, the command indented under prose.
        let man_text = "\
EXAMPLES
     The following creates a new archive called file.tar.gz:
           tar -czf file.tar.gz source.c source.h

     To view a detailed table of contents for this archive:
           tar -tvf file.tar.gz

SEE ALSO
";
        assert_eq!(
            extract_man_examples(man_text, "tar"),
            vec![
                "tar -czf file.tar.gz source.c source.h".to_string(),
                "tar -tvf file.tar.gz".to_string()
            ]
        );
    }

    #[test]
    fn test_extract_man_examples_skips_the_prose() {
        // Every one of these pages interleaves description with commands; only
        // the tool's own name separates the two in the bare layout.
        let man_text = "\
EXAMPLES
     The following creates a new archive called file.tar.gz that contains two
     files source.c and source.h:
           tar -czf file.tar.gz source.c source.h
";
        assert_eq!(
            extract_man_examples(man_text, "tar"),
            vec!["tar -czf file.tar.gz source.c source.h".to_string()]
        );
    }

    #[test]
    fn test_extract_man_examples_stops_at_the_next_section() {
        let man_text = "\
EXAMPLES
     $ ls -l

SEE ALSO
     $ ls -Z
";
        assert_eq!(
            extract_man_examples(man_text, "ls"),
            vec!["ls -l".to_string()]
        );
    }

    #[test]
    fn test_extract_man_examples_absent_section() {
        let man_text = "DESCRIPTION\n     A tool.\n\nSEE ALSO\n     other(1)\n";
        assert!(extract_man_examples(man_text, "tool").is_empty());
    }

    #[test]
    fn test_extract_man_examples_deduplicates() {
        let man_text = "EXAMPLES\n     $ ls -l\n\n     $ ls -l\n";
        assert_eq!(
            extract_man_examples(man_text, "ls"),
            vec!["ls -l".to_string()]
        );
    }

    #[test]
    fn test_extract_man_options_basic() {
        let man_text = r#"NAME
       mytool - does things

DESCRIPTION
       A useful tool.

OPTIONS
       --verbose
              Enable verbose output.

       --format
              Set output format.

ENVIRONMENT
       HOME   User home directory.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].long_name.as_deref(), Some("--verbose"));
        assert!(flags[0].description.contains("Enable verbose output"));
        assert_eq!(flags[1].long_name.as_deref(), Some("--format"));
        assert!(flags[1].description.contains("Set output format"));
    }

    #[test]
    fn test_extract_man_options_empty() {
        let man_text = "NAME\n       tool - does things\n\nDESCRIPTION\n       A tool.\n";
        assert!(extract_man_options(man_text).is_empty());
    }

    #[test]
    fn test_extract_man_options_multi_line_desc() {
        let man_text = r#"OPTIONS
       --output
              Specify the output file path. This flag accepts
              an absolute or relative filesystem path and will
              create intermediate directories as needed.

ENVIRONMENT
       HOME   User home.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].long_name.as_deref(), Some("--output"));
        assert!(flags[0]
            .description
            .contains("Specify the output file path"));
        assert!(flags[0]
            .description
            .contains("create intermediate directories"));
    }

    #[test]
    fn test_extract_man_options_bsd_description_layout() {
        // BSD pages (macOS `ls`, `cp`, ...) have no OPTIONS section: options are
        // listed inside DESCRIPTION and indented by 5 columns, not 7.
        let man_text = r#"DESCRIPTION
     For each operand that names a file, ls displays its name.

     The following options are available:

     -@      Display extended attribute keys and sizes.

     -A      Include directory entries whose names begin with a dot.

ENVIRONMENT
     COLUMNS  Screen width.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].short_name.as_deref(), Some("-@"));
        assert!(flags[0].description.contains("Display extended attribute"));
        assert_eq!(flags[1].short_name.as_deref(), Some("-A"));
    }

    #[test]
    fn test_is_bsd_option_intro_accepts_every_observed_wording() {
        // Each of these is a real macOS man page. They arrived one at a time as
        // bugs, which is why this is a pattern rather than a list.
        for wording in [
            "the following options are available:",
            "the options are as follows:",
            "the command line options are as follows:",
            "the generic options are as follows:",
        ] {
            assert!(is_bsd_option_intro(wording), "should accept: {wording}");
        }
        for wording in [
            "the tool reads options from a file",
            "these options are documented elsewhere",
            "the following environment variables are used",
        ] {
            assert!(!is_bsd_option_intro(wording), "should reject: {wording}");
        }
    }

    #[test]
    fn test_extract_man_options_bsd_command_line_options_intro() {
        // macOS `sort` uses a third wording. Missing it hides the entire option
        // block, which is silent rather than loud: the scan just returns fewer
        // flags.
        let man_text = r#"DESCRIPTION
     Sorts lines.

     The command line options are as follows:

     -r      Reverse the sort order.

ENVIRONMENT
     LANG   Locale.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].short_name.as_deref(), Some("-r"));
    }

    #[test]
    fn test_extract_man_options_bsd_options_are_as_follows() {
        let man_text = r#"DESCRIPTION
     Copies files.

     The options are as follows:

     -f      Force an existing file to be overwritten.

ENVIRONMENT
     HOME   User home.
"#;
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].short_name.as_deref(), Some("-f"));
    }

    #[test]
    fn test_extract_man_options_parses_alias_list() {
        let man_text = "OPTIONS\n       -a, --all\n              Stage all files.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].short_name.as_deref(), Some("-a"));
        assert_eq!(flags[0].long_name.as_deref(), Some("--all"));
        assert!(flags[0].description.contains("Stage all files"));
    }

    #[test]
    fn test_extract_man_options_boolean_flag_has_no_value() {
        let man_text = "OPTIONS\n       --verbose\n              Be verbose.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags[0].value_type, ValueType::Boolean);
        assert!(flags[0].value_name.is_none());
    }

    #[test]
    fn test_extract_man_options_detects_value_placeholder() {
        // A lone uppercase or lowercase trailing token names the value; a
        // capitalized word starts a description instead.
        let man_text = "OPTIONS\n       -o FILE\n              Write output.\n\n       -D format\n              Use format for dates.\n\n       -q      Quiet mode.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 3);
        assert_eq!(flags[0].value_name.as_deref(), Some("FILE"));
        // The placeholder states the value's shape as well as its existence.
        assert_eq!(flags[0].value_type, ValueType::Path);
        // An unrecognized placeholder stays a plain string rather than being
        // guessed at, since a wrong type rejects a value the tool accepts.
        assert_eq!(flags[1].value_name.as_deref(), Some("format"));
        assert_eq!(flags[1].value_type, ValueType::String);
        assert!(flags[2].value_name.is_none());
        assert!(flags[2].description.contains("Quiet mode"));
    }

    #[test]
    fn test_extract_man_options_detects_inline_value() {
        let man_text = "OPTIONS\n       --color=when\n              Colorize output.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].long_name.as_deref(), Some("--color"));
        assert_eq!(flags[0].value_name.as_deref(), Some("when"));
    }

    #[test]
    fn test_extract_man_options_strips_optional_value_bracket() {
        // Regression (#18): git's `--exec-path[=<path>]` produced a flag named
        // `--exec-path[`, hence a schema property key ending in `[` — a token
        // git rejects and an awkward identifier for a code generator.
        let man_text =
            "OPTIONS\n       --exec-path[=<path>]\n              Path to core programs.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].long_name.as_deref(), Some("--exec-path"));
        assert_eq!(flags[0].value_name.as_deref(), Some("path"));
        assert_eq!(flags[0].canonical_name(), "exec_path");
        assert!(
            flags[0].value_optional,
            "the bracket is the only marker of an optional value; dropping it loses the fact"
        );
    }

    #[test]
    fn test_extract_man_options_strips_optional_name_segment() {
        // `--[no-]verify` and `--reference[-if-able]` produced the property keys
        // `[no_]verify` and `reference[_if_able]`, neither of which renders back
        // to a flag any tool accepts.
        let man_text = "OPTIONS\n       --[no-]verify\n              Run hooks.\n\n       --reference[-if-able] <repo>\n              Reference repository.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].long_name.as_deref(), Some("--verify"));
        assert_eq!(flags[0].canonical_name(), "verify");
        assert_eq!(flags[1].long_name.as_deref(), Some("--reference"));
        assert_eq!(flags[1].canonical_name(), "reference");
    }

    #[test]
    fn test_extract_man_options_splits_an_attached_short_value() {
        // git spells `-n<num>`, `-S<string>`, `-G<regex>` with no separator.
        let man_text = "OPTIONS\n       -n<num>\n              Limit output.\n\n       -S<string>\n              Search for string.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[0].short_name.as_deref(), Some("-n"));
        assert_eq!(flags[0].value_name.as_deref(), Some("num"));
        assert_eq!(flags[0].canonical_name(), "n");
        assert_eq!(flags[1].short_name.as_deref(), Some("-S"));
        assert_eq!(flags[1].value_name.as_deref(), Some("string"));
    }

    #[test]
    fn test_extract_man_options_parses_alias_after_a_detached_value() {
        // `-n <number>, --max-count=<number>` stopped at `-n`, silently losing
        // every long alias git spells this way.
        let man_text =
            "OPTIONS\n       -n <number>, --max-count=<number>\n              Limit commits.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].short_name.as_deref(), Some("-n"));
        assert_eq!(flags[0].long_name.as_deref(), Some("--max-count"));
        assert_eq!(flags[0].value_name.as_deref(), Some("number"));
    }

    #[test]
    fn test_extract_man_options_skips_the_end_of_options_marker() {
        // A bare `--` is the end-of-options separator, not an option; it used to
        // become a flag whose property key was the empty string.
        let man_text = "OPTIONS\n       --\n              Do not interpret any more arguments as options.\n\n       --all\n              Everything.\n";
        let flags = extract_man_options(man_text);
        let names: Vec<&str> = flags
            .iter()
            .filter_map(|f| f.long_name.as_deref())
            .collect();
        assert_eq!(names, vec!["--all"]);
        assert!(
            flags.iter().all(|f| !f.canonical_name().is_empty()),
            "a flag with no usable name must not reach the schema"
        );
    }

    #[test]
    fn test_extract_man_options_ignores_deeper_indented_continuation() {
        // A description continuation that happens to start with `-` must not be
        // mistaken for a new flag.
        let man_text = "OPTIONS\n     -B      Force printing of non-printable characters\n             -like control codes- in file names.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].description.contains("-like control codes-"));
    }

    #[test]
    fn test_extract_man_options_stops_at_next_section() {
        let man_text = "OPTIONS\n       --keep\n              Keep it.\n\nEXAMPLES\n       --not-a-flag\n              Ignored.\n";
        let flags = extract_man_options(man_text);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].long_name.as_deref(), Some("--keep"));
    }
}
