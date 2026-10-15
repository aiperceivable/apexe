use textwrap::{core::display_width, Options, WordSplitter};

const DEFAULT_WIDTH: usize = 80;
const COLUMN_GAP: usize = 2;
const MIN_DESCRIPTION_WIDTH: usize = 20;

pub(super) fn terminal_width() -> usize {
    let detected = terminal_size::terminal_size_of(std::io::stdout())
        .map(|(terminal_size::Width(width), _)| usize::from(width));
    resolve_width(detected, std::env::var("COLUMNS").ok().as_deref())
}

fn resolve_width(detected: Option<usize>, columns: Option<&str>) -> usize {
    detected
        .filter(|width| *width > 0)
        .or_else(|| columns?.parse::<usize>().ok().filter(|width| *width > 0))
        .unwrap_or(DEFAULT_WIDTH)
}

pub(super) fn format_table(modules: &[(&str, &str)], width: usize) -> String {
    let width = width.max(1);
    let id_width = modules
        .iter()
        .map(|(id, _)| display_width(id))
        .max()
        .unwrap_or(0)
        .max("MODULE ID".len());
    let use_columns = width >= id_width + COLUMN_GAP + MIN_DESCRIPTION_WIDTH;
    let mut output = if use_columns {
        format!("{:<id_width$}  DESCRIPTION\n", "MODULE ID")
    } else {
        String::new()
    };
    let rows: Vec<String> = modules
        .iter()
        .map(|(id, description)| {
            if use_columns {
                format_columns(id, description, id_width, width)
            } else {
                format_stacked(id, description, width)
            }
        })
        .collect();
    output.push_str(&rows.join("\n\n"));
    output.push_str(&format!("\n\n{} module(s) found.\n", modules.len()));
    output
}

fn format_columns(id: &str, description: &str, id_width: usize, width: usize) -> String {
    let padding = " ".repeat(id_width - display_width(id) + COLUMN_GAP);
    let initial_indent = format!("{id}{padding}");
    let subsequent_indent = " ".repeat(id_width + COLUMN_GAP);
    let options = Options::new(width)
        .initial_indent(&initial_indent)
        .subsequent_indent(&subsequent_indent)
        .word_splitter(WordSplitter::NoHyphenation);
    textwrap::fill(description, options).trim_end().to_string()
}

fn format_stacked(id: &str, description: &str, width: usize) -> String {
    let mut output = textwrap::fill(
        id,
        Options::new(width).word_splitter(WordSplitter::NoHyphenation),
    );
    if !description.is_empty() {
        let indent = if width > COLUMN_GAP { "  " } else { "" };
        let options = Options::new(width)
            .initial_indent(indent)
            .subsequent_indent(indent)
            .word_splitter(WordSplitter::NoHyphenation);
        output.push('\n');
        output.push_str(&textwrap::fill(description, options));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_width_prefers_detected_terminal_and_handles_invalid_columns() {
        assert_eq!(resolve_width(Some(120), Some("40")), 120);
        assert_eq!(resolve_width(None, Some("100")), 100);
        for columns in [None, Some(""), Some("0"), Some("-1"), Some("invalid")] {
            assert_eq!(resolve_width(None, columns), DEFAULT_WIDTH);
        }
        assert_eq!(resolve_width(Some(0), Some("60")), 60);
    }

    #[test]
    fn test_format_table_aligns_continuations_and_separates_modules() {
        let output = format_table(
            &[
                (
                    "cli.cat",
                    "Read files sequentially and write them to standard output.",
                ),
                ("cli.chmod", "Change file modes."),
            ],
            40,
        );
        assert_eq!(
            output,
            concat!(
                "MODULE ID  DESCRIPTION\n",
                "cli.cat    Read files sequentially and\n",
                "           write them to standard\n",
                "           output.\n\n",
                "cli.chmod  Change file modes.\n\n",
                "2 module(s) found.\n"
            )
        );
    }

    #[test]
    fn test_format_table_adapts_to_terminal_width_without_losing_words() {
        let description =
            "Read files sequentially and write their complete contents to standard output.";
        for width in [32, 60, 120] {
            let output = format_table(&[("cli.cat", description)], width);
            assert!(output.lines().all(|line| display_width(line) <= width));
            let reconstructed = output
                .lines()
                .skip(1)
                .take_while(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let words: Vec<&str> = reconstructed.split_whitespace().skip(1).collect();
            assert_eq!(words, description.split_whitespace().collect::<Vec<_>>());
        }
    }

    #[test]
    fn test_format_table_stacks_long_ids_in_narrow_terminals() {
        let output = format_table(
            &[(
                "cli.command.with.a.long.name",
                "Read every file completely.",
            )],
            24,
        );
        assert!(output.starts_with("cli.command.with.a.long.\nname\n  Read every file"));
        assert!(output.contains("\n  completely."));
        assert!(output.lines().all(|line| display_width(line) <= 24));
    }

    #[test]
    fn test_format_table_wraps_long_words_without_truncation() {
        let word = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let output = format_table(&[("cli.cat", word)], 32);
        let reconstructed: String = output
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .flat_map(str::split_whitespace)
            .skip(1)
            .collect();
        assert_eq!(reconstructed, word);
        assert!(output.lines().all(|line| display_width(line) <= 32));
    }

    #[test]
    fn test_format_table_measures_unicode_display_width_and_preserves_paragraphs() {
        let description = "\u{8bfb}\u{53d6}\u{6587}\u{4ef6} cafe\u{301} contents without truncation.\n\nSecond paragraph.";
        let output = format_table(&[("cli.cat", description)], 32);
        assert!(output.lines().all(|line| display_width(line) <= 32));
        assert!(output.contains("\u{8bfb}\u{53d6}\u{6587}\u{4ef6}"));
        assert!(output.contains("cafe\u{301}"));
        assert!(output.contains("Second paragraph."));
        assert!(output.contains("\n           \n"));
    }

    #[test]
    fn test_format_table_handles_empty_descriptions_and_tiny_widths() {
        assert!(format_table(&[("cli.cat", "")], 80).contains("\ncli.cat\n"));
        for width in [0, 1, 2, 10] {
            let output = format_table(&[("cli.cat", "Read all files.")], width);
            assert!(!output.contains("..."));
            assert!(output.contains("1 module(s) found."));
        }
    }
}
