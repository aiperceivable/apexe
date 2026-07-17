//! Shared `Registry`/`Executor` construction for the MCP and A2A server builders.
//!
//! Loads `.binding.yaml` files, registers each as a [`CliModule`], and applies
//! the same governance stack (logging middleware, ACL, approval handler) no
//! matter which protocol adapter ends up serving the resulting [`Executor`].

use std::path::Path;
use std::sync::Arc;

use apcore::middleware::circuit_breaker::CircuitBreakerMiddleware;
use apcore::middleware::logging::LoggingMiddleware;
use apcore::middleware::retry::{RetryConfig, RetryMiddleware};
use apcore::registry::registry::ModuleDescriptor;
use apcore::{ApprovalHandler, Config, ErrorCode, Executor, ModuleError, Registry};
use apcore_mcp::{ApprovalStore, ElicitationApprovalHandler, StorageBackedApprovalHandler};
use apcore_toolkit::ScannedModule;

use crate::module::CliModule;
use crate::output::load_modules_from_dir;

/// Options controlling how the shared `Executor` is assembled.
pub struct ExecutorOptions<'a> {
    pub modules_dir: Option<&'a Path>,
    pub timeout_ms: u64,
    pub acl_path: Option<&'a Path>,
    /// Path to the JSONL governance audit log. When set, module executions and
    /// ACL allow/deny decisions are appended here (F5 §4.3). `None` disables
    /// auditing.
    pub audit_path: Option<&'a Path>,
    pub enable_logging: bool,
    pub enable_approval: bool,
    /// Short-circuit calls to a (module, caller) pair after repeated
    /// failures, instead of letting every caller keep hammering a hanging
    /// or broken CLI tool. See [`CircuitBreakerMiddleware`].
    pub enable_circuit_breaker: bool,
    /// Retry a call after a transient failure. Only ever fires on errors
    /// explicitly marked `retryable` — `CliModule` only does that for
    /// timeouts on modules annotated `idempotent`, so this can't cause a
    /// destructive command to run twice. See [`RetryMiddleware`].
    pub enable_retry: bool,
    /// Optional pluggable approval persistence. When set (and
    /// `enable_approval` is true), approvals become non-blocking: a call to
    /// a `requires_approval` module returns an `ApprovalPending` error
    /// immediately via [`StorageBackedApprovalHandler`] instead of blocking
    /// on a synchronous MCP elicitation response, and a separate
    /// out-of-band mechanism you build (e.g. a Slack bot backed by the same
    /// store) resolves the decision later by calling
    /// `ApprovalStore::resolve`.
    ///
    /// Library-only — there is no `apexe serve --approval-store` CLI flag,
    /// since a generic CLI can't construct an arbitrary `Arc<dyn
    /// ApprovalStore>`. `apcore-mcp`'s `InMemoryApprovalStore` is explicitly
    /// documented as unsuitable for production (state is lost on restart
    /// and isn't shared across processes); embed apexe as a library and
    /// supply your own persistent store (Redis, a database, etc.) to use
    /// this for real. Leave `None` to keep the default synchronous
    /// [`ElicitationApprovalHandler`] behavior.
    pub approval_store: Option<Arc<dyn ApprovalStore>>,
}

/// Load scanned modules, register them into a fresh [`Registry`], and wrap the
/// result in a fully configured [`Executor`] wrapped in an `Arc` (logging
/// middleware, ACL, approval handler).
// ModuleError is the crate-wide domain error; boxing it would diverge from
// the rest of the apexe/apcore API surface.
#[allow(clippy::result_large_err)]
pub fn build_executor(opts: &ExecutorOptions<'_>) -> Result<Arc<Executor>, ModuleError> {
    let modules = load_scanned_modules(opts.modules_dir)?;

    // Governance audit sink (F5 §4.3): shared across all modules and the ACL.
    let audit = opts
        .audit_path
        .map(|p| Arc::new(crate::governance::AuditManager::new(p)));

    let registry = Registry::new();
    register_modules(&modules, &registry, opts.timeout_ms, audit.clone());
    tracing::info!(count = registry.count(), "Registered CLI modules");

    let mut executor = Executor::new(registry, Config::default());

    if opts.enable_logging {
        let logging = LoggingMiddleware::with_defaults();
        if let Err(e) = executor.use_middleware(Box::new(logging)) {
            tracing::warn!(error = %e, "Failed to add LoggingMiddleware");
        }
    }

    if opts.enable_circuit_breaker {
        let breaker = CircuitBreakerMiddleware::builder().build();
        if let Err(e) = executor.use_middleware(Box::new(breaker)) {
            tracing::warn!(error = %e, "Failed to add CircuitBreakerMiddleware");
        }
    }

    if opts.enable_retry {
        let retry = RetryMiddleware::new(RetryConfig::default());
        if let Err(e) = executor.use_middleware(Box::new(retry)) {
            tracing::warn!(error = %e, "Failed to add RetryMiddleware");
        }
    }

    // ACL is a security control: when the operator explicitly requests it via
    // `--acl`, a missing file or a load failure must FAIL CLOSED (refuse to
    // build the server) rather than degrade to an unguarded allow-all executor.
    // apcore skips ACL enforcement entirely when no ACL is set, so silently
    // continuing here would serve every tool to every caller.
    if let Some(acl_path) = opts.acl_path {
        if !acl_path.exists() {
            return Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                format!(
                    "ACL file not found: {} — refusing to start without the requested access control",
                    acl_path.display()
                ),
            ));
        }
        let acl_mgr = crate::governance::AclManager::from_config(acl_path)?;
        let mut acl = acl_mgr.into_inner();
        // Record every allow/deny decision to the audit trail (F5).
        if let Some(ref audit) = audit {
            let audit = audit.clone();
            acl.set_audit_logger(move |entry| audit.log_acl_decision(entry));
        }
        executor.set_acl(acl);
        tracing::info!(acl = %acl_path.display(), "ACL enforcement active");
    }

    if opts.enable_approval {
        let handler: Box<dyn ApprovalHandler> = match &opts.approval_store {
            Some(store) => Box::new(StorageBackedApprovalHandler::new(store.clone())),
            None => Box::new(ElicitationApprovalHandler::new(None)),
        };
        let store_backed = opts.approval_store.is_some();
        executor.set_approval_handler(handler);
        tracing::info!(
            store_backed,
            "Approval handler enabled for destructive commands"
        );
    }

    Ok(Arc::new(executor))
}

/// Load `ScannedModule`s from the configured modules directory.
#[allow(clippy::result_large_err)]
fn load_scanned_modules(modules_dir: Option<&Path>) -> Result<Vec<ScannedModule>, ModuleError> {
    match modules_dir {
        Some(dir) if dir.is_dir() => load_modules_from_dir(dir),
        Some(dir) => {
            tracing::warn!(
                dir = %dir.display(),
                "Modules directory not found, starting with zero tools"
            );
            Ok(vec![])
        }
        None => Ok(vec![]),
    }
}

/// Register `ScannedModule`s into a `Registry` as `CliModule`s.
///
/// Carries the full binding metadata (version, documentation, tags, and the
/// `DisplayResolver`-computed `metadata["display"]` overlay) into the
/// `ModuleDescriptor`, so apcore-mcp/apcore-a2a's alias-resolution and
/// rich-description logic (which reads `descriptor.display` /
/// `descriptor.metadata["display"]`) actually sees it.
fn register_modules(
    modules: &[ScannedModule],
    registry: &Registry,
    timeout_ms: u64,
    audit: Option<Arc<crate::governance::AuditManager>>,
) {
    for scanned in modules {
        match CliModule::from_scanned(scanned, timeout_ms) {
            Ok(cli_module) => {
                let cli_module = cli_module.with_audit(audit.clone());
                let module_id = scanned.module_id.clone();
                let annotations = scanned.annotations.clone().unwrap_or_default();
                let descriptor = ModuleDescriptor {
                    module_id: module_id.clone(),
                    name: None,
                    description: scanned.description.clone(),
                    documentation: scanned.documentation.clone(),
                    input_schema: scanned.input_schema.clone(),
                    output_schema: scanned.output_schema.clone(),
                    version: scanned.version.clone(),
                    tags: scanned.tags.clone(),
                    annotations: Some(annotations),
                    examples: vec![],
                    metadata: scanned.metadata.clone(),
                    display: scanned.display.clone(),
                    sunset_date: None,
                    dependencies: vec![],
                    enabled: true,
                };
                if let Err(e) = registry.register(&module_id, Box::new(cli_module), descriptor) {
                    tracing::warn!(module_id, error = %e, "Failed to register module");
                }
            }
            Err(e) => {
                tracing::warn!(
                    module_id = scanned.module_id,
                    error = %e,
                    "Failed to create CliModule"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::YamlOutput;
    use serde_json::json;
    use tempfile::TempDir;

    fn opts(modules_dir: Option<&Path>) -> ExecutorOptions<'_> {
        ExecutorOptions {
            modules_dir,
            timeout_ms: 30_000,
            acl_path: None,
            audit_path: None,
            enable_logging: true,
            enable_approval: false,
            enable_circuit_breaker: true,
            enable_retry: true,
            approval_store: None,
        }
    }

    #[test]
    fn test_build_executor_no_modules_dir() {
        let executor = build_executor(&opts(None)).unwrap();
        assert_eq!(executor.registry().count(), 0);
    }

    #[test]
    fn test_build_executor_wires_resilience_middleware() {
        let executor = build_executor(&opts(None)).unwrap();
        let names = executor.middlewares();
        assert!(names.contains(&"circuit_breaker".to_string()));
        assert!(names.contains(&"retry".to_string()));
    }

    #[test]
    fn test_build_executor_resilience_middleware_optional() {
        let mut opts = opts(None);
        opts.enable_circuit_breaker = false;
        opts.enable_retry = false;
        let executor = build_executor(&opts).unwrap();
        let names = executor.middlewares();
        assert!(!names.contains(&"circuit_breaker".to_string()));
        assert!(!names.contains(&"retry".to_string()));
    }

    #[test]
    fn test_build_executor_fails_closed_on_missing_acl_file() {
        // Operator requested --acl but the path does not exist: must refuse to
        // start rather than serve unguarded (fail closed, not fail open).
        let mut opts = opts(None);
        let missing = Path::new("/nonexistent/does-not-exist.acl.yaml");
        opts.acl_path = Some(missing);
        let result = build_executor(&opts);
        assert!(
            result.is_err(),
            "build_executor must fail when --acl points to a missing file"
        );
    }

    #[test]
    fn test_build_executor_fails_closed_on_malformed_acl_file() {
        // Operator requested --acl but the file is unparseable: must propagate
        // the error rather than build an executor with no ACL.
        let dir = TempDir::new().unwrap();
        let acl_path = dir.path().join("acl.yaml");
        std::fs::write(&acl_path, "this: is: not: valid: acl: [[[").unwrap();
        let mut opts = opts(None);
        opts.acl_path = Some(&acl_path);
        let result = build_executor(&opts);
        assert!(
            result.is_err(),
            "build_executor must fail when --acl file is malformed"
        );
    }

    #[test]
    fn test_build_executor_registers_modules_with_display_metadata() {
        let dir = TempDir::new().unwrap();
        let modules = vec![ScannedModule::new(
            "echo.hello".to_string(),
            "Echo hello".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo hello".to_string(),
        )];
        let output = YamlOutput::without_verification();
        output.write(&modules, dir.path(), false).unwrap();

        let executor = build_executor(&opts(Some(dir.path()))).unwrap();
        assert_eq!(executor.registry().count(), 1);

        let descriptor = executor
            .registry()
            .get_definition("echo.hello")
            .unwrap()
            .expect("module should be registered");
        assert_eq!(descriptor.module_id, "echo.hello");
        assert_eq!(descriptor.description, "Echo hello");
    }

    #[tokio::test]
    async fn test_build_executor_approval_store_makes_calls_non_blocking() {
        use apcore::module::ModuleAnnotations;
        use apcore_mcp::InMemoryApprovalStore;

        let dir = TempDir::new().unwrap();
        let mut module = ScannedModule::new(
            "cli.destroy".to_string(),
            "Destroy something".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo destroyed".to_string(),
        );
        module.annotations = Some(ModuleAnnotations {
            destructive: true,
            requires_approval: true,
            ..Default::default()
        });
        YamlOutput::without_verification()
            .write(&[module], dir.path(), false)
            .unwrap();

        let store: Arc<dyn ApprovalStore> = Arc::new(InMemoryApprovalStore::new());
        let mut opts = opts(Some(dir.path()));
        opts.enable_approval = true;
        opts.approval_store = Some(store);
        let executor = build_executor(&opts).unwrap();

        // A store-backed handler doesn't block waiting for a human; it
        // returns immediately with a pending decision instead of hanging.
        let result = executor.call("cli.destroy", json!({}), None, None).await;
        assert!(
            result.is_err(),
            "requires_approval module should not execute before approval"
        );
    }
}
