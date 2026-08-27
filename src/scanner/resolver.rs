use std::time::Duration;

use crate::errors::ApexeError;
use regex::Regex;
use serde::{Deserialize, Serialize};

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
    pub fn resolve(&self, tool_name: &str, timeout: Duration) -> Result<ResolvedTool, ApexeError> {
        let binary_path = which::which(tool_name)
            .map_err(|_| ApexeError::ToolNotFound {
                tool_name: tool_name.to_string(),
            })?
            .to_string_lossy()
            .to_string();

        let version = self.get_version(&binary_path, tool_name, timeout);

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            binary_path,
            version,
        })
    }

    /// Extract version from --version output (bounded by `timeout`).
    fn get_version(
        &self,
        binary_path: &str,
        _tool_name: &str,
        timeout: Duration,
    ) -> Option<String> {
        let output = super::exec::run_with_timeout(binary_path, &["--version"], timeout).ok()?;

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

/// Extract the binary path from a scanned module's `exec://{binary_path}
/// {command_parts...}` target. Mirrors the parsing `CliModule::from_scanned`
/// does, without constructing a whole `CliModule` just to read one field.
pub fn extract_binary_path(target: &str) -> Option<&str> {
    target.strip_prefix("exec://")?.split_whitespace().next()
}

/// Whether `binary_path` is reachable on this machine right now.
///
/// A path containing a separator is checked directly on disk -- this is what
/// a scan-time `which` resolution produces, and is the common case for a
/// binding file. A bare name is re-resolved against the current `PATH`.
///
/// Deliberately just a filesystem/PATH lookup, not a `--version` probe like
/// [`ToolResolver::resolve`]: this can run once per listed or registered
/// module, so it has to stay cheap and side-effect-free rather than spawn a
/// subprocess per tool.
pub fn binary_is_reachable(binary_path: &str) -> bool {
    if binary_path.contains(std::path::MAIN_SEPARATOR) {
        std::path::Path::new(binary_path).is_file()
    } else {
        which::which(binary_path).is_ok()
    }
}

/// Whether the binary named by a scanned module's `target` is reachable on
/// this machine. A target that doesn't parse is treated as unavailable --
/// [`CliModule::from_scanned`](crate::module::CliModule::from_scanned) would
/// refuse it too.
pub fn target_is_available(target: &str) -> bool {
    extract_binary_path(target).is_some_and(binary_is_reachable)
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
        let result = resolver.resolve("sh", std::time::Duration::from_secs(5));
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.name, "sh");
        assert!(!resolved.binary_path.is_empty());
    }

    #[test]
    fn test_resolve_unknown_tool() {
        let resolver = ToolResolver;
        let result = resolver.resolve("zzz_no_such_tool_xyz", std::time::Duration::from_secs(5));
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

    #[test]
    fn test_extract_binary_path_strips_scheme_and_command_parts() {
        assert_eq!(
            extract_binary_path("exec:///usr/bin/git cat-file -p"),
            Some("/usr/bin/git")
        );
        assert_eq!(extract_binary_path("exec:///bin/ls"), Some("/bin/ls"));
    }

    #[test]
    fn test_extract_binary_path_rejects_non_exec_target() {
        assert_eq!(extract_binary_path("http:///bin/ls"), None);
        assert_eq!(extract_binary_path("exec://"), None);
    }

    #[test]
    fn test_binary_is_reachable_for_existing_absolute_path() {
        // /bin/sh exists on every Unix apexe targets; this is the scan-time
        // snapshot format binding files actually store.
        assert!(binary_is_reachable("/bin/sh"));
    }

    #[test]
    fn test_binary_is_reachable_false_for_missing_absolute_path() {
        assert!(!binary_is_reachable("/nonexistent/zzz_no_such_binary_xyz"));
    }

    #[test]
    fn test_binary_is_reachable_resolves_bare_name_on_path() {
        assert!(binary_is_reachable("sh"));
        assert!(!binary_is_reachable("zzz_no_such_tool_xyz"));
    }

    #[test]
    fn test_target_is_available_end_to_end() {
        assert!(target_is_available("exec:///bin/sh"));
        assert!(!target_is_available(
            "exec:///nonexistent/zzz_no_such_binary_xyz"
        ));
        assert!(!target_is_available("not-a-valid-target"));
    }
}
