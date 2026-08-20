use std::time::Duration;

use tracing::warn;

use super::man_page::strip_overstrike;
use super::pipeline::ParserPipeline;
use crate::models::ScannedCommand;

/// Recursively discovers and scans subcommands.
pub struct SubcommandDiscovery<'a> {
    pipeline: &'a ParserPipeline,
    max_depth: u32,
    /// Per-subprocess wall-clock timeout for each `<tool> <sub> --help` probe.
    timeout: Duration,
}

impl<'a> SubcommandDiscovery<'a> {
    pub fn new(pipeline: &'a ParserPipeline, max_depth: u32, timeout: Duration) -> Self {
        Self {
            pipeline,
            max_depth,
            timeout,
        }
    }

    /// Scan one subcommand and its nested subcommands.
    ///
    /// `None` when the subcommand's `--help` produced nothing, which is a skip
    /// rather than a failure: a tool listing a subcommand it cannot describe
    /// should not take the rest of the scan with it.
    fn scan_subcommand(
        &self,
        tool_name: &str,
        parent_command: &[String],
        sub_name: &str,
        depth: u32,
    ) -> Option<ScannedCommand> {
        let mut full_cmd: Vec<String> = parent_command.to_vec();
        full_cmd.push(sub_name.to_string());

        let help_text = self.run_help(tool_name, &full_cmd)?;
        let parsed = self.pipeline.parse(&help_text, tool_name);

        let nested = if parsed.subcommand_names.is_empty() {
            Vec::new()
        } else {
            self.discover(tool_name, &full_cmd, &parsed.subcommand_names, depth + 1)
        };

        Some(ScannedCommand {
            name: sub_name.to_string(),
            full_command: full_cmd.join(" "),
            description: parsed.description,
            flags: parsed.flags,
            positional_args: parsed.positional_args,
            subcommands: nested,
            examples: parsed.examples,
            help_format: parsed.help_format,
            structured_output: parsed.structured_output,
            end_of_options: false,
            raw_help: help_text,
        })
    }

    /// Scan every named subcommand, recursing until `max_depth`.
    /// Recursively discover subcommands.
    ///
    /// Returns a list of ScannedCommand with nested subcommands.
    pub fn discover(
        &self,
        tool_name: &str,
        parent_command: &[String],
        subcommand_names: &[String],
        depth: u32,
    ) -> Vec<ScannedCommand> {
        if depth >= self.max_depth {
            warn!(
                tool = tool_name,
                depth = depth,
                "Max subcommand depth reached"
            );
            return Vec::new();
        }

        subcommand_names
            .iter()
            .filter_map(|sub_name| self.scan_subcommand(tool_name, parent_command, sub_name, depth))
            .collect()
    }

    /// Run `<tool> <subcommand...> --help` and capture output.
    ///
    /// Returns stdout if non-empty, falls back to stderr, or None if both empty.
    ///
    /// The captured text is passed through [`strip_overstrike`] because several
    /// tools delegate `--help` to their man page: `git log --help` runs `man
    /// git-log`, whose output carries nroff overstrike sequences (`l\x08l`).
    /// That text is stored verbatim as the module's `documentation`, so leaving
    /// it encoded makes the field both unreadable and roughly twice its needed
    /// size — and no parser recognizes a section header spelled `N\x08NA\x08AME`.
    pub fn run_help(&self, tool_name: &str, full_cmd: &[String]) -> Option<String> {
        let mut args: Vec<&str> = full_cmd[1..].iter().map(|s| s.as_str()).collect();
        args.push("--help");

        let output = super::exec::run_with_timeout(tool_name, &args, self.timeout).ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Some tools output help to stderr
        let text = if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            stderr
        } else if !stdout.trim().is_empty() {
            stdout
        } else {
            warn!(command = %full_cmd.join(" "), "Empty help output");
            return None;
        };
        Some(strip_overstrike(&text).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // T28: basic discovery with mocked help
    // We test discovery logic using the pipeline with synthetic help text.
    // Real subprocess tests are in integration tests.

    #[test]
    fn test_discovery_max_depth_zero_returns_empty() {
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 0, std::time::Duration::from_secs(5));
        let result = discovery.discover("tool", &["tool".into()], &["sub1".into()], 0);
        assert!(result.is_empty());
    }

    // T29: max depth enforcement
    #[test]
    fn test_discovery_respects_max_depth() {
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 1, std::time::Duration::from_secs(5));
        // At depth 1, should return empty
        let result = discovery.discover("tool", &["tool".into()], &["sub1".into()], 1);
        assert!(result.is_empty());
    }

    // T30: run_help stdout/stderr fallback
    #[test]
    fn test_run_help_captures_stdout() {
        // echo outputs to stdout
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 2, std::time::Duration::from_secs(5));
        // Use a tool that produces stdout on --help
        let result = discovery.run_help("echo", &["echo".into(), "hello".into()]);
        // echo will output "hello --help" to stdout
        assert!(result.is_some());
    }

    #[test]
    fn test_run_help_strips_man_page_overstrike() {
        // `git log --help` delegates to `man git-log`, whose output encodes bold
        // as `l\x08l`. That text is stored as the module's `documentation`, so
        // it has to be collapsed at the point of capture. `printf` stands in for
        // a tool that pipes its man page out.
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 2, std::time::Duration::from_secs(5));
        let result = discovery
            .run_help(
                "printf",
                &["printf".into(), "N\\bNA\\bAM\\bME\\bE\\n".into()],
            )
            .expect("printf writes to stdout");
        assert!(
            result.contains("NAME"),
            "overstrike not collapsed: {result:?}"
        );
        assert!(!result.contains('\u{8}'), "backspace survived: {result:?}");
    }

    #[test]
    fn test_run_help_nonexistent_tool() {
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 2, std::time::Duration::from_secs(5));
        let result = discovery.run_help("zzz_no_such_tool_xyz", &["zzz_no_such_tool_xyz".into()]);
        assert!(result.is_none());
    }
}
