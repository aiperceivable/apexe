use std::borrow::Cow;
use std::process::Command;

use crate::models::{ScannedFlag, ValueType};
use crate::scanner::protocol::ParsedHelp;

/// Intro lines that start an option list embedded in a BSD `DESCRIPTION`
/// section, which has no dedicated `OPTIONS` header.
const BSD_OPTION_INTROS: &[&str] = &[
    "the following options are available",
    "the options are as follows",
    // macOS `sort` words it differently again; without this its whole option
    // block is invisible to Tier 2.
    "the command line options are as follows",
];

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
        let flags = extract_man_options(&text);

        if description.is_empty() && flags.is_empty() {
            return None;
        }

        Some(ParsedHelp {
            description,
            flags,
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
    let lowered = trimmed.to_ascii_lowercase();
    BSD_OPTION_INTROS
        .iter()
        .any(|intro| lowered.starts_with(intro))
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
/// Handles alias lists (`-a, --all`), inline values (`--color=when`), and
/// separate value placeholders (`-D format`, `-o FILE`).
fn parse_flag_line(trimmed: &str) -> FlagLine {
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let mut short_name: Option<String> = None;
    let mut long_name: Option<String> = None;
    let mut value_name: Option<String> = None;
    let mut consumed = 0;

    while consumed < tokens.len() {
        let token = tokens[consumed].trim_end_matches(',');
        if !token.starts_with('-') || token.len() < 2 {
            break;
        }
        let (name, inline_value) = match token.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (token, None),
        };
        if let Some(value) = inline_value.filter(|value| !value.is_empty()) {
            value_name = Some(strip_placeholder_brackets(value));
        }
        if name.starts_with("--") {
            long_name = Some(name.to_string());
        } else if short_name.is_none() {
            short_name = Some(name.to_string());
        }
        consumed += 1;
    }

    let remainder = &tokens[consumed..];
    let mut inline_description = remainder.join(" ");
    if value_name.is_none() && remainder.len() == 1 && is_value_placeholder(remainder[0]) {
        value_name = Some(strip_placeholder_brackets(remainder[0]));
        inline_description = String::new();
    }

    let value_type = if value_name.is_some() {
        ValueType::String
    } else {
        ValueType::Boolean
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
        assert_eq!(flags[0].value_type, ValueType::String);
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
