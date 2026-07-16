use std::path::Path;

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::{BindingLoadError, BindingLoader, ScannedModule};

/// Load `ScannedModule`s from `.binding.yaml` files in a directory.
///
/// Delegates to apcore-toolkit's [`BindingLoader`] (non-recursive, strict
/// mode — `module_id`, `description`, `input_schema`, `output_schema`,
/// `tags`, and `target` are all required, matching what `YAMLWriter` always
/// emits), which additionally enforces a 16 MiB per-file cap and a 10,000
/// file per-directory cap that the previous hand-rolled parser did not.
// ModuleError is the crate-wide domain error; boxing it would diverge from the
// rest of the apexe/apcore API surface.
#[allow(clippy::result_large_err)]
pub fn load_modules_from_dir(dir: &Path) -> Result<Vec<ScannedModule>, ModuleError> {
    BindingLoader::new()
        .load(dir, /* strict */ true, /* recursive */ false)
        .map_err(|e| match e {
            BindingLoadError::PathNotFound { path } => ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Modules directory not found: {path}"),
            ),
            other => ModuleError::new(ErrorCode::GeneralInternalError, other.to_string()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::YamlOutput;
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
    fn test_loader_reads_binding_files() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::without_verification();
        let modules = vec![make_test_module("loader_read")];
        output.write(&modules, dir.path(), false).unwrap();

        let loaded = load_modules_from_dir(dir.path()).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].module_id, "loader_read");
    }

    #[test]
    fn test_loader_empty_directory() {
        let dir = TempDir::new().unwrap();

        let loaded = load_modules_from_dir(dir.path()).unwrap();

        assert!(loaded.is_empty());
    }

    #[test]
    fn test_loader_nonexistent_directory() {
        let result = load_modules_from_dir(Path::new("/nonexistent/path/abc123"));

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("not found"),
            "error should mention directory not found: {}",
            err.message
        );
    }

    #[test]
    fn test_loader_ignores_non_yaml() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "not a binding").unwrap();
        std::fs::write(dir.path().join("data.json"), "{}").unwrap();

        let loaded = load_modules_from_dir(dir.path()).unwrap();

        assert!(loaded.is_empty());
    }

    #[test]
    fn test_loader_roundtrip() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::without_verification();
        let original = vec![
            make_test_module("roundtrip_a"),
            make_test_module("roundtrip_b"),
        ];
        output.write(&original, dir.path(), false).unwrap();

        let loaded = load_modules_from_dir(dir.path()).unwrap();

        assert_eq!(loaded.len(), 2);
        let mut ids: Vec<&str> = loaded.iter().map(|m| m.module_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["roundtrip_a", "roundtrip_b"]);

        // Verify content matches
        for loaded_module in &loaded {
            let orig = original
                .iter()
                .find(|m| m.module_id == loaded_module.module_id)
                .expect("module should exist in original");
            assert_eq!(loaded_module.description, orig.description);
            assert_eq!(loaded_module.target, orig.target);
            assert_eq!(loaded_module.tags, orig.tags);
        }
    }

    #[test]
    fn test_loader_rejects_incomplete_binding_in_strict_mode() {
        let dir = TempDir::new().unwrap();
        // Missing input_schema/output_schema/target - not a valid binding.
        std::fs::write(
            dir.path().join("broken.binding.yaml"),
            "bindings:\n  - module_id: cli.broken\n    description: incomplete\n",
        )
        .unwrap();

        let result = load_modules_from_dir(dir.path());
        assert!(result.is_err(), "incomplete binding should be rejected");
    }
}
