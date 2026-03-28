use std::path::Path;

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::{ScannedModule, Verifier, WriteResult, YAMLVerifier, YAMLWriter};

/// Wraps apcore-toolkit's YAMLWriter to write ScannedModules as `.binding.yaml` files.
pub struct YamlOutput {
    writer: YAMLWriter,
    verify: bool,
}

impl YamlOutput {
    /// Create a new YamlOutput with verification enabled.
    pub fn new() -> Self {
        Self {
            writer: YAMLWriter,
            verify: true,
        }
    }

    /// Create a new YamlOutput with verification disabled.
    pub fn without_verification() -> Self {
        Self {
            writer: YAMLWriter,
            verify: false,
        }
    }

    /// Write modules to YAML binding files in `output_dir`.
    ///
    /// Each module is written to its own file: `{sanitized_module_id}.binding.yaml`.
    /// Returns `WriteResult`s for each module written.
    // ModuleError is the crate-wide domain error; boxing it would diverge from the
    // rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub fn write(
        &self,
        modules: &[ScannedModule],
        output_dir: &Path,
        dry_run: bool,
    ) -> Result<Vec<WriteResult>, ModuleError> {
        if modules.is_empty() {
            return Ok(vec![]);
        }

        if !dry_run {
            std::fs::create_dir_all(output_dir).map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("Failed to create output directory: {e}"),
                )
            })?;
        }

        let output_dir_str = output_dir.to_string_lossy();

        let yaml_verifier = YAMLVerifier;
        let verifiers: Vec<&dyn Verifier> = if self.verify {
            vec![&yaml_verifier]
        } else {
            vec![]
        };

        self.writer
            .write(
                modules,
                &output_dir_str,
                dry_run,
                self.verify,
                Some(&verifiers),
            )
            .map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("Failed to write binding files: {e}"),
                )
            })
    }
}

impl Default for YamlOutput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_test_module(id: &str) -> ScannedModule {
        ScannedModule::new(
            id.to_string(),
            format!("Test module {id}"),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string(), "test".to_string()],
            format!("exec:///usr/bin/test {id}"),
        )
    }

    #[test]
    fn test_yaml_output_writes_file() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::new();
        let modules = vec![make_test_module("write_test")];

        let results = output.write(&modules, dir.path(), false).unwrap();

        assert_eq!(results.len(), 1);
        let path = results[0].path.as_ref().expect("path should be set");
        assert!(
            std::path::Path::new(path).exists(),
            "binding file should exist"
        );
    }

    #[test]
    fn test_yaml_output_file_is_valid_yaml() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::new();
        let modules = vec![make_test_module("roundtrip")];

        let results = output.write(&modules, dir.path(), false).unwrap();
        let path = results[0].path.as_ref().unwrap();
        let contents = std::fs::read_to_string(path).unwrap();
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&contents).expect("should be valid YAML");

        let bindings = parsed["bindings"]
            .as_sequence()
            .expect("should have bindings array");
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0]["module_id"].as_str(),
            Some("roundtrip"),
            "module_id should match"
        );
    }

    #[test]
    fn test_yaml_output_dry_run_no_files() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::new();
        let modules = vec![make_test_module("dry_run")];

        let results = output.write(&modules, dir.path(), true).unwrap();

        assert_eq!(results.len(), 1);
        // No files should be created
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty(), "dry run should not create files");
    }

    #[test]
    fn test_yaml_output_empty_modules() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::new();

        let results = output.write(&[], dir.path(), false).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn test_yaml_output_creates_directory() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        assert!(!nested.exists());

        let output = YamlOutput::new();
        let modules = vec![make_test_module("nested")];

        let results = output.write(&modules, &nested, false).unwrap();

        assert_eq!(results.len(), 1);
        assert!(nested.exists(), "nested directory should be created");
    }

    #[test]
    fn test_yaml_output_without_verification() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::without_verification();
        let modules = vec![make_test_module("no_verify")];

        let results = output.write(&modules, dir.path(), false).unwrap();

        assert_eq!(results.len(), 1);
        let path = results[0].path.as_ref().expect("path should be set");
        assert!(std::path::Path::new(path).exists());
    }
}
