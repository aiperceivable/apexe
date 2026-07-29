//! Parser for tools whose `--help` *is* a man page.
//!
//! `git log --help` does not print help; it runs `man git-log`. So does every
//! other git subcommand, and the same delegation shows up in other tools that
//! ship roff documentation. Nothing in the Tier 1 pipeline recognized that
//! shape, so the parse yielded no flags and no positional args, and the module
//! for a subcommand was assembled from the *tool's* global flags alone —
//! `cli.git.log`'s 25 properties were `--git-dir`, `--paginate`, `--html-path`
//! and friends, not one of which belongs to `git log`. An agent reading that
//! contract is told `git log` accepts `--paginate` and nothing else, which is a
//! description of `git` filed under the name of `git log`.
//!
//! Tier 2 already knows how to read a man page — it is how `ls` gets its
//! options, since BSD `--help` is a single bundled usage line. This parser
//! reuses that machinery at Tier 1, where the text arrives from the
//! subcommand's own `--help` rather than from a `man` lookup, so a subcommand
//! finally describes itself.

use crate::models::HelpFormat;
use crate::scanner::man_page::{
    extract_man_examples, extract_man_options, extract_man_summary, extract_man_synopsis,
    is_man_page,
};
use crate::scanner::parsers::gnu::detect_structured_output;
use crate::scanner::parsers::positional_args::extract_args_from_usage_line;
use crate::scanner::protocol::{CliParser, ParsedHelp};

/// The bare command name, for a `tool_name` that may be an absolute path.
///
/// `extract_man_examples` accepts a bare invocation only when the line starts
/// with the tool's own name, which is what separates a command from the prose
/// around it. The scanner accepts a tool as either a bare name (`git`) or a
/// path (`/usr/bin/git`) — AP Studio passes the latter — and forwards whichever
/// it was given. Left unnormalized, scanning by path matched no EXAMPLES line
/// at all, so the same tool yielded a different contract depending on how it
/// was named. Tier 2 already normalizes; this brings Tier 1 in line.
fn documentation_name(tool_name: &str) -> &str {
    std::path::Path::new(tool_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(tool_name)
}

/// Tier 1 parser for man-page-shaped help output.
pub struct ManHelpParser;

impl CliParser for ManHelpParser {
    fn name(&self) -> &str {
        "man"
    }

    fn priority(&self) -> u32 {
        // Ahead of every other built-in. A man page's prose routinely contains
        // the words "usage:" and "Options:", so the BSD (95) and GNU (100)
        // parsers both accept one and then extract almost nothing from it.
        // `can_parse` here is far more specific — two unindented section
        // headers — so trying it first costs nothing and settles the ambiguity.
        90
    }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        is_man_page(help_text)
    }

    fn parse(&self, help_text: &str, tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let flags = extract_man_options(help_text);
        // The synopsis is the only place a man page states the operands:
        // `git log [<options>] [<revision-range>] [[--] <path>...]`. Without it
        // `git log <revision-range>` cannot be expressed at all, even though
        // the module's own documentation field spells it out.
        let positional_args = extract_args_from_usage_line(&extract_man_synopsis(help_text));
        let structured_output = detect_structured_output(&flags, help_text);

        Ok(ParsedHelp {
            description: extract_man_summary(help_text),
            flags,
            positional_args,
            // A man page documents one command. Subcommand *discovery* stays
            // with the parsers that read a "Commands:" listing; a page's
            // cross-references in SEE ALSO are related commands, not children.
            subcommand_names: Vec::new(),
            examples: extract_man_examples(help_text, documentation_name(tool_name)),
            structured_output,
            help_format: HelpFormat::Man,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down `git log --help`, in the exact layout git emits.
    const GIT_LOG_MAN: &str = "\
GIT-LOG(1)                        Git Manual                        GIT-LOG(1)

NAME
       git-log - Show commit logs

SYNOPSIS
       git log [<options>] [<revision-range>] [[--] <path>...]

DESCRIPTION
       Shows the commit logs.

OPTIONS
       --follow
           Continue listing the history of a file beyond renames.

       -n <number>, --max-count=<number>
           Limit the number of commits to output.

       --oneline
           This is a shorthand for \"--pretty=oneline --abbrev-commit\".

GIT
       Part of the git(1) suite
";

    fn parse(text: &str) -> ParsedHelp {
        ManHelpParser
            .parse(text, "git")
            .expect("parse should succeed")
    }

    #[test]
    fn test_man_parser_claims_a_man_page() {
        assert!(ManHelpParser.can_parse(GIT_LOG_MAN, "git"));
    }

    #[test]
    fn test_man_parser_declines_ordinary_help_output() {
        // The GNU/BSD parsers must keep their own text. `SYNOPSIS` in prose is
        // not a section header.
        let gnu = "Usage: tool [OPTIONS]\n\nOptions:\n  -v, --verbose  Be verbose\n";
        assert!(!ManHelpParser.can_parse(gnu, "tool"));
        assert!(!ManHelpParser.can_parse("see the SYNOPSIS above", "tool"));
    }

    #[test]
    fn test_man_parser_extracts_the_subcommands_own_flags() {
        // Regression (#18): `cli.git.log` carried git's 25 global flags and not
        // one option belonging to `git log`.
        let parsed = parse(GIT_LOG_MAN);
        let names: Vec<&str> = parsed
            .flags
            .iter()
            .filter_map(|f| f.long_name.as_deref())
            .collect();

        assert!(names.contains(&"--follow"), "{names:?}");
        assert!(names.contains(&"--max-count"), "{names:?}");
        assert!(names.contains(&"--oneline"), "{names:?}");
    }

    #[test]
    fn test_man_parser_carries_the_synopsis_operands() {
        // `git log <revision-range>` and `git log -- <path>` were inexpressible:
        // `positional` was empty for every cli.git.* module.
        let parsed = parse(GIT_LOG_MAN);
        let names: Vec<&str> = parsed
            .positional_args
            .iter()
            .map(|a| a.name.as_str())
            .collect();

        assert!(names.contains(&"revision-range"), "{names:?}");
        assert!(names.contains(&"path"), "{names:?}");
        assert!(
            !names.contains(&"options"),
            "the option group is not an operand: {names:?}"
        );
    }

    #[test]
    fn test_man_parser_uses_the_name_section_as_the_description() {
        let parsed = parse(GIT_LOG_MAN);
        assert_eq!(parsed.description, "Show commit logs");
    }

    #[test]
    fn test_man_parser_reports_its_format() {
        assert_eq!(parse(GIT_LOG_MAN).help_format, HelpFormat::Man);
    }

    #[test]
    fn test_man_parser_finds_examples_when_scanned_by_absolute_path() {
        // Regression: the scanner forwards whatever it was given, and AP Studio
        // passes an absolute path. `extract_man_examples` matches a bare
        // invocation by prefix, so `/usr/bin/git` matched nothing and the same
        // tool produced a different contract depending on how it was named.
        const WITH_EXAMPLES: &str = "\
GIT-LOG(1)                        Git Manual                        GIT-LOG(1)

NAME
       git-log - Show commit logs

SYNOPSIS
       git log [<options>] [<revision-range>] [[--] <path>...]

EXAMPLES
       git log --no-merges
           Show the whole commit history, but skip any merges.
";
        let by_name = ManHelpParser.parse(WITH_EXAMPLES, "git").unwrap();
        let by_path = ManHelpParser.parse(WITH_EXAMPLES, "/usr/bin/git").unwrap();

        assert_eq!(by_name.examples, vec!["git log --no-merges"]);
        assert_eq!(
            by_path.examples, by_name.examples,
            "a path and a bare name must yield the same contract"
        );
    }

    #[test]
    fn test_man_parser_finds_no_subcommands() {
        // SEE ALSO lists sibling commands, not children; inventing subcommands
        // from a man page would generate modules for commands that are not
        // reachable through this one.
        assert!(parse(GIT_LOG_MAN).subcommand_names.is_empty());
    }
}
