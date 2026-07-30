//! Shared `Registry`/`Executor` construction for the MCP and A2A server builders.
//!
//! Loads `.binding.yaml` files, registers each as a [`CliModule`], and applies
//! the same governance stack (logging middleware, ACL, approval handler) no
//! matter which protocol adapter ends up serving the resulting [`Executor`].

use std::path::Path;
use std::sync::Arc;

use apcore::middleware::logging::LoggingMiddleware;
use apcore::middleware::retry::{RetryConfig, RetryMiddleware};
use apcore::registry::registry::ModuleDescriptor;
use apcore::{Config, ErrorCode, Executor, ModuleError, Registry};
use apcore_mcp::{ApprovalStore, StorageBackedApprovalHandler};
use apcore_toolkit::ScannedModule;

use crate::module::{CliModule, DenyApprovalHandler, HealthOnlyCircuitBreaker};
use crate::output::load_modules_from_dir;

/// Which subset of the scanned modules a server exposes.
///
/// Applied at *registration* time, not at listing time. apcore-mcp builds the
/// advertised tool list through `registry.list(tags, prefix, ..)` but routes an
/// incoming `tools/call` straight to `registry.get`, so a filter handed to the
/// protocol adapter restricts what is advertised while leaving every excluded
/// module fully callable by name — and leaves `resources/read` serving its
/// documentation too. Filtering before `Registry::register` closes all three at
/// once: an excluded module simply does not exist on this server, so every
/// lookup path returns `ModuleNotFound`. This is the invariant `module_id`
/// registration already holds — the surface a server advertises and the surface
/// it serves are the same set.
#[derive(Debug, Default, Clone)]
pub struct ModuleFilter {
    /// Keep only modules whose `module_id` starts with this prefix.
    pub prefix: Option<String>,
    /// Keep only modules carrying *every* listed tag (AND logic), matching
    /// `Registry::list`'s own semantics.
    pub tags: Option<Vec<String>>,
}

impl ModuleFilter {
    /// Whether `module` survives the filter. A filter with neither field set
    /// admits everything.
    pub fn admits(&self, module: &ScannedModule) -> bool {
        if let Some(ref prefix) = self.prefix {
            if !module.module_id.starts_with(prefix.as_str()) {
                return false;
            }
        }
        if let Some(ref required) = self.tags {
            if !required.iter().all(|tag| module.tags.contains(tag)) {
                return false;
            }
        }
        true
    }

    /// Whether this filter would exclude anything at all.
    fn is_active(&self) -> bool {
        self.prefix.is_some() || self.tags.is_some()
    }
}

/// Options controlling how the shared `Executor` is assembled.
pub struct ExecutorOptions<'a> {
    pub modules_dir: Option<&'a Path>,
    pub timeout_ms: u64,
    pub acl_path: Option<&'a Path>,
    /// Restrict which scanned modules are registered at all. See
    /// [`ModuleFilter`].
    pub filter: ModuleFilter,
    /// Path to the JSONL governance audit log. When set, module executions and
    /// ACL allow/deny decisions are appended here (F5 §4.3). `None` disables
    /// auditing.
    pub audit_path: Option<&'a Path>,
    pub enable_logging: bool,
    /// Include each call's `inputs` and `output` in the structured log.
    ///
    /// Separate from `enable_logging` because the two protect different things.
    /// A wrapped CLI tool's credential options (`curl --user`, `--header`,
    /// `--oauth2-bearer`, an ssh key path) arrive as ordinary tool arguments,
    /// and the scanner marks the ones it recognizes `x-sensitive` so apcore
    /// redacts them — but a heuristic cannot be exhaustive over every tool's
    /// option set, and neither a request body (`curl --data`) nor a key in a
    /// URL's query string announces itself in any schema.
    ///
    /// Turning this off drops the payload without losing the operational
    /// record: `CliModule` emits its own per-call `tracing` events carrying
    /// `module_id`, `trace_id`, caller, `exit_code` and `duration_ms`, and the
    /// audit trail is unaffected (it never held raw argument values). What is
    /// lost is apcore's `START`/`Completed` pair, which exists to carry
    /// `inputs=`/`output=`. So an operator no longer has to choose between
    /// logging credentials and logging nothing.
    pub log_arguments: bool,
    pub enable_approval: bool,
    /// Short-circuit calls to a (module, caller) pair after repeated
    /// failures, instead of letting every caller keep hammering a hanging
    /// or broken CLI tool. Only failures that say the wrapped binary is
    /// unhealthy count; a caller sending invalid arguments does not open a
    /// circuit. See [`HealthOnlyCircuitBreaker`].
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

    let admitted: Vec<ScannedModule> = modules
        .into_iter()
        .filter(|module| opts.filter.admits(module))
        .collect();
    if opts.filter.is_active() {
        tracing::info!(
            prefix = ?opts.filter.prefix,
            tags = ?opts.filter.tags,
            admitted = admitted.len(),
            "Module filter active; excluded modules are neither listed nor callable"
        );
    }

    let registry = Registry::new();
    register_modules(&admitted, &registry, opts.timeout_ms, audit.clone());
    tracing::info!(count = registry.count(), "Registered CLI modules");
    // Snapshot the ids that survived registration (a module whose `CliModule`
    // failed to build is logged and skipped), taken before the registry moves
    // into the `Executor`. `include_hidden` is true because a module annotated
    // `discoverable: false` is still callable by id and so still needs an ACL.
    let registered_ids: Vec<String> = registry.module_ids_full(true);

    let mut executor = Executor::new(registry, Config::default());

    if opts.enable_logging {
        // (log_inputs, log_outputs, log_errors) — errors always logged; the
        // payload is what `log_arguments` governs. See `log_arguments`.
        let logging = LoggingMiddleware::new(opts.log_arguments, opts.log_arguments, true);
        if let Err(e) = executor.use_middleware(Box::new(logging)) {
            tracing::warn!(error = %e, "Failed to add LoggingMiddleware");
        }
    }

    if opts.enable_circuit_breaker {
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        if let Err(e) = executor.use_middleware(Box::new(breaker)) {
            tracing::warn!(error = %e, "Failed to add HealthOnlyCircuitBreaker");
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
        // A rule whose target names nothing registered, or whose target list is
        // empty, enforces nothing — and until now said nothing about it either.
        // The registry is populated by this point (and already narrowed by
        // `ModuleFilter`), so this is the first and only place both the policy
        // and the surface it guards exist together. See `validate_acl_rules`
        // for the refuse-vs-warn split.
        let report = crate::governance::validate_acl_rules(acl.rules(), &registered_ids);
        report.emit_warnings();
        if let Some(error) = report.fatal_error(acl_path) {
            return Err(error);
        }
        // Record every allow/deny decision to the audit trail (F5).
        if let Some(ref audit) = audit {
            let audit = audit.clone();
            acl.set_audit_logger(move |entry| audit.log_acl_decision(entry));
        }
        executor.set_acl(acl);
        tracing::info!(acl = %acl_path.display(), "ACL enforcement active");
    }

    if opts.enable_approval {
        match &opts.approval_store {
            Some(store) => {
                executor.set_approval_handler(Box::new(StorageBackedApprovalHandler::new(
                    store.clone(),
                )));
                tracing::info!(
                    store_backed = true,
                    "Approval handler enabled for destructive commands"
                );
            }
            None => {
                // See `DenyApprovalHandler`: there is no reachable interactive
                // approval path, so say plainly that this is a deny gate rather
                // than letting the operator discover it one bricked tool at a
                // time.
                executor.set_approval_handler(Box::new(DenyApprovalHandler::new()));
                tracing::warn!(
                    "Approval gate enabled WITHOUT an approval store: every call to a module \
                     marked `requires_approval` will be DENIED. Interactive approval is not \
                     available on a CLI-launched server. Use `--acl` for a per-caller boundary, \
                     or embed apexe as a library with an ApprovalStore."
                );
            }
        }
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

/// Remove `display.mcp.alias` from a display overlay, in place.
///
/// apcore-mcp advertises `display.mcp.alias` as the MCP tool name, but routes
/// an incoming `tools/call` by treating the received tool name *as* a
/// `module_id` and looking it up in the registry — there is no alias-to-id
/// reverse map anywhere on that path. An alias that differs from the
/// `module_id` therefore produces a tool that is listed but cannot be called,
/// and apcore-toolkit's `sanitize_mcp_alias` rewrites `.` to `_`, so a dotted
/// `module_id` — the apcore convention, and what apexe emits — guarantees the
/// mismatch. Every tool apexe exposed over MCP failed with `ModuleNotFound`.
///
/// Dropping the alias makes apcore-mcp fall back to `module_id`, so the name it
/// advertises is the name that works. Only the MCP alias is removed: A2A keeps
/// the id and the display name in separate fields (`skill.id` / `skill.name`)
/// and never had this problem, and `mcp.description` / `mcp.guidance` are left
/// in place because only the *name* is load-bearing for routing.
fn strip_mcp_alias(display: &mut serde_json::Value) {
    if let Some(mcp) = display
        .get_mut("mcp")
        .and_then(serde_json::Value::as_object_mut)
    {
        mcp.remove("alias");
    }
}

/// Register `ScannedModule`s into a `Registry` as `CliModule`s.
///
/// Carries the full binding metadata (version, documentation, tags, and the
/// `DisplayResolver`-computed `metadata["display"]` overlay) into the
/// `ModuleDescriptor`, so apcore-mcp/apcore-a2a's alias-resolution and
/// rich-description logic (which reads `descriptor.display` /
/// `descriptor.metadata["display"]`) actually sees it — minus the MCP tool-name
/// alias, which [`strip_mcp_alias`] removes so the advertised name stays
/// callable.
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

                // Both places apcore-mcp looks for the alias must be cleared:
                // it reads `descriptor.display` first and only falls back to
                // `metadata["display"]`, so leaving either one intact would
                // still expose an uncallable tool name.
                let mut metadata = scanned.metadata.clone();
                if let Some(display) = metadata.get_mut("display") {
                    strip_mcp_alias(display);
                }
                let mut display = scanned.display.clone();
                if let Some(display) = display.as_mut() {
                    strip_mcp_alias(display);
                }

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
                    // Man-page invocations, carried through so apcore-a2a's
                    // skill mapper can render them. Dropping them here was why
                    // the A2A agent card advertised no examples at all even
                    // though the binding file has them.
                    examples: scanned.examples.clone(),
                    metadata,
                    display,
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
            filter: ModuleFilter::default(),
            audit_path: None,
            enable_logging: true,
            log_arguments: true,
            enable_approval: false,
            enable_circuit_breaker: true,
            enable_retry: true,
            approval_store: None,
        }
    }

    /// Two modules under distinct prefixes with distinct tags, written to a
    /// temp dir — the fixture the filter tests share.
    fn write_two_modules(dir: &Path) {
        let git = ScannedModule::new(
            "cli.git.log".to_string(),
            "Show commit logs".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string(), "git".to_string()],
            "exec:///usr/bin/git log".to_string(),
        );
        let cp = ScannedModule::new(
            "cli.cp".to_string(),
            "Copy files".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string(), "fileops".to_string()],
            "exec:///bin/cp".to_string(),
        );
        YamlOutput::without_verification()
            .write(&[git, cp], dir, false)
            .unwrap();
    }

    #[test]
    fn test_build_executor_prefix_filter_excludes_from_registry() {
        // Regression for #28: --prefix used to filter tools/list only, leaving
        // every excluded module callable by name. An excluded module must not
        // be in the registry at all.
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());

        let mut opts = opts(Some(dir.path()));
        opts.filter = ModuleFilter {
            prefix: Some("cli.git".to_string()),
            tags: None,
        };
        let executor = build_executor(&opts).unwrap();

        assert_eq!(executor.registry().count(), 1);
        assert!(executor
            .registry()
            .get_definition("cli.cp")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_build_executor_tags_filter_excludes_from_registry() {
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());

        let mut opts = opts(Some(dir.path()));
        opts.filter = ModuleFilter {
            prefix: None,
            tags: Some(vec!["git".to_string()]),
        };
        let executor = build_executor(&opts).unwrap();

        assert_eq!(executor.registry().count(), 1);
        assert!(executor
            .registry()
            .get_definition("cli.cp")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn test_build_executor_filtered_module_is_not_callable() {
        // The whole point of #28: the excluded module must fail to execute,
        // not merely fail to appear in a listing.
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());

        let mut opts = opts(Some(dir.path()));
        opts.filter = ModuleFilter {
            prefix: Some("zzz.".to_string()),
            tags: None,
        };
        let executor = build_executor(&opts).unwrap();

        let err = executor
            .call("cli.cp", json!({}), None, None)
            .await
            .expect_err("a filtered-out module must not be callable");
        assert_eq!(err.code, ErrorCode::ModuleNotFound);
    }

    #[test]
    fn test_module_filter_admits_everything_when_empty() {
        let module = ScannedModule::new(
            "cli.ls".to_string(),
            "List".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/ls".to_string(),
        );
        assert!(ModuleFilter::default().admits(&module));
    }

    #[test]
    fn test_module_filter_tags_require_all() {
        let module = ScannedModule::new(
            "cli.ls".to_string(),
            "List".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec!["cli".to_string(), "readonly".to_string()],
            "exec:///bin/ls".to_string(),
        );

        let one = ModuleFilter {
            prefix: None,
            tags: Some(vec!["readonly".to_string()]),
        };
        assert!(one.admits(&module));

        // AND logic: a tag the module lacks excludes it.
        let both = ModuleFilter {
            prefix: None,
            tags: Some(vec!["readonly".to_string(), "git".to_string()]),
        };
        assert!(!both.admits(&module));
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

    /// Write an ACL file with the given rule bodies (YAML fragments).
    fn write_acl(dir: &Path, rules_yaml: &str) -> std::path::PathBuf {
        let path = dir.join("acl.yaml");
        std::fs::write(&path, format!("default_effect: deny\nrules:\n{rules_yaml}")).unwrap();
        path
    }

    #[test]
    fn test_build_executor_fails_closed_on_empty_acl_target_list() {
        // #39 item 5(b): apcore accepts `targets: []` (only an OMITTED key is
        // rejected) and its matcher returns false for an empty pattern list, so
        // the deny never fires and the module runs. Refuse to start.
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());
        let acl_path = write_acl(
            dir.path(),
            "  - callers: [\"*\"]\n    targets: []\n    effect: deny\n",
        );

        let mut opts = opts(Some(dir.path()));
        opts.acl_path = Some(&acl_path);
        let err = build_executor(&opts).expect_err("an inert deny rule must refuse to start");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(
            err.message.contains("empty list"),
            "error should name the defect: {}",
            err.message
        );
    }

    #[test]
    fn test_build_executor_fails_closed_on_misspelled_acl_target() {
        // #39 item 5(a): `cli.cp` is registered but the rule names `cp`, so the
        // deny protects nothing while the module it meant stays callable.
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());
        let acl_path = write_acl(
            dir.path(),
            "  - callers: [\"*\"]\n    targets: [\"cp\"]\n    effect: deny\n",
        );

        let mut opts = opts(Some(dir.path()));
        opts.acl_path = Some(&acl_path);
        let err = build_executor(&opts).expect_err("a near-miss target must refuse to start");
        assert!(
            err.message.contains("cli.cp"),
            "error should name the spelling that works: {}",
            err.message
        );
    }

    #[test]
    fn test_build_executor_accepts_acl_targeting_registered_modules() {
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());
        let acl_path = write_acl(
            dir.path(),
            "  - callers: [\"*\"]\n    targets: [\"cli.cp\"]\n    effect: deny\n  \
             - callers: [\"*\"]\n    targets: [\"cli.git.*\"]\n    effect: allow\n",
        );

        let mut opts = opts(Some(dir.path()));
        opts.acl_path = Some(&acl_path);
        assert!(build_executor(&opts).is_ok(), "a correct ACL must load");
    }

    #[test]
    fn test_build_executor_tolerates_acl_target_for_filtered_out_module() {
        // One ACL file is meant to be shared across servers whose registries
        // differ. `cli.cp` is excluded by --prefix here, so its rule matches
        // nothing — that is a warning, not a refusal.
        let dir = TempDir::new().unwrap();
        write_two_modules(dir.path());
        let acl_path = write_acl(
            dir.path(),
            "  - callers: [\"*\"]\n    targets: [\"cli.cp\"]\n    effect: deny\n",
        );

        let mut opts = opts(Some(dir.path()));
        opts.acl_path = Some(&acl_path);
        opts.filter = ModuleFilter {
            prefix: Some("cli.git".to_string()),
            tags: None,
        };
        assert!(
            build_executor(&opts).is_ok(),
            "a target excluded by the module filter must warn, not refuse"
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
