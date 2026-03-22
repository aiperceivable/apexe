use std::process::Command;

use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::errors::ApexeError;

/// Resolved tool binary information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTool {
    pub name: String,
    pub binary_path: String,
    pub version: Option<String>,
}

/// Resolves CLI tool names to binary paths and version info.
pub struct ToolResolver;

impl ToolResolver {
    /// Resolve a tool name to its binary path and version.
    ///
    /// Returns `Err(ToolNotFound)` if tool is not on PATH.
    pub fn resolve(&self, tool_name: &str) -> Result<ResolvedTool, ApexeError> {
        let binary_path = which::which(tool_name)
            .map_err(|_| ApexeError::ToolNotFound {
                tool_name: tool_name.to_string(),
            })?
            .to_string_lossy()
            .to_string();

        let version = self.get_version(&binary_path, tool_name);

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            binary_path,
            version,
        })
    }

    /// Extract version from --version output.
    fn get_version(&self, binary_path: &str, _tool_name: &str) -> Option<String> {
        let output = Command::new(binary_path)
            .arg("--version")
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_line = stdout.lines().next()?;

        extract_version_from_line(first_line)
    }
}

/// Extract a semver-like version string from a line of text.
pub fn extract_version_from_line(line: &str) -> Option<String> {
    let re = Regex::new(r"(\d+\.\d+[\.\d]*)").ok()?;
    re.captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Detect the help format from help text heuristics.
pub fn detect_help_format(help_text: &str) -> crate::models::HelpFormat {
    use crate::models::HelpFormat;

    if help_text.contains("Available Commands:") {
        return HelpFormat::Cobra;
    }
    if help_text.contains("SUBCOMMANDS:") {
        return HelpFormat::Clap;
    }
    if help_text.contains("[OPTIONS]") && help_text.contains("Commands:") {
        return HelpFormat::Click;
    }
    if help_text.contains("Usage:") || help_text.contains("usage:") {
        return HelpFormat::Gnu;
    }
    HelpFormat::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    // T9: ResolvedTool serde
    #[test]
    fn test_resolved_tool_serde_with_version() {
        let tool = ResolvedTool {
            name: "git".into(),
            binary_path: "/usr/bin/git".into(),
            version: Some("2.43.0".into()),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let back: ResolvedTool = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "git");
        assert_eq!(back.version, Some("2.43.0".into()));
    }

    #[test]
    fn test_resolved_tool_serde_no_version() {
        let tool = ResolvedTool {
            name: "mytool".into(),
            binary_path: "/usr/bin/mytool".into(),
            version: None,
        };
        let json = serde_json::to_string(&tool).unwrap();
        let back: ResolvedTool = serde_json::from_str(&json).unwrap();
        assert!(back.version.is_none());
    }

    // T10: ToolResolver resolve
    #[test]
    fn test_resolve_known_tool() {
        let resolver = ToolResolver;
        let result = resolver.resolve("sh");
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.name, "sh");
        assert!(!resolved.binary_path.is_empty());
    }

    #[test]
    fn test_resolve_unknown_tool() {
        let resolver = ToolResolver;
        let result = resolver.resolve("zzz_no_such_tool_xyz");
        assert!(result.is_err());
        match result.unwrap_err() {
            ApexeError::ToolNotFound { tool_name } => {
                assert_eq!(tool_name, "zzz_no_such_tool_xyz");
            }
            other => panic!("Expected ToolNotFound, got: {other:?}"),
        }
    }

    // T11: get_version extraction
    #[test]
    fn test_extract_version_git_style() {
        assert_eq!(
            extract_version_from_line("git version 2.43.0"),
            Some("2.43.0".into())
        );
    }

    #[test]
    fn test_extract_version_curl_style() {
        assert_eq!(
            extract_version_from_line("curl 8.1.2 (x86_64-apple-darwin)"),
            Some("8.1.2".into())
        );
    }

    #[test]
    fn test_extract_version_no_version() {
        assert_eq!(extract_version_from_line("no version here"), None);
    }

    #[test]
    fn test_extract_version_empty() {
        assert_eq!(extract_version_from_line(""), None);
    }

    // T12: detect_help_format
    #[test]
    fn test_detect_cobra() {
        let text = "Some tool\n\nAvailable Commands:\n  sub1  desc\n";
        assert_eq!(detect_help_format(text), crate::models::HelpFormat::Cobra);
    }

    #[test]
    fn test_detect_clap() {
        let text = "tool 1.0\n\nSUBCOMMANDS:\n  sub1  desc\n";
        assert_eq!(detect_help_format(text), crate::models::HelpFormat::Clap);
    }

    #[test]
    fn test_detect_click() {
        let text = "Usage: tool [OPTIONS] COMMAND\n\nCommands:\n  run  Run it\n";
        assert_eq!(detect_help_format(text), crate::models::HelpFormat::Click);
    }

    #[test]
    fn test_detect_gnu() {
        let text = "Usage: tool [OPTION]... FILE\n\nOptions:\n  -v  verbose\n";
        assert_eq!(detect_help_format(text), crate::models::HelpFormat::Gnu);
    }

    #[test]
    fn test_detect_unknown() {
        let text = "This is just random text with no patterns.";
        assert_eq!(
            detect_help_format(text),
            crate::models::HelpFormat::Unknown
        );
    }
}
