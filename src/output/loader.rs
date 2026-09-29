use std::path::Path;

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::{BindingLoadError, BindingLoader, ScannedModule};

/// Load `ScannedModule`s from the binding files in a directory.
///
/// Delegates to apcore-toolkit's [`BindingLoader`] (non-recursive, strict
/// mode — `module_id`, `description`, `input_schema`, `output_schema`,
/// `tags`, and `target` are all required, matching what `YAMLWriter` always
/// emits), which additionally enforces a 16 MiB per-file cap and a 10,000
/// file per-directory cap that the previous hand-rolled parser did not.
///
/// `pattern` is apcore's `bindings.pattern` when a configuration declares one,
/// and `None` otherwise — which selects the toolkit's own `*.binding.yaml`,
/// the same canonical default. It is threaded through rather than read here
/// because the pure-data loader holds no `Config`; see
/// [`crate::config::ApexeConfig::bindings_pattern`] for where the value is
/// resolved and why only a declared one counts.
///
/// Until apcore-toolkit 0.12 there was no parameter to pass it through, so a
/// consumer needing the loader's *return value* rather than apcore's
/// registration side effect silently ignored the key
/// ([apcore-toolkit#18](https://github.com/aiperceivable/apcore-toolkit/issues/18)).
/// apexe is that consumer: it converts each `ScannedModule` into its own
/// `CliModule`, so apcore's config-aware loader would bypass every control
/// `CliModule` exists to apply.
// ModuleError is the crate-wide domain error; boxing it would diverge from the
// rest of the apexe/apcore API surface.
#[allow(clippy::result_large_err)]
pub fn load_modules_from_dir(
    dir: &Path,
    pattern: Option<&str>,
) -> Result<Vec<ScannedModule>, ModuleError> {
    BindingLoader::new()
        .load_with_pattern(
            dir, /* strict */ true, /* recursive */ false, pattern,
        )
        .map_err(|e| match e {
            BindingLoadError::PathNotFound { path } => ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Bindings directory not found: {path}"),
            ),
            other => ModuleError::new(ErrorCode::GeneralInternalError, other.to_string()),
        })
}

/// The pre-0.8.0 default bindings directory, when it still holds bindings.
///
/// Returns `None` unless `dir` is a default-layout `bindings` directory whose
/// `modules` sibling exists and holds at least one `*.binding.yaml`. The
/// `file_name` guard is deliberate: an operator who declared `bindings.dir`
/// pointing somewhere of their own has no `modules` sibling to have upgraded
/// from, and guessing one would name a path that was never apexe's.
fn legacy_bindings_dir(dir: &Path) -> Option<std::path::PathBuf> {
    if dir.file_name()? != std::ffi::OsStr::new("bindings") {
        return None;
    }
    let legacy = dir.parent()?.join("modules");
    let holds_a_binding = std::fs::read_dir(&legacy).ok()?.flatten().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .ends_with(".binding.yaml")
    });
    holds_a_binding.then_some(legacy)
}

/// Warn when a bindings directory yields nothing to serve.
///
/// A server with zero modules is a server with no callable tools, and until
/// now that arrived silently: [`crate::module::registry`] warned only when the
/// directory was *missing*, and `apexe list` treated a missing one as "no
/// modules yet" and said nothing at all. An empty directory and an unscanned
/// host read identically, which is the shape the 0.8.0 upgrade turns into a
/// certainty rather than a possibility — apcore's canonical `bindings.dir`
/// moved the default from `~/.apexe/modules` to `~/.apexe/bindings`, so an
/// install that scanned under 0.7.0 finds the new directory empty and its
/// bindings still sitting in the old one.
///
/// Called by the two places a user reaches, not by
/// [`load_modules_from_dir`] itself, which stays a pure data path.
pub fn warn_if_no_bindings(dir: &Path, loaded: usize) {
    if loaded > 0 {
        return;
    }
    match legacy_bindings_dir(dir) {
        Some(legacy) => tracing::warn!(
            dir = %dir.display(),
            legacy_dir = %legacy.display(),
            "No bindings here, so there are NO callable tools -- but the pre-0.8.0 \
             default directory still holds some. apexe now follows apcore's canonical \
             `bindings.dir`, which moved the default from `modules` to `bindings`. \
             Re-run `apexe scan` to regenerate them here (recommended -- a binding \
             written by an older apexe can also be missing contract keywords), or move \
             the files across."
        ),
        None => tracing::warn!(
            dir = %dir.display(),
            "No bindings found, so there are NO callable tools. Run `apexe scan <tool>` first."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::YamlOutput;

    /// The upgrade case: `bindings` is empty and `modules` still holds files.
    #[test]
    fn test_the_pre_0_8_0_directory_is_named_when_it_still_holds_bindings() {
        let root = TempDir::new().unwrap();
        let bindings = root.path().join("bindings");
        let legacy = root.path().join("modules");
        std::fs::create_dir_all(&bindings).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("cli.git.binding.yaml"), "bindings: []\n").unwrap();

        assert_eq!(legacy_bindings_dir(&bindings), Some(legacy));
    }

    /// A fresh install has no `modules` sibling, and an operator who already
    /// migrated has an empty one. Neither should be pointed at.
    #[test]
    fn test_the_pre_0_8_0_directory_is_not_named_when_it_holds_nothing() {
        let root = TempDir::new().unwrap();
        let bindings = root.path().join("bindings");
        std::fs::create_dir_all(&bindings).unwrap();
        assert_eq!(legacy_bindings_dir(&bindings), None, "no sibling at all");

        let legacy = root.path().join("modules");
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(legacy_bindings_dir(&bindings), None, "empty sibling");

        std::fs::write(legacy.join("notes.txt"), "not a binding").unwrap();
        assert_eq!(
            legacy_bindings_dir(&bindings),
            None,
            "a sibling holding no *.binding.yaml is not a bindings directory"
        );
    }

    /// A declared `bindings.dir` of the operator's own never had a `modules`
    /// sibling to upgrade from, so guessing one would name a path that was
    /// never apexe's -- even when such a directory happens to exist.
    #[test]
    fn test_a_declared_bindings_dir_is_not_given_a_legacy_sibling() {
        let root = TempDir::new().unwrap();
        let declared = root.path().join("team-policy");
        let decoy = root.path().join("modules");
        std::fs::create_dir_all(&declared).unwrap();
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("cli.git.binding.yaml"), "bindings: []\n").unwrap();

        assert_eq!(legacy_bindings_dir(&declared), None);
    }

    /// The version has to survive the write, or a later apexe reads nothing.
    #[test]
    fn test_the_generating_version_round_trips_through_a_binding_file() {
        let dir = TempDir::new().unwrap();
        let mut module = make_test_module("cli.demo");
        module.metadata.insert(
            "generated_by".to_string(),
            json!(format!("apexe {}", env!("CARGO_PKG_VERSION"))),
        );

        YamlOutput::new()
            .write(std::slice::from_ref(&module), dir.path(), false)
            .unwrap();
        let loaded = load_modules_from_dir(dir.path(), None).unwrap();

        assert_eq!(
            loaded[0].metadata.get("generated_by"),
            Some(&json!(format!("apexe {}", env!("CARGO_PKG_VERSION")))),
            "a binding must say which apexe wrote it after a round trip, or a \
             later release cannot tell a stale file from a current one"
        );
    }
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

        let loaded = load_modules_from_dir(dir.path(), None).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].module_id, "loader_read");
    }

    /// The approval gate's stand-down decision reads `annotations.extra`, and
    /// the annotations it sees came off disk rather than out of the scan that
    /// wrote them. A serializer that dropped `extra` would leave every
    /// flag-derived module prompting again — safe, but the fix would be inert
    /// and nothing else in the suite would notice.
    #[test]
    fn test_loader_preserves_the_approval_basis_through_a_binding_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut module = make_test_module("cli.git.push");
        let mut annotations = apcore::module::ModuleAnnotations {
            requires_approval: true,
            ..Default::default()
        };
        annotations.extra.insert(
            crate::adapter::annotations::APPROVAL_BASIS_KEY.to_string(),
            json!(crate::adapter::annotations::APPROVAL_BASIS_FLAGS),
        );
        annotations.extra.insert(
            crate::adapter::annotations::ESCALATING_PARAMS_KEY.to_string(),
            json!(["force", "prune"]),
        );
        module.annotations = Some(annotations);

        YamlOutput::without_verification()
            .write(&[module], dir.path(), false)
            .unwrap();
        let loaded = load_modules_from_dir(dir.path(), None).unwrap();

        let extra = &loaded[0].annotations.as_ref().unwrap().extra;
        assert_eq!(
            extra.get(crate::adapter::annotations::APPROVAL_BASIS_KEY),
            Some(&json!(crate::adapter::annotations::APPROVAL_BASIS_FLAGS))
        );
        assert_eq!(
            extra.get(crate::adapter::annotations::ESCALATING_PARAMS_KEY),
            Some(&json!(["force", "prune"]))
        );
    }

    #[test]
    fn test_loader_empty_directory() {
        let dir = TempDir::new().unwrap();

        let loaded = load_modules_from_dir(dir.path(), None).unwrap();

        assert!(loaded.is_empty());
    }

    #[test]
    fn test_loader_nonexistent_directory() {
        let result = load_modules_from_dir(Path::new("/nonexistent/path/abc123"), None);

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

        let loaded = load_modules_from_dir(dir.path(), None).unwrap();

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

        let loaded = load_modules_from_dir(dir.path(), None).unwrap();

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

        let result = load_modules_from_dir(dir.path(), None);
        assert!(result.is_err(), "incomplete binding should be rejected");
    }

    /// A declared pattern selects by it, not by the default suffix.
    ///
    /// The end of what apcore-toolkit#18 opened: apexe resolves
    /// `bindings.pattern` from apcore's `Config` and hands it to the loader.
    /// Asserted on both sides of the same directory so a pattern that quietly
    /// did nothing would fail rather than look like a pass.
    #[test]
    fn test_a_declared_pattern_decides_which_files_are_loaded() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::without_verification();
        output
            .write(&[make_test_module("default_suffix")], dir.path(), false)
            .unwrap();

        // Same content under a name the default pattern does not select.
        let written = dir.path().join("default_suffix.binding.yaml");
        std::fs::copy(&written, dir.path().join("renamed.apexe.yaml")).unwrap();

        let default_pattern = load_modules_from_dir(dir.path(), None).unwrap();
        assert_eq!(
            default_pattern.len(),
            1,
            "the default pattern selects only the .binding.yaml file"
        );

        let declared = load_modules_from_dir(dir.path(), Some("*.apexe.yaml")).unwrap();
        assert_eq!(
            declared.len(),
            1,
            "a declared pattern selects the other file instead"
        );
        assert_eq!(declared[0].module_id, "default_suffix");

        let both = load_modules_from_dir(dir.path(), Some("*.yaml")).unwrap();
        assert_eq!(both.len(), 2, "a wider pattern selects both");
    }
}
