use std::process::Command;

use tracing::warn;

use super::pipeline::ParserPipeline;
use crate::models::{HelpFormat, ScannedCommand};

/// Recursively discovers and scans subcommands.
pub struct SubcommandDiscovery<'a> {
    pipeline: &'a ParserPipeline,
    max_depth: u32,
}

impl<'a> SubcommandDiscovery<'a> {
    pub fn new(pipeline: &'a ParserPipeline, max_depth: u32) -> Self {
        Self {
            pipeline,
            max_depth,
        }
    }

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

        let mut commands = Vec::new();

        for sub_name in subcommand_names {
            let mut full_cmd: Vec<String> = parent_command.to_vec();
            full_cmd.push(sub_name.clone());

            // Run --help for this subcommand
            let help_text = match self.run_help(tool_name, &full_cmd) {
                Some(text) => text,
                None => continue,
            };

            // Parse help text
            let parsed = self.pipeline.parse(&help_text, tool_name, None);

            // Recursively discover nested subcommands
            let nested = if !parsed.subcommand_names.is_empty() {
                self.discover(tool_name, &full_cmd, &parsed.subcommand_names, depth + 1)
            } else {
                Vec::new()
            };

            commands.push(ScannedCommand {
                name: sub_name.clone(),
                full_command: full_cmd.join(" "),
                description: parsed.description,
                flags: parsed.flags,
                positional_args: parsed.positional_args,
                subcommands: nested,
                examples: parsed.examples,
                help_format: HelpFormat::Unknown,
                structured_output: parsed.structured_output,
                raw_help: help_text,
            });
        }

        commands
    }

    /// Run `<tool> <subcommand...> --help` and capture output.
    ///
    /// Returns stdout if non-empty, falls back to stderr, or None if both empty.
    pub fn run_help(&self, tool_name: &str, full_cmd: &[String]) -> Option<String> {
        let mut args: Vec<&str> = full_cmd[1..].iter().map(|s| s.as_str()).collect();
        args.push("--help");

        let output = Command::new(tool_name).args(&args).output().ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Some tools output help to stderr
        if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            Some(stderr)
        } else if !stdout.trim().is_empty() {
            Some(stdout)
        } else {
            warn!(command = %full_cmd.join(" "), "Empty help output");
            None
        }
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
        let discovery = SubcommandDiscovery::new(&pipeline, 0);
        let result = discovery.discover("tool", &["tool".into()], &["sub1".into()], 0);
        assert!(result.is_empty());
    }

    // T29: max depth enforcement
    #[test]
    fn test_discovery_respects_max_depth() {
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 1);
        // At depth 1, should return empty
        let result = discovery.discover("tool", &["tool".into()], &["sub1".into()], 1);
        assert!(result.is_empty());
    }

    // T30: run_help stdout/stderr fallback
    #[test]
    fn test_run_help_captures_stdout() {
        // echo outputs to stdout
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 2);
        // Use a tool that produces stdout on --help
        let result = discovery.run_help("echo", &["echo".into(), "hello".into()]);
        // echo will output "hello --help" to stdout
        assert!(result.is_some());
    }

    #[test]
    fn test_run_help_nonexistent_tool() {
        let pipeline = ParserPipeline::new(None);
        let discovery = SubcommandDiscovery::new(&pipeline, 2);
        let result = discovery.run_help("zzz_no_such_tool_xyz", &["zzz_no_such_tool_xyz".into()]);
        assert!(result.is_none());
    }
}
