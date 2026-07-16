use std::path::Path;

use apcore::{ACLRule, ErrorCode, ModuleError, ACL};
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};

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

        // Destructive modules -> unconditional deny. `require_approval` is not
        // a registered apcore ACL condition key (only `identity_types`,
        // `roles`, `max_call_depth`, `$or`, `$not` are); a rule with an
        // unregistered condition key can never match (apcore treats an
        // unknown condition as unsatisfied), so it must not be used here —
        // it would silently fall through to whatever rule/default_effect
        // follows instead of denying. Actual approval-gating for destructive
        // commands happens via the Executor's ApprovalHandler
        // (see `crate::module::build_executor`), not the ACL layer.
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
                conditions: None,
            });
        }

        let acl = ACL::new(rules, "deny", None);
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
        // Must be an unconditional deny: apcore only registers `identity_types`,
        // `roles`, `max_call_depth`, `$or`, `$not` as condition keys, so any
        // other key (e.g. `require_approval`) can never match and would let
        // the rule silently fall through instead of denying.
        assert!(rules[0].conditions.is_none());
    }

    #[test]
    fn test_acl_generate_default_destructive_rule_denies_on_its_own() {
        // Regression for the CRITICAL finding: the destructive-deny rule
        // previously carried `conditions: Some({"require_approval": true})`,
        // a condition key apcore never registers. An unregistered condition
        // key can never be satisfied, so the rule could never match — it
        // relied entirely on `default_effect: "deny"` for protection. Prove
        // the rule denies on its own merits by appending a more permissive
        // rule after it (lower priority) and confirming the deny still wins,
        // which only happens if the deny rule actually matches.
        let modules = vec![make_module_with_annotations("cli.git.clean", false, true)];
        let mgr = AclManager::generate_default(&modules);
        let mut rules = mgr.acl.rules().to_vec();
        rules.push(ACLRule {
            callers: vec!["*".to_string()],
            targets: vec!["cli.git.clean".to_string()],
            effect: "allow".to_string(),
            description: None,
            conditions: None,
        });
        let acl = ACL::new(rules, "allow", None);
        assert!(
            !acl.check(None, "cli.git.clean", None),
            "destructive command must be denied by its own ACL rule, not merely \
             by a coincidental default_effect"
        );
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
