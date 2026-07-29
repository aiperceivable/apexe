use tracing::{info, warn};

use super::protocol::{CliParser, ParsedHelp};

/// Selects the best parser for a given help text using priority-based routing.
pub struct ParserPipeline {
    parsers: Vec<Box<dyn CliParser>>,
}

impl ParserPipeline {
    /// Initialize with built-in parsers and optional plugins.
    ///
    /// If `plugins` is None, only built-in parsers are loaded.
    pub fn new(plugins: Option<Vec<Box<dyn CliParser>>>) -> Self {
        let mut parsers: Vec<Box<dyn CliParser>> = vec![
            Box::new(super::parsers::man_help::ManHelpParser),
            Box::new(super::parsers::bsd_usage::BsdUsageParser),
            Box::new(super::parsers::gnu::GnuHelpParser),
            Box::new(super::parsers::click::ClickHelpParser),
            Box::new(super::parsers::cobra::CobraHelpParser),
            Box::new(super::parsers::clap_parser::ClapHelpParser),
        ];

        // Add plugin parsers
        if let Some(ext_parsers) = plugins {
            parsers.extend(ext_parsers);
        }

        parsers.sort_by_key(|p| p.priority());

        Self { parsers }
    }

    /// Parse help text using the highest-priority matching parser.
    ///
    /// Priority resolution:
    /// 1. Try each parser in priority order (ascending).
    /// 2. Fallback: return ParsedHelp with raw text as description.
    ///
    /// This function knows nothing about user overrides. It used to accept a
    /// `user_override: Option<&Path>` that loaded a `ParsedHelp` YAML document,
    /// but that hook sat below the layer where the tool's *variant* is known,
    /// so it could not express "BSD `ls`, but not the Homebrew GNU one". It has
    /// been lifted into [`crate::scanner::OverlayStore`], which owns the single
    /// override path (`apexe scan --overlay <PATH>`).
    pub fn parse(&self, help_text: &str, tool_name: &str) -> ParsedHelp {
        // Normalize line endings once here so every parser sees `\n`. The
        // parsers rely on `(?m)^...$`-anchored regexes and byte-offset section
        // slicing, both of which misbehave on CRLF (`\r\n`) help text (e.g. a
        // Windows build's `--help`): `$` won't match a header line that ends in
        // `\r`, and offset math undercounts. Fixing it at the boundary lets all
        // parsers stay `\n`-only.
        let normalized = normalize_line_endings(help_text);

        // Try parsers in priority order
        for parser in &self.parsers {
            if parser.can_parse(&normalized, tool_name) {
                match parser.parse(&normalized, tool_name) {
                    Ok(mut result) => {
                        info!(parser = parser.name(), "Parsed help text");
                        // Applied here rather than inside each parser: every
                        // format has option lines whose value placeholder its
                        // regex does not anticipate, and a flag that takes a
                        // value but is typed boolean is unusable in both
                        // directions. See `value_placeholder`.
                        super::value_placeholder::recover_values_from_descriptions(
                            &mut result.flags,
                        );
                        return result;
                    }
                    Err(e) => {
                        warn!(parser = parser.name(), "Parser failed, trying next: {e}");
                    }
                }
            }
        }

        // Fallback
        warn!(tool = tool_name, "No parser matched, using raw help text");
        ParsedHelp {
            description: normalized.chars().take(500).collect(),
            ..Default::default()
        }
    }

    /// Return the number of registered parsers.
    pub fn parser_count(&self) -> usize {
        self.parsers.len()
    }
}

/// Normalize CRLF (`\r\n`) and lone CR (`\r`) line endings to `\n`.
///
/// Returns the input unchanged (borrowed) when it contains no carriage return,
/// so the common Unix case allocates nothing.
fn normalize_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::protocol::CliParser;

    struct TestParser {
        parser_name: &'static str,
        prio: u32,
        can: bool,
        fail: bool,
    }

    impl CliParser for TestParser {
        fn name(&self) -> &str {
            self.parser_name
        }
        fn priority(&self) -> u32 {
            self.prio
        }
        fn can_parse(&self, _help_text: &str, _tool_name: &str) -> bool {
            self.can
        }
        fn parse(&self, _help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
            if self.fail {
                Err(anyhow::anyhow!("intentional failure"))
            } else {
                Ok(ParsedHelp {
                    description: format!("parsed by {}", self.parser_name),
                    ..Default::default()
                })
            }
        }
    }

    // T25: Pipeline initialization
    #[test]
    fn test_pipeline_new_builtin_count() {
        let pipeline = ParserPipeline::new(None);
        assert_eq!(pipeline.parser_count(), 6);
    }

    #[test]
    fn test_pipeline_new_with_plugin() {
        let plugin: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "plugin",
            prio: 200,
            can: true,
            fail: false,
        });
        let pipeline = ParserPipeline::new(Some(vec![plugin]));
        assert_eq!(pipeline.parser_count(), 7);
    }

    #[test]
    fn test_pipeline_parsers_sorted_by_priority() {
        let p1: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "low",
            prio: 50,
            can: true,
            fail: false,
        });
        let p2: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "high",
            prio: 300,
            can: true,
            fail: false,
        });
        let pipeline = ParserPipeline::new(Some(vec![p2, p1]));
        // first parser should have lowest priority number
        assert_eq!(pipeline.parsers[0].priority(), 50);
    }

    // T26: Pipeline parse routing
    #[test]
    fn test_pipeline_uses_gnu_for_gnu_help() {
        let pipeline = ParserPipeline::new(None);
        let gnu_help = "Description\n\nUsage: tool [OPTIONS]\n\nOptions:\n  -v, --verbose          Be verbose\n";
        let result = pipeline.parse(gnu_help, "tool");
        assert!(!result.description.is_empty());
    }

    #[test]
    fn test_pipeline_uses_cobra_for_cobra_help() {
        let pipeline = ParserPipeline::new(None);
        let cobra_help =
            "A tool\n\nAvailable Commands:\n  sub1  First\n\nFlags:\n  --verbose  Be verbose\n";
        let result = pipeline.parse(cobra_help, "tool");
        assert!(result.subcommand_names.contains(&"sub1".to_string()));
    }

    #[test]
    fn test_pipeline_normalizes_crlf_help() {
        // Regression: CRLF help must parse identically to LF. Parser section
        // regexes (`^Flags:$`) and byte-offset slicing break on `\r\n`; the
        // pipeline normalizes line endings so every parser sees `\n`.
        let pipeline = ParserPipeline::new(None);
        let lf = "A tool\n\nAvailable Commands:\n  sub1  First\n\nFlags:\n  --alpha  Alpha\n  --bravo  Bravo\n";
        let crlf = lf.replace('\n', "\r\n");

        let lf_result = pipeline.parse(lf, "tool");
        let crlf_result = pipeline.parse(&crlf, "tool");

        let names = |p: &ParsedHelp| {
            let mut v: Vec<String> = p.flags.iter().filter_map(|f| f.long_name.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(
            names(&crlf_result),
            vec!["--alpha".to_string(), "--bravo".to_string()],
            "CRLF flags not parsed"
        );
        assert_eq!(
            names(&lf_result),
            names(&crlf_result),
            "CRLF differs from LF"
        );
    }

    #[test]
    fn test_pipeline_reports_help_format() {
        // The matched parser now records its format on ParsedHelp so discovery
        // can tag each subcommand (was hardcoded to Unknown).
        use crate::models::HelpFormat;
        let pipeline = ParserPipeline::new(None);

        let cobra = "A tool\n\nAvailable Commands:\n  sub1  First\n\nFlags:\n  --v  V\n";
        assert_eq!(pipeline.parse(cobra, "tool").help_format, HelpFormat::Cobra);

        // Unmatched text falls back to Unknown.
        assert_eq!(
            pipeline.parse("random gibberish", "tool").help_format,
            HelpFormat::Unknown
        );
    }

    #[test]
    fn test_pipeline_routes_bundled_usage_to_bsd_parser() {
        // The GNU parser also accepts this text (it contains "usage:") but
        // extracts nothing from it, so the BSD parser must be tried first.
        let pipeline = ParserPipeline::new(None);
        let bsd = "usage: ls [-@ABCFGHILOPRSTUWXabcdefghiklmnopqrstuvwxy1%,] [--color=when] [-D format] [file ...]\n";
        let result = pipeline.parse(bsd, "ls");
        assert!(
            result.flags.len() > 20,
            "expected bundled group expanded, got {} flags",
            result.flags.len()
        );
    }

    #[test]
    fn test_pipeline_still_routes_gnu_help_to_gnu_parser() {
        // Adding the BSD parser must not steal ordinary GNU help.
        let pipeline = ParserPipeline::new(None);
        let gnu = "Usage: tool [OPTIONS]\n\nOptions:\n  -m, --message MSG  Use the given message\n  -a, --all          Stage all\n";
        let result = pipeline.parse(gnu, "tool");
        assert_eq!(result.help_format, crate::models::HelpFormat::Gnu);
        assert!(result
            .flags
            .iter()
            .any(|flag| flag.long_name.as_deref() == Some("--message")));
    }

    #[test]
    fn test_pipeline_extracts_git_style_grouped_subcommands() {
        // git introduces its command list in prose and splits it into blank
        // line separated groups; both previously defeated subcommand extraction.
        let pipeline = ParserPipeline::new(None);
        let git_help = "usage: git [--version] <command> [<args>]\n\nThese are common Git commands used in various situations:\n\nstart a working area (see also: git help tutorial)\n   clone      Clone a repository into a new directory\n   init       Create an empty Git repository\n\nwork on the current change (see also: git help everyday)\n   add        Add file contents to the index\n   restore    Restore working tree files\n";
        let result = pipeline.parse(git_help, "git");
        assert_eq!(
            result.subcommand_names,
            vec!["clone", "init", "add", "restore"]
        );
    }

    #[test]
    fn test_pipeline_fallback_on_no_match() {
        let pipeline = ParserPipeline::new(None);
        let result = pipeline.parse("random gibberish text", "tool");
        assert_eq!(result.description, "random gibberish text");
        assert!(result.flags.is_empty());
    }

    #[test]
    fn test_pipeline_tries_next_on_parse_failure() {
        let p1: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "failing",
            prio: 10,
            can: true,
            fail: true,
        });
        let p2: Box<dyn CliParser> = Box::new(TestParser {
            parser_name: "working",
            prio: 20,
            can: true,
            fail: false,
        });
        let pipeline = ParserPipeline::new(Some(vec![p1, p2]));
        let result = pipeline.parse("any text", "tool");
        assert_eq!(result.description, "parsed by working");
    }

    #[test]
    fn test_pipeline_parse_is_pure_text_to_structure() {
        // The former `user_override` parameter is gone: overriding a scan is
        // now the overlay mechanism's job, at the layer that knows the tool's
        // variant. `parse` must therefore depend on nothing but its arguments,
        // so the same help text always yields the same structure.
        let pipeline = ParserPipeline::new(None);
        let help = "Usage: tool [OPTIONS]\n\nOptions:\n  -v, --verbose  Be verbose\n";
        let first = pipeline.parse(help, "tool");
        let second = pipeline.parse(help, "tool");
        assert_eq!(first.description, second.description);
        assert_eq!(first.flags.len(), second.flags.len());
    }
}
