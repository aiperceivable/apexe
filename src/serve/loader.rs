use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::warn;

/// A single loaded binding ready for MCP serving.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedBinding {
    pub module_id: String,
    pub description: String,
    pub input_schema: JsonValue,
    pub output_schema: JsonValue,
    pub annotations: HashMap<String, JsonValue>,
    /// The CLI command parts to execute (e.g., ["git", "status"]).
    pub tool_command: Vec<String>,
    /// The binary to invoke.
    pub tool_binary: String,
    /// Timeout in seconds.
    pub timeout: u64,
    /// Optional JSON output flag.
    pub json_flag: Option<String>,
}

/// Raw binding entry as stored in .binding.yaml files.
#[derive(Debug, Clone, Deserialize)]
struct RawBinding {
    module_id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    input_schema: JsonValue,
    #[serde(default)]
    output_schema: JsonValue,
    #[serde(default)]
    annotations: HashMap<String, JsonValue>,
    #[serde(default)]
    metadata: HashMap<String, JsonValue>,
}

/// Raw binding file structure.
#[derive(Debug, Clone, Deserialize)]
struct RawBindingFile {
    bindings: Vec<RawBinding>,
}

/// Load all .binding.yaml files from a directory.
///
/// Malformed files are logged as warnings and skipped.
pub fn load_bindings(modules_dir: &Path) -> Result<Vec<LoadedBinding>> {
    if !modules_dir.is_dir() {
        bail!("Modules directory not found: {}", modules_dir.display());
    }

    let mut loaded = Vec::new();

    let entries = std::fs::read_dir(modules_dir)
        .with_context(|| format!("Failed to read directory: {}", modules_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".binding.yaml"))
        {
            continue;
        }

        match load_binding_file(&path) {
            Ok(bindings) => loaded.extend(bindings),
            Err(e) => {
                warn!(path = %path.display(), "Skipping malformed binding file: {e}");
            }
        }
    }

    Ok(loaded)
}

fn load_binding_file(path: &Path) -> Result<Vec<LoadedBinding>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    // Strip comment lines before parsing
    let yaml_content: String = content
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<&str>>()
        .join("\n");

    let raw: RawBindingFile = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;

    let mut result = Vec::new();
    for binding in raw.bindings {
        let tool_binary = binding
            .metadata
            .get("apexe_binary")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let tool_command: Vec<String> = binding
            .metadata
            .get("apexe_command")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let timeout = binding
            .metadata
            .get("apexe_timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        let json_flag = binding
            .metadata
            .get("apexe_json_flag")
            .and_then(|v| v.as_str())
            .map(String::from);

        result.push(LoadedBinding {
            module_id: binding.module_id,
            description: binding.description,
            input_schema: binding.input_schema,
            output_schema: binding.output_schema,
            annotations: binding.annotations,
            tool_command,
            tool_binary,
            timeout,
            json_flag,
        });
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_valid_binding(dir: &Path, name: &str) {
        let content = format!(
            r#"bindings:
  - module_id: "cli.{name}.status"
    description: "Show status"
    target: "apexe::executor::execute_cli"
    input_schema:
      type: object
      properties: {{}}
    output_schema:
      type: object
    tags:
      - cli
    version: "1.0.0"
    annotations: {{}}
    metadata:
      apexe_binary: "{name}"
      apexe_command:
        - "{name}"
        - "status"
      apexe_timeout: 30
"#
        );
        std::fs::write(dir.join(format!("{name}.binding.yaml")), content).unwrap();
    }

    #[test]
    fn test_discover_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let result = load_bindings(tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_discover_valid_bindings() {
        let tmp = TempDir::new().unwrap();
        write_valid_binding(tmp.path(), "git");
        write_valid_binding(tmp.path(), "docker");

        let result = load_bindings(tmp.path()).unwrap();
        assert_eq!(result.len(), 2);

        let ids: Vec<&str> = result.iter().map(|b| b.module_id.as_str()).collect();
        assert!(ids.contains(&"cli.git.status"));
        assert!(ids.contains(&"cli.docker.status"));
    }

    #[test]
    fn test_discover_skips_malformed() {
        let tmp = TempDir::new().unwrap();
        write_valid_binding(tmp.path(), "git");
        std::fs::write(
            tmp.path().join("bad.binding.yaml"),
            "this is not valid yaml: [[[",
        )
        .unwrap();

        let result = load_bindings(tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].module_id, "cli.git.status");
    }

    #[test]
    fn test_discover_nonexistent_dir() {
        let result = load_bindings(Path::new("/nonexistent/path/xyz"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "Expected 'not found' in error, got: {err}"
        );
    }

    #[test]
    fn test_ignores_non_binding_files() {
        let tmp = TempDir::new().unwrap();
        write_valid_binding(tmp.path(), "git");
        std::fs::write(tmp.path().join("readme.txt"), "not a binding").unwrap();
        std::fs::write(tmp.path().join("data.yaml"), "key: value").unwrap();

        let result = load_bindings(tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_metadata_extraction() {
        let tmp = TempDir::new().unwrap();
        write_valid_binding(tmp.path(), "git");

        let result = load_bindings(tmp.path()).unwrap();
        assert_eq!(result[0].tool_binary, "git");
        assert_eq!(result[0].tool_command, vec!["git", "status"]);
        assert_eq!(result[0].timeout, 30);
    }
}
