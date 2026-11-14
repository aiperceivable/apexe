use std::path::{Path, PathBuf};

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::{BindingLoadError, BindingLoader, ScannedModule};

/// Load `ScannedModule`s from the binding files in a directory.
///
/// Delegates to apcore-toolkit's [`BindingLoader`] (non-recursive, strict
/// mode — `module_id`, `input_schema`, `output_schema`, and `target` are
/// required, matching what `YAMLWriter` always emits). Unknown document or
/// entry keys, unpaired inline schemas, and non-list `tags` are rejected;
/// application-specific entry fields belong in `metadata`. The loader also
/// enforces a 16 MiB per-file cap and a 10,000 file per-directory cap.
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
    let mut modules = BindingLoader::new()
        .load_with_pattern(
            dir, /* strict */ true, /* recursive */ false, pattern,
        )
        .map_err(|e| match e {
            BindingLoadError::PathNotFound { path } => ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Bindings directory not found: {path}"),
            ),
            other => ModuleError::new(ErrorCode::GeneralInternalError, other.to_string()),
        })?;
    crate::verification::attach_evidence(dir, &mut modules).map_err(|error| {
        ModuleError::new(
            ErrorCode::GeneralInternalError,
            format!("Invalid behavioral evidence: {error}"),
        )
    })?;
    Ok(modules)
}

/// Load only the binding files belonging to `module_ids`.
///
/// The shim hot path already knows, from its manifest, exactly which routes a
/// shim can serve, so parsing the whole installation's grammar on every command
/// is wasted work: a governed `ls` paid for every installed tool's bindings
/// (7 MB across 174 files, ~105 ms measured) before it ran `ls` at all. This
/// narrows that to the files the shim could possibly select, leaving the cost
/// of a call independent of how many tools are installed.
///
/// Selection is by filename prefix (`<module_id>.`), which is the
/// `{sanitized_module_id}.binding.yaml` convention [`crate::output::YamlOutput`]
/// writes and which also holds for a file whose suffix pattern an operator
/// changed. Narrowing may only ever be a performance decision, never a coverage
/// decision, so every input that could make the subset disagree with a full
/// load falls back to [`load_modules_from_dir`]: an empty `module_ids` slice, no
/// file matching any of them, or an unreadable directory.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn load_modules_for_ids(
    dir: &Path,
    module_ids: &[String],
) -> Result<Vec<ScannedModule>, ModuleError> {
    let Some(selected) = select_binding_files(dir, module_ids) else {
        return load_modules_from_dir(dir, None);
    };
    let mut modules: Vec<ScannedModule> = Vec::new();
    for path in selected {
        modules.extend(load_binding_file(&path)?);
    }
    crate::verification::attach_evidence(dir, &mut modules).map_err(|error| {
        ModuleError::new(
            ErrorCode::GeneralInternalError,
            format!("Invalid behavioral evidence: {error}"),
        )
    })?;
    Ok(modules)
}

/// Load the routes one native invocation could select under `tool_id`.
///
/// A native call knows two things before it reads any binding: the tool
/// (`argv[0]`), and — unless the first argument is a flag — the subcommand
/// token. Route resolution can only ever pick a route whose `command_path`
/// starts with those tokens, plus the tool's own root route, so parsing the
/// tool's whole namespace (150 bindings for `git`, 6.5 MB) is wasted when one
/// subcommand's grammar is the whole candidate set.
///
/// The root route is included unconditionally: `command_path == ["git"]`
/// matches any argv, and excluding it would refuse calls that used to work.
/// When the token matches nothing — a subcommand spelled with a character the
/// id sanitizer rewrote, or a flag the scan never declared — the whole tool
/// namespace is loaded instead, because narrowing may never remove a candidate
/// the full load would have offered.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn load_modules_for_tool_route(
    dir: &Path,
    tool_id: &str,
    token: Option<&str>,
) -> Result<Vec<ScannedModule>, ModuleError> {
    let root = format!("{tool_id}.binding.yaml");
    let subcommand = token
        .map(|token| format!("{tool_id}.{token}."))
        .filter(|prefix| !prefix.is_empty());
    let Some(entries) = select_files_matching(dir, &root, subcommand.as_deref()) else {
        return load_modules_for_ids(dir, &[tool_id.to_string()]);
    };
    if let Some(prefix) = &subcommand {
        let matched_subcommand = entries.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        });
        if !matched_subcommand {
            return load_modules_for_ids(dir, &[tool_id.to_string()]);
        }
    }
    let mut modules: Vec<ScannedModule> = Vec::new();
    for path in entries {
        modules.extend(load_binding_file(&path)?);
    }
    crate::verification::attach_evidence(dir, &mut modules).map_err(|error| {
        ModuleError::new(
            ErrorCode::GeneralInternalError,
            format!("Invalid behavioral evidence: {error}"),
        )
    })?;
    Ok(modules)
}

/// Files named `root` or starting with `subcommand`, or `None` when the
/// directory cannot be read or holds neither.
fn select_files_matching(dir: &Path, root: &str, subcommand: Option<&str>) -> Option<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut selected: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let matches = name == root || subcommand.is_some_and(|prefix| name.starts_with(prefix));
        if matches {
            selected.push(path);
        }
    }
    (!selected.is_empty()).then_some(selected)
}

/// The binding files that belong to `module_ids`, or `None` to load them all.
///
/// `None` is the safe answer: it is returned whenever the subset cannot be
/// proved to be a strict narrowing, and the caller then takes the full path.
fn select_binding_files(dir: &Path, module_ids: &[String]) -> Option<Vec<PathBuf>> {
    if module_ids.is_empty() {
        return None;
    }
    let prefixes: Vec<String> = module_ids
        .iter()
        .filter(|id| !id.is_empty())
        .map(|id| format!("{id}."))
        .collect();
    if prefixes.is_empty() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut selected: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if prefixes.iter().any(|prefix| name.starts_with(prefix)) {
            selected.push(path);
        }
    }
    if selected.is_empty() {
        None
    } else {
        Some(selected)
    }
}

/// Parse one binding file, reporting a defect against the file that holds it.
#[allow(clippy::result_large_err)] // Same crate-boundary boundary as `load_modules_for_ids`
fn load_binding_file(path: &Path) -> Result<Vec<ScannedModule>, ModuleError> {
    BindingLoader::new()
        .load_with_pattern(
            path, /* strict */ true, /* recursive */ false, None,
        )
        .map_err(|error| match error {
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
pub(crate) fn warn_if_no_bindings(dir: &Path, loaded: usize) {
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
            "No bindings found, so there are NO callable tools. Run `apexe init` first, or `apexe scan <tool>` for a custom tool."
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

    /// A native call narrows to its subcommand plus the tool's root route:
    /// `git status` must not parse the other 149 git bindings, and the root
    /// route must survive because `command_path == ["git"]` matches any argv.
    #[test]
    fn test_tool_route_loads_the_subcommand_and_the_root_route() {
        let dir = TempDir::new().unwrap();
        YamlOutput::without_verification()
            .write(
                &[
                    make_test_module("cli.git.status"),
                    make_test_module("cli.git.log"),
                    make_test_module("cli.git"),
                ],
                dir.path(),
                false,
            )
            .unwrap();

        let loaded = load_modules_for_tool_route(dir.path(), "cli.git", Some("status")).unwrap();

        let mut ids: Vec<&str> = loaded.iter().map(|m| m.module_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["cli.git", "cli.git.status"]);
    }

    /// A token the bindings do not spell (a sanitized subcommand, an
    /// undeclared flag) must fall back to the whole tool namespace. Narrowing
    /// that dropped it would refuse a call the full load resolves.
    #[test]
    fn test_tool_route_falls_back_when_the_token_matches_nothing() {
        let dir = TempDir::new().unwrap();
        YamlOutput::without_verification()
            .write(
                &[
                    make_test_module("cli.git.status"),
                    make_test_module("cli.git.log"),
                ],
                dir.path(),
                false,
            )
            .unwrap();

        let loaded = load_modules_for_tool_route(dir.path(), "cli.git", Some("rebase")).unwrap();

        assert_eq!(
            loaded.len(),
            2,
            "an unmatched token loads the tool's whole namespace, not nothing"
        );
    }

    /// The shim hot path loads only the bindings of the routes its command can
    /// reach. If the subset is not actually a subset of what a full load would
    /// return, a route that used to run would suddenly be "no longer
    /// registered", so the narrowing has to be provably narrow.
    #[test]
    fn test_loading_for_ids_returns_only_the_requested_bindings() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::without_verification();
        let modules = vec![
            make_test_module("cli.git.status"),
            make_test_module("cli.git.log"),
            make_test_module("cli.ls"),
        ];
        output.write(&modules, dir.path(), false).unwrap();

        let loaded = load_modules_for_ids(dir.path(), &["cli.ls".to_string()]).unwrap();

        assert_eq!(loaded.len(), 1, "only the requested route may be parsed");
        assert_eq!(loaded[0].module_id, "cli.ls");
    }

    /// A sibling whose id merely *starts* with a requested one must not be
    /// swept in: `cli.git.log` and a hypothetical `cli.git.logx` are different
    /// routes, and the prefix test has to end at the id boundary.
    #[test]
    fn test_loading_for_ids_stops_at_the_id_boundary() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("bindings");
        std::fs::create_dir_all(&dir).unwrap();
        let output = YamlOutput::without_verification();
        output
            .write(
                &[
                    make_test_module("cli.git.log"),
                    make_test_module("cli.git.logx"),
                ],
                &dir,
                false,
            )
            .unwrap();

        let loaded = load_modules_for_ids(&dir, &["cli.git.log".to_string()]).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].module_id, "cli.git.log");
    }

    /// A manifest can name a route whose binding file was removed (a rescan
    /// pruned it, or the tool was uninstalled). Narrowing must then degrade to
    /// the full load rather than report the route as missing on its own, or the
    /// refusal would blame the wrong layer.
    #[test]
    fn test_loading_for_ids_falls_back_when_no_file_matches() {
        let dir = TempDir::new().unwrap();
        let output = YamlOutput::without_verification();
        output
            .write(&[make_test_module("cli.ls")], dir.path(), false)
            .unwrap();

        let loaded = load_modules_for_ids(dir.path(), &["cli.vanished".to_string()]).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].module_id, "cli.ls",
            "an unmatched id must fall back to the full load, not to nothing"
        );
    }

    /// Operators may change `bindings.pattern`, which changes the file *suffix*
    /// but not the id prefix the writer embeds. Prefix selection has to keep
    /// working across that, or every custom-pattern installation loses the
    /// narrowing without saying so.
    #[test]
    fn test_loading_for_ids_matches_a_custom_file_suffix() {
        let dir = TempDir::new().unwrap();
        YamlOutput::without_verification()
            .write(&[make_test_module("cli.demo")], dir.path(), false)
            .unwrap();
        std::fs::rename(
            dir.path().join("cli.demo.binding.yaml"),
            dir.path().join("cli.demo.team.yaml"),
        )
        .unwrap();

        let loaded = load_modules_for_ids(dir.path(), &["cli.demo".to_string()]).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].module_id, "cli.demo");
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

    #[test]
    fn test_loader_rejects_unsupported_binding_fields() {
        for (at_document_root, key) in [
            (true, "custom"),
            (false, "custom"),
            (false, "auto_schema"),
            (false, "schema_ref"),
        ] {
            let dir = TempDir::new().unwrap();
            YamlOutput::without_verification()
                .write(&[make_test_module("cli.demo")], dir.path(), false)
                .unwrap();
            let path = dir.path().join("cli.demo.binding.yaml");
            let mut document: serde_json::Value =
                serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let object = if at_document_root {
                &mut document
            } else {
                &mut document["bindings"][0]
            };
            object[key] = json!(true);
            std::fs::write(&path, serde_yaml::to_string(&document).unwrap()).unwrap();

            let error = load_modules_from_dir(dir.path(), None).unwrap_err();
            assert!(error.message.contains("unknown"), "{error}");
            assert!(error.message.contains(key), "{error}");
            assert!(error.message.contains(path.to_str().unwrap()), "{error}");
        }
    }

    #[test]
    fn test_loader_rejects_unpaired_schemas_and_non_list_tags() {
        for field in ["input_schema", "output_schema", "tags"] {
            let dir = TempDir::new().unwrap();
            YamlOutput::without_verification()
                .write(&[make_test_module("cli.demo")], dir.path(), false)
                .unwrap();
            let path = dir.path().join("cli.demo.binding.yaml");
            let mut document: serde_json::Value =
                serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            if field == "tags" {
                document["bindings"][0][field] = json!("cli");
            } else {
                document["bindings"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            }
            std::fs::write(&path, serde_yaml::to_string(&document).unwrap()).unwrap();

            let error = load_modules_from_dir(dir.path(), None).unwrap_err();
            let diagnostic = if field == "tags" {
                "tags"
            } else {
                "input_schema and output_schema"
            };
            assert!(error.message.contains(diagnostic), "{error}");
        }
    }

    #[test]
    fn test_loader_preserves_application_metadata() {
        let dir = TempDir::new().unwrap();
        let mut module = make_test_module("cli.demo");
        module
            .metadata
            .insert("custom".to_string(), json!({"owner": "ops"}));
        YamlOutput::new()
            .write(&[module], dir.path(), false)
            .unwrap();

        let loaded = load_modules_from_dir(dir.path(), None).unwrap();
        assert_eq!(loaded[0].metadata["custom"], json!({"owner": "ops"}));
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
