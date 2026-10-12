use crate::config::ApexeConfig;
use crate::module::{ExecutorOptions, ModuleFilter};
use apcore::{Context, Identity};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(super) fn acl_path(
    explicit: Option<&Path>,
    config: &ApexeConfig,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        return Ok(Some(path.into()));
    }
    let path = config.config_dir.join("acl.yaml");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(acl = %path.display(), "Running with no ACL loaded; access control is inactive");
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn context(caller: &str, asserted: bool) -> Context<Value> {
    let mut context = Context::new(Identity::new(
        caller.into(),
        if asserted { "asserted" } else { "local" }.into(),
        vec![],
        Default::default(),
    ));
    context.call_chain = vec![caller.into()];
    context.caller_id = Some(caller.into());
    context
}

pub(super) fn options<'a>(
    config: &'a ApexeConfig,
    bindings: &'a Path,
    pattern: Option<&'a str>,
    acl: Option<&'a Path>,
    filter: ModuleFilter,
) -> ExecutorOptions<'a> {
    ExecutorOptions {
        bindings_dir: Some(bindings),
        bindings_pattern: pattern,
        timeout_ms: config.default_timeout.saturating_mul(1000),
        acl_path: acl,
        filter,
        audit_path: Some(&config.audit_log),
        enable_logging: true,
        log_arguments: false,
        enable_approval: true,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
    }
}
