use std::path::{Path, PathBuf};

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::{format_module, ModuleStyle, ScannedModule};

/// Writes Claude Skill files (`SKILL.md`) for scanned modules.
///
/// Each module becomes `.claude/skills/<module_id>/SKILL.md`, generated via
/// apcore-toolkit's `ModuleStyle::Skill` formatter — directly usable by
/// Claude Code as a skill without any additional conversion step, closing
/// the gap between "apexe scans a CLI tool" and "an agent can use it as a
/// skill".
pub struct SkillOutput;

impl SkillOutput {
    /// Create a new `SkillOutput`.
    pub fn new() -> Self {
        Self
    }

    /// Write one `SKILL.md` per module under `<output_dir>/.claude/skills/`.
    ///
    /// Returns the paths written.
    // ModuleError is the crate-wide domain error; boxing it would diverge
    // from the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub fn write(
        &self,
        modules: &[ScannedModule],
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>, ModuleError> {
        let mut written = Vec::with_capacity(modules.len());
        // module_id is not itself a safe path component (see
        // sanitize_skill_dir_name), so distinct module_ids can sanitize to
        // the same directory name (e.g. "a/b" and "a_b" both -> "a_b").
        // Without this check the second module's SKILL.md would silently
        // overwrite the first's instead of surfacing the naming clash.
        let mut seen_dirs: std::collections::HashMap<String, &str> =
            std::collections::HashMap::new();
        for module in modules {
            let dir_name = sanitize_skill_dir_name(&module.module_id);
            if let Some(&existing_module_id) = seen_dirs.get(&dir_name) {
                if existing_module_id != module.module_id {
                    return Err(ModuleError::new(
                        ErrorCode::GeneralInvalidInput,
                        format!(
                            "Skill directory name collision: modules '{existing_module_id}' \
                             and '{}' both sanitize to '.claude/skills/{dir_name}'",
                            module.module_id
                        ),
                    ));
                }
            } else {
                seen_dirs.insert(dir_name.clone(), &module.module_id);
            }

            let content = format_module(module, ModuleStyle::Skill, true)
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| {
                    ModuleError::new(
                        ErrorCode::GeneralInternalError,
                        format!(
                            "Skill formatter returned non-text output for '{}'",
                            module.module_id
                        ),
                    )
                })?;

            let skill_dir = output_dir.join(".claude").join("skills").join(dir_name);
            std::fs::create_dir_all(&skill_dir).map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!(
                        "Failed to create skill directory {}: {e}",
                        skill_dir.display()
                    ),
                )
            })?;

            let path = skill_dir.join("SKILL.md");
            std::fs::write(&path, content).map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("Failed to write {}: {e}", path.display()),
                )
            })?;
            written.push(path);
        }
        Ok(written)
    }
}

impl Default for SkillOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// Sanitize a `module_id` into a single safe path component.
///
/// `module_id` is normally apcore-Registry-validated (dot-separated, no
/// slashes) by the time it reaches here, but `SkillOutput` is also usable
/// directly as a library against hand-loaded `ScannedModule`s, so this
/// defends against a malformed/adversarial `module_id` escaping
/// `.claude/skills/` via a path separator or a bare `..` component.
fn sanitize_skill_dir_name(module_id: &str) -> String {
    let sanitized: String = module_id
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "_".to_string()
    } else {
        sanitized
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
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            json!({"type": "object"}),
            vec!["cli".to_string(), "test".to_string()],
            format!("exec:///usr/bin/test {id}"),
        )
    }

    #[test]
    fn test_skill_output_writes_file() {
        let dir = TempDir::new().unwrap();
        let modules = vec![make_test_module("cli.git.commit")];

        let paths = SkillOutput::new().write(&modules, dir.path()).unwrap();

        assert_eq!(paths.len(), 1);
        let expected = dir.path().join(".claude/skills/cli.git.commit/SKILL.md");
        assert_eq!(paths[0], expected);
        assert!(expected.exists());
    }

    #[test]
    fn test_skill_output_content_has_frontmatter_and_description() {
        let dir = TempDir::new().unwrap();
        let modules = vec![make_test_module("cli.git.commit")];

        let paths = SkillOutput::new().write(&modules, dir.path()).unwrap();
        let content = std::fs::read_to_string(&paths[0]).unwrap();

        assert!(content.starts_with("---\n"));
        assert!(content.contains("description:"));
        assert!(content.contains("Test module cli.git.commit"));
    }

    #[test]
    fn test_skill_output_multiple_modules() {
        let dir = TempDir::new().unwrap();
        let modules = vec![
            make_test_module("cli.git.commit"),
            make_test_module("cli.git.push"),
        ];

        let paths = SkillOutput::new().write(&modules, dir.path()).unwrap();

        assert_eq!(paths.len(), 2);
        for path in &paths {
            assert!(path.exists());
        }
    }

    #[test]
    fn test_skill_output_empty_modules() {
        let dir = TempDir::new().unwrap();
        let paths = SkillOutput::new().write(&[], dir.path()).unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn test_skill_output_write_rejects_colliding_module_ids() {
        // Regression for the WARNING finding: "a/b" and "a_b" both sanitize
        // to the directory name "a_b". write() must reject the batch
        // instead of letting the second module's SKILL.md silently
        // overwrite the first's.
        let dir = TempDir::new().unwrap();
        let modules = vec![make_test_module("a/b"), make_test_module("a_b")];

        let err = SkillOutput::new()
            .write(&modules, dir.path())
            .expect_err("colliding module_ids must be rejected");
        assert!(err.message.contains("collision"));
        assert!(err.message.contains("a/b"));
        assert!(err.message.contains("a_b"));
    }

    #[test]
    fn test_sanitize_skill_dir_name_blocks_traversal() {
        assert_eq!(
            sanitize_skill_dir_name("../../etc/passwd"),
            ".._.._etc_passwd"
        );
        assert_eq!(sanitize_skill_dir_name(".."), "_");
        assert_eq!(sanitize_skill_dir_name(""), "_");
        assert_eq!(sanitize_skill_dir_name("cli.git.commit"), "cli.git.commit");
    }
}
