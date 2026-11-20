//! Delegated approval for per-match child commands.
//!
//! A parent call that embeds another program (`find -exec rm {} ;`) may create
//! a short-lived grant before it runs. The child command is rewritten by the
//! standard `PATH` order to the installed shim, and each per-file invocation
//! presents the grant: the child shim still runs its own ACL and path policy,
//! but the approval prompt is the parent's, not one per file.
//!
//! The grant is deliberately narrow: a set of child module ids, the parent's
//! starting paths, the parent trace, and an expiry. It never bypasses ACL or
//! the path guard. Only the scopes that received human approval can satisfy
//! a child's approval gate. The file is private (0700 directory, 0600
//! file) and removed when the parent call returns; a crash leaves it until the
//! expiry, which is the reason its scope is read again on every use.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::ApexeConfig;

/// Environment variable carrying the grant id from the parent to its children.
pub(crate) const DELEGATION_ENV: &str = "APEXE_DELEGATION";

/// How long a grant survives if its parent dies without cleaning up.
const TTL_SECONDS: u64 = 30 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Delegation {
    pub(crate) id: String,
    pub(crate) parent_trace: String,
    pub(crate) parent_module: String,
    pub(crate) children: Vec<ChildScope>,
    pub(crate) roots: Vec<String>,
    pub(crate) shim_dir: PathBuf,
    pub(crate) acl: Option<PathBuf>,
    pub(crate) policy_digest: Option<String>,
    pub(crate) caller: Option<String>,
    pub(crate) human_approved: bool,
    pub(crate) expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChildScope {
    pub(crate) module_id: String,
    binding_digest: String,
    input: Value,
    batched: bool,
    requires_approval: bool,
}

impl ChildScope {
    pub(crate) fn new(
        module: &ScannedModule,
        input: Value,
        batched: bool,
        requires_approval: bool,
    ) -> Self {
        Self {
            module_id: module.module_id.clone(),
            binding_digest: binding_digest(module),
            input: scope_input(&input),
            batched,
            requires_approval,
        }
    }

    pub(crate) fn matches(
        &self,
        grant: &Delegation,
        module: &ScannedModule,
        input: &Value,
    ) -> bool {
        self.module_id == module.module_id
            && self.binding_digest == binding_digest(module)
            && matches_input(grant, &self.input, input, self.batched)
    }

    pub(crate) fn approved(&self, grant: &Delegation) -> bool {
        grant.human_approved && self.requires_approval
    }

    pub(crate) fn requires_approval(&self) -> bool {
        self.requires_approval
    }
}

fn binding_digest(module: &ScannedModule) -> String {
    let contract = serde_json::json!([module.target, module.input_schema, module.annotations]);
    format!("{:x}", Sha256::digest(contract.to_string()))
}

/// Keep the shape and path placeholders without writing literal arguments.
fn scope_input(input: &Value) -> Value {
    match input {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), scope_input(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(scope_input).collect()),
        Value::String(value) if value == "{}" => Value::Null,
        _ => literal_digest(input),
    }
}

fn literal_digest(value: &Value) -> Value {
    Value::String(format!("{:x}", Sha256::digest(value.to_string())))
}

fn matches_input(grant: &Delegation, expected: &Value, actual: &Value, batched: bool) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            expected.len() == actual.len()
                && expected.iter().all(|(key, value)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| matches_input(grant, value, actual, batched))
                })
        }
        (Value::Array(expected), Value::Array(actual)) => {
            let variadic = batched && expected.last().is_some_and(Value::is_null);
            let fixed = expected.len() - usize::from(variadic);
            let length_matches = if variadic {
                actual.len() > fixed
            } else {
                actual.len() == fixed
            };
            length_matches
                && expected[..fixed]
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| matches_input(grant, expected, actual, false))
                && (!variadic
                    || actual[fixed..]
                        .iter()
                        .all(|actual| matches_input(grant, &expected[fixed], actual, false)))
        }
        (Value::Null, Value::String(actual)) => path_within(grant, actual),
        (Value::String(_), _) => *expected == literal_digest(actual),
        _ => false,
    }
}

/// Prepared scope, without a grant file or inherited authority. Activation
/// belongs to the parent module's execution path, after its preflight and gate.
#[derive(Debug)]
pub(crate) struct DelegationPlan {
    pub(crate) config_dir: PathBuf,
    pub(crate) children: Vec<ChildScope>,
    pub(crate) roots: Vec<String>,
    pub(crate) shim_dir: PathBuf,
    pub(crate) acl: Option<PathBuf>,
    pub(crate) policy_digest: Option<String>,
    pub(crate) caller: Option<String>,
    pub(crate) human_approved: Arc<AtomicBool>,
}

impl DelegationPlan {
    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    pub(crate) fn activate(
        &self,
        module: &str,
        trace: &str,
        audit: Option<&crate::governance::AuditManager>,
    ) -> Result<DelegationHandle, apcore::ModuleError> {
        if self.children.iter().any(ChildScope::requires_approval)
            && !self.human_approved.load(Ordering::Acquire)
        {
            return Err(apcore::ModuleError::new(
                apcore::ErrorCode::ApprovalDenied,
                "Delegation requires a completed human approval for the embedded commands",
            ));
        }
        let result = create(self, module, trace);
        if let Some(audit) = audit {
            let (decision, reason, grant) = match &result {
                Ok(handle) => ("delegate_grant", None, Some(handle.id())),
                Err(_) => ("refused", Some("delegation_unavailable"), None),
            };
            let mut event = serde_json::json!({
                "parent_module": module, "parent_trace": trace,
                "children": self.children.iter().map(|child| &child.module_id).collect::<Vec<_>>(),
                "decision": decision, "human_approved": self.human_approved.load(Ordering::Acquire),
            });
            if let Some(reason) = reason {
                event["reason"] = reason.into();
            }
            if let Some(grant) = grant {
                event["grant"] = grant.into();
            }
            audit.log_governance_event("child_exec", event);
        }
        result.map_err(|error| {
            apcore::ModuleError::new(
                apcore::ErrorCode::ModuleExecuteError,
                format!("Delegated approval could not be created: {error}"),
            )
        })
    }
}

/// Owns the grant file; dropping it removes the grant.
#[derive(Debug)]
pub(crate) struct DelegationHandle {
    path: PathBuf,
    id: String,
}

impl DelegationHandle {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for DelegationHandle {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn directory(config: &ApexeConfig) -> PathBuf {
    config.config_dir.join("delegation")
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Create a grant, make it visible to the parent's descendants, and return the
/// handle that removes it when the parent call finishes.
#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
fn create(
    plan: &DelegationPlan,
    parent_module: &str,
    parent_trace: &str,
) -> anyhow::Result<DelegationHandle> {
    let directory = plan.config_dir.join("delegation");
    std::fs::create_dir_all(&directory)?;
    restrict(&directory, 0o700)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let grant = Delegation {
        id: id.clone(),
        parent_trace: parent_trace.to_string(),
        parent_module: parent_module.to_string(),
        children: plan.children.clone(),
        roots: plan.roots.iter().map(|root| canonical(root)).collect(),
        shim_dir: plan.shim_dir.clone(),
        acl: plan.acl.clone(),
        policy_digest: plan.policy_digest.clone(),
        caller: plan.caller.clone(),
        human_approved: plan.human_approved.load(Ordering::Acquire),
        expires_at: now() + TTL_SECONDS,
    };
    let path = directory.join(format!("{id}.json"));
    let bytes = serde_json::to_vec(&grant)?;
    let mut file = open_private(&path)?;
    if let Err(error) = std::io::Write::write_all(&mut file, &bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(error.into());
    }
    drop(file);
    Ok(DelegationHandle { path, id })
}

/// Load the grant named by the environment, if it is still valid.
pub(crate) fn active_from_env(config: &ApexeConfig) -> anyhow::Result<Option<Delegation>> {
    let Some(id) = std::env::var_os(DELEGATION_ENV) else {
        return Ok(None);
    };
    load(config, id.to_str().unwrap_or_default())
        .map(Some)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "The inherited delegation is invalid or has expired; retry the parent command"
            )
        })
}

fn load(config: &ApexeConfig, id: &str) -> Option<Delegation> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let path = directory(config).join(format!("{id}.json"));
    let bytes = std::fs::read(path).ok()?;
    let grant: Delegation = serde_json::from_slice(&bytes).ok()?;
    (grant.id == id && grant.expires_at > now()).then_some(grant)
}

/// Whether `value` resolves to a path under one of the grant's parents.
///
/// The check uses the same resolver as the path guard, so a symlink is judged
/// by where it points; the grant is a scope, not an argument allow-list.
pub(crate) fn path_within(grant: &Delegation, value: &str) -> bool {
    let (resolved, _) = crate::governance::path_guard::active()
        .inspect(value, crate::governance::path_guard::AccessMode::Write);
    let resolved = Path::new(&resolved);
    grant
        .roots
        .iter()
        .any(|root| resolved.starts_with(Path::new(root)))
}

/// Create the grant file with owner-only permissions from the start.
///
/// `std::fs::write` would create it with the process umask and only afterwards
/// be chmod-ed; a failure between the two would leave a readable grant with no
/// handle to remove it.
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn canonical(value: &str) -> String {
    std::fs::canonicalize(value)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn test_create_writes_a_private_grant_and_drop_removes_it() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let config = crate::config::ApexeConfig {
            config_dir: temp.path().join("apexe"),
            audit_log: temp.path().join("audit.jsonl"),
            ..Default::default()
        };
        let plan = DelegationPlan {
            config_dir: config.config_dir.clone(),
            children: vec![],
            roots: vec![temp.path().to_string_lossy().into_owned()],
            shim_dir: temp.path().into(),
            acl: None,
            policy_digest: None,
            caller: Some("local".into()),
            human_approved: Default::default(),
        };
        let handle = create(&plan, "cli.find", "trace").unwrap();
        let id = handle.id().to_string();
        let path = directory(&config).join(format!("{id}.json"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_ne!(std::env::var(DELEGATION_ENV).as_deref(), Ok(handle.id()));
        let grant = load(&config, handle.id()).unwrap();
        assert_eq!(grant.parent_trace, "trace");
        assert_eq!(grant.caller.as_deref(), Some("local"));
        assert!(!grant.human_approved);
        drop(handle);
        assert!(!path.exists(), "the handle must remove the grant");
        assert!(load(&config, &id).is_none());
    }
}
