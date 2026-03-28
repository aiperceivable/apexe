use std::path::Path;

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::ScannedModule;

/// Load `ScannedModule`s from `.binding.yaml` files in a directory.
///
/// Each binding file is expected to contain a YAML document with a top-level
/// `bindings` array, matching the format produced by `YAMLWriter`.
// ModuleError is the crate-wide domain error; boxing it would diverge from the
// rest of the apexe/apcore API surface.
#[allow(clippy::result_large_err)]
pub fn load_modules_from_dir(dir: &Path) -> Result<Vec<ScannedModule>, ModuleError> {
    if !dir.exists() {
        return Err(ModuleError::new(
            ErrorCode::GeneralInternalError,
            format!("Modules directory not found: {}", dir.display()),
        ));
    }

    let mut modules = Vec::new();

    let entries = std::fs::read_dir(dir).map_err(|e| {
        ModuleError::new(
            ErrorCode::GeneralInternalError,
            format!("Failed to read directory: {e}"),
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to read entry: {e}"),
            )
        })?;
        let path = entry.path();

        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !filename.ends_with(".binding.yaml") {
            continue;
        }

        let contents = std::fs::read_to_string(&path).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to read {}: {e}", path.display()),
            )
        })?;

        // YAMLWriter produces files with a top-level `bindings` array.
        // Try that format first, then fall back to a single ScannedModule.
        let parsed: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to parse YAML in {}: {e}", path.display()),
            )
        })?;

        if let Some(bindings_val) = parsed.get("bindings") {
            let binding_modules: Vec<ScannedModule> = serde_yaml::from_value(bindings_val.clone())
                .map_err(|e| {
                    ModuleError::new(
                        ErrorCode::GeneralInternalError,
                        format!("Failed to deserialize bindings in {}: {e}", path.display()),
                    )
                })?;
            modules.extend(binding_modules);
        } else {
            // Fall back: try as a single ScannedModule
            let module: ScannedModule = serde_yaml::from_value(parsed).map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("Failed to parse {}: {e}", path.display()),
                )
            })?;
            modules.push(module);
        }
    }

    Ok(modules)
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
}
