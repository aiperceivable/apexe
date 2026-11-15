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
//! the path guard; it only says "this child module was already put to a human
//! as part of the parent call". The file is private (0700 directory, 0600
//! file) and removed when the parent call returns; a crash leaves it until the
//! expiry, which is the reason its scope is read again on every use.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    pub(crate) child_modules: Vec<String>,
    pub(crate) roots: Vec<String>,
    pub(crate) expires_at: u64,
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
        if std::env::var(DELEGATION_ENV).as_deref() == Ok(self.id.as_str()) {
            std::env::remove_var(DELEGATION_ENV);
        }
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
pub(crate) fn create(
    config: &ApexeConfig,
    parent_module: &str,
    parent_trace: &str,
    child_modules: &[String],
    roots: &[String],
) -> anyhow::Result<DelegationHandle> {
    let directory = directory(config);
    std::fs::create_dir_all(&directory)?;
    restrict(&directory, 0o700)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let grant = Delegation {
        id: id.clone(),
        parent_trace: parent_trace.to_string(),
        parent_module: parent_module.to_string(),
        child_modules: child_modules.to_vec(),
        roots: roots.iter().map(|root| canonical(root)).collect(),
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
    std::env::set_var(DELEGATION_ENV, &id);
    Ok(DelegationHandle { path, id })
}

/// Load the grant named by the environment, if it is still valid.
pub(crate) fn active_from_env(config: &ApexeConfig) -> Option<Delegation> {
    let id = std::env::var(DELEGATION_ENV).ok()?;
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
        let handle = create(
            &config,
            "cli.find",
            "trace",
            &["cli.rm".to_string()],
            &[temp.path().to_string_lossy().into_owned()],
        )
        .unwrap();
        let id = handle.id().to_string();
        let path = directory(&config).join(format!("{id}.json"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let grant = active_from_env(&config).unwrap();
        assert_eq!(grant.parent_trace, "trace");
        assert_eq!(grant.child_modules, vec!["cli.rm"]);
        drop(handle);
        assert!(!path.exists(), "the handle must remove the grant");
    }
}
