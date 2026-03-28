use std::path::Path;

use apcore::{ACLRule, ErrorCode, ModuleError, ACL};
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Serializable representation of an ACL config file (rules + default_effect).
#[derive(Debug, Serialize, Deserialize)]
struct AclConfig {
    rules: Vec<ACLRule>,
    default_effect: String,
}

/// Manages access control for CLI modules using apcore's ACL system.
pub struct AclManager {
    acl: ACL,
    /// Cached default_effect so we can serialize without needing an accessor on ACL.
    default_effect: String,
}

impl AclManager {
    /// Load ACL from a YAML config file.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    pub fn from_config(config_path: &Path) -> Result<Self, ModuleError> {
        let acl = ACL::load(&config_path.to_string_lossy()).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to load ACL: {e}"),
            )
        })?;
        // Re-read the file to extract default_effect since ACL has no public accessor.
        let default_effect = Self::read_default_effect(config_path);
        Ok(Self {
            acl,
            default_effect,
        })
    }

    /// Generate default ACL from scanned modules based on annotations.
    pub fn generate_default(modules: &[ScannedModule]) -> Self {
        let mut rules = Vec::new();

        // Readonly modules -> allow
        let readonly_ids: Vec<String> = modules
            .iter()
            .filter(|m| m.annotations.as_ref().is_some_and(|a| a.readonly))
            .map(|m| m.module_id.clone())
            .collect();
        if !readonly_ids.is_empty() {
            rules.push(ACLRule {
                callers: vec!["*".to_string()],
                targets: readonly_ids,
                effect: "allow".to_string(),
                description: Some("Auto-allow readonly CLI commands".to_string()),
                conditions: None,
            });
        }

        // Destructive modules -> deny with require_approval
        let destructive_ids: Vec<String> = modules
            .iter()
            .filter(|m| m.annotations.as_ref().is_some_and(|a| a.destructive))
            .map(|m| m.module_id.clone())
            .collect();
        if !destructive_ids.is_empty() {
            rules.push(ACLRule {
                callers: vec!["*".to_string()],
                targets: destructive_ids,
                effect: "deny".to_string(),
                description: Some("Block destructive CLI commands by default".to_string()),
                conditions: Some(json!({"require_approval": true})),
            });
        }

        let acl = ACL::new(rules, "deny");
        Self {
            acl,
            default_effect: "deny".to_string(),
        }
    }

    /// Write ACL to a YAML file.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    pub fn write_config(&self, path: &Path) -> Result<(), ModuleError> {
        let config = AclConfig {
            rules: self.acl.rules().to_vec(),
            default_effect: self.default_effect.clone(),
        };
        let yaml = serde_yaml::to_string(&config).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to serialize ACL: {e}"),
            )
        })?;
        std::fs::write(path, yaml).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to write ACL file: {e}"),
            )
        })?;
        Ok(())
    }

    /// Consume the manager and return the inner ACL.
    pub fn into_inner(self) -> ACL {
        self.acl
    }

    /// Read `default_effect` from a YAML file (best-effort, falls back to "deny").
    fn read_default_effect(path: &Path) -> String {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_yaml::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("default_effect")?.as_str().map(String::from))
            .unwrap_or_else(|| "deny".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_module_with_annotations(id: &str, readonly: bool, destructive: bool) -> ScannedModule {
        let mut module = ScannedModule::new(
            id.to_string(),
            format!("Test {id}"),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            format!("exec:///usr/bin/test {id}"),
        );
        module.annotations = Some(apcore::module::ModuleAnnotations {
            readonly,
            destructive,
            requires_approval: destructive,
            ..Default::default()
        });
        module
    }

    #[test]
    fn test_acl_generate_default_readonly() {
        let modules = vec![
            make_module_with_annotations("cli.git.status", true, false),
            make_module_with_annotations("cli.git.log", true, false),
        ];
        let mgr = AclManager::generate_default(&modules);
        let rules = mgr.acl.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].effect, "allow");
        assert_eq!(rules[0].targets.len(), 2);
    }

    #[test]
    fn test_acl_generate_default_destructive() {
        let modules = vec![make_module_with_annotations("cli.git.clean", false, true)];
        let mgr = AclManager::generate_default(&modules);
        let rules = mgr.acl.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].effect, "deny");
        assert!(rules[0].conditions.is_some());
    }

    #[test]
    fn test_acl_generate_default_mixed() {
        let modules = vec![
            make_module_with_annotations("cli.git.status", true, false),
            make_module_with_annotations("cli.git.clean", false, true),
        ];
        let mgr = AclManager::generate_default(&modules);
        let rules = mgr.acl.rules();
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn test_acl_generate_default_empty() {
        let mgr = AclManager::generate_default(&[]);
        let rules = mgr.acl.rules();
        assert!(rules.is_empty());
        assert_eq!(mgr.default_effect, "deny");
    }

    #[test]
    fn test_acl_write_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("acl_manager.yaml");

        let modules = vec![
            make_module_with_annotations("cli.git.status", true, false),
            make_module_with_annotations("cli.git.clean", false, true),
        ];
        let mgr = AclManager::generate_default(&modules);
        mgr.write_config(&path).unwrap();

        let loaded = AclManager::from_config(&path).unwrap();
        assert_eq!(loaded.acl.rules().len(), 2);
        assert_eq!(loaded.default_effect, "deny");
    }
}
