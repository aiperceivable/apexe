use std::sync::Arc;

use apcore::{ErrorCode, ModuleError};
use apcore_a2a::{APCoreA2AConfig, BackendSource};
use apcore_mcp::ApprovalStore;

use crate::module::{build_executor, ExecutorOptions};

/// Builder for creating an A2A agent server from apexe's scanned CLI modules.
///
/// Shares [`build_executor`](crate::module::build_executor) with
/// [`crate::mcp::McpServerBuilder`], so an ACL policy, the logging
/// middleware, and the approval handler apply identically whether a module
/// is served over MCP or A2A.
pub struct A2aServerBuilder {
    name: String,
    url: String,
    explorer: bool,
    modules_dir: Option<std::path::PathBuf>,
    timeout_ms: u64,
    /// Path to ACL YAML file for access control.
    acl_path: Option<std::path::PathBuf>,
    /// Path to the JSONL governance audit log (F5 §4.3). None disables auditing.
    audit_path: Option<std::path::PathBuf>,
    /// Enable LoggingMiddleware for structured execution logging.
    enable_logging: bool,
    /// Enable ElicitationApprovalHandler for destructive command approval.
    enable_approval: bool,
    /// Enable CircuitBreakerMiddleware (short-circuit a hanging/broken tool).
    enable_circuit_breaker: bool,
    /// Enable RetryMiddleware (retries only ever fire on idempotent timeouts).
    enable_retry: bool,
    /// Optional pluggable approval store (library-only, no CLI flag). See
    /// [`ExecutorOptions::approval_store`](crate::module::ExecutorOptions::approval_store).
    /// apcore-a2a has no meta-tool equivalent to MCP's
    /// `__apcore_approval_check`; setting this only affects how the shared
    /// `Executor` handles a `requires_approval` call (non-blocking vs. the
    /// default synchronous elicitation).
    approval_store: Option<Arc<dyn ApprovalStore>>,
    /// Per-task execution timeout in seconds (A2A tasks run async).
    execution_timeout: u64,
    /// Allowed CORS origins. Empty = no CORS layer.
    cors_origins: Vec<String>,
}

impl A2aServerBuilder {
    /// Create a new builder with sensible defaults.
    pub fn new() -> Self {
        Self {
            name: "apexe".to_string(),
            url: "http://127.0.0.1:8000".to_string(),
            explorer: false,
            modules_dir: None,
            timeout_ms: 30_000,
            acl_path: None,
            audit_path: None,
            enable_logging: true,
            enable_approval: false,
            enable_circuit_breaker: true,
            enable_retry: true,
            approval_store: None,
            execution_timeout: 300,
            cors_origins: vec![],
        }
    }

    /// Set the A2A agent name.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set the base URL to bind the A2A server to (e.g. `http://0.0.0.0:8000`).
    pub fn url(mut self, url: &str) -> Self {
        self.url = url.to_string();
        self
    }

    /// Enable or disable the built-in Explorer UI.
    pub fn explorer(mut self, enabled: bool) -> Self {
        self.explorer = enabled;
        self
    }

    /// Set the directory containing `.binding.yaml` module files.
    pub fn modules_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.modules_dir = Some(dir.into());
        self
    }

    /// Set the subprocess execution timeout in milliseconds.
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Set ACL config file path for access control on the Executor.
    pub fn audit_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.audit_path = Some(path.into());
        self
    }

    pub fn acl_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.acl_path = Some(path.into());
        self
    }

    /// Enable or disable structured logging middleware (default: enabled).
    pub fn enable_logging(mut self, enabled: bool) -> Self {
        self.enable_logging = enabled;
        self
    }

    /// Enable ElicitationApprovalHandler for destructive commands.
    pub fn enable_approval(mut self, enabled: bool) -> Self {
        self.enable_approval = enabled;
        self
    }

    /// Enable or disable CircuitBreakerMiddleware (default: enabled).
    pub fn enable_circuit_breaker(mut self, enabled: bool) -> Self {
        self.enable_circuit_breaker = enabled;
        self
    }

    /// Enable or disable RetryMiddleware (default: enabled).
    pub fn enable_retry(mut self, enabled: bool) -> Self {
        self.enable_retry = enabled;
        self
    }

    /// Set a pluggable approval store, switching approvals to the
    /// non-blocking `StorageBackedApprovalHandler`. Library-only; see
    /// [`ExecutorOptions::approval_store`](crate::module::ExecutorOptions::approval_store).
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approval_store = Some(store);
        self
    }

    /// Set the per-task execution timeout in seconds.
    pub fn execution_timeout(mut self, secs: u64) -> Self {
        self.execution_timeout = secs;
        self
    }

    /// Set the allowed CORS origins (empty disables the CORS layer).
    pub fn cors_origins(mut self, origins: Vec<String>) -> Self {
        self.cors_origins = origins;
        self
    }

    /// Options shared with the MCP builder for assembling a governed `Executor`.
    fn executor_options(&self) -> ExecutorOptions<'_> {
        ExecutorOptions {
            modules_dir: self.modules_dir.as_deref(),
            timeout_ms: self.timeout_ms,
            acl_path: self.acl_path.as_deref(),
            audit_path: self.audit_path.as_deref(),
            enable_logging: self.enable_logging,
            enable_approval: self.enable_approval,
            enable_circuit_breaker: self.enable_circuit_breaker,
            enable_retry: self.enable_retry,
            approval_store: self.approval_store.clone(),
        }
    }

    /// Load modules, register them, and serve as an A2A agent until the
    /// server stops or errors. Must be driven from a Tokio runtime.
    // ModuleError is the crate-wide domain error; boxing it would diverge from
    // the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub async fn serve(self) -> Result<(), ModuleError> {
        if self.enable_approval && self.approval_store.is_none() {
            return Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                "A2A server has no session/elicitation mechanism, so --enable-approval without \
                 an approval_store would reject every requires_approval call",
            )
            .with_retryable(false)
            .with_ai_guidance(
                "apcore-a2a has no MCP-style session/elicitation to prompt a human for \
                 approval, so the default ElicitationApprovalHandler can never resolve here. \
                 Provide `.approval_store(...)` (a persistent ApprovalStore) via the library \
                 API, or disable `.enable_approval(false)`.",
            ));
        }

        let executor = build_executor(&self.executor_options())?;

        let config = APCoreA2AConfig {
            name: self.name.clone(),
            description: format!("apexe A2A agent '{}'", self.name),
            url: self.url.clone(),
            execution_timeout: self.execution_timeout,
            explorer: self.explorer,
            sys_modules: false,
            cors_origins: self.cors_origins.clone(),
            ..APCoreA2AConfig::default()
        };

        apcore_a2a::async_serve(BackendSource::Executor(executor), config)
            .await
            .map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("A2A server error: {e}"),
                )
            })
    }
}

impl Default for A2aServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a2a_server_builder_defaults() {
        let builder = A2aServerBuilder::new();
        assert_eq!(builder.name, "apexe");
        assert_eq!(builder.url, "http://127.0.0.1:8000");
        assert!(!builder.explorer);
        assert!(builder.modules_dir.is_none());
        assert_eq!(builder.timeout_ms, 30_000);
        assert_eq!(builder.execution_timeout, 300);
        assert!(builder.cors_origins.is_empty());
    }

    #[test]
    fn test_a2a_server_builder_chain() {
        let builder = A2aServerBuilder::new()
            .name("my-agent")
            .url("http://0.0.0.0:9090")
            .explorer(true)
            .modules_dir("/tmp/modules")
            .timeout_ms(60_000)
            .execution_timeout(600)
            .cors_origins(vec!["https://example.com".to_string()]);

        assert_eq!(builder.name, "my-agent");
        assert_eq!(builder.url, "http://0.0.0.0:9090");
        assert!(builder.explorer);
        assert_eq!(
            builder.modules_dir,
            Some(std::path::PathBuf::from("/tmp/modules"))
        );
        assert_eq!(builder.timeout_ms, 60_000);
        assert_eq!(builder.execution_timeout, 600);
        assert_eq!(builder.cors_origins, vec!["https://example.com"]);
    }

    #[test]
    fn test_a2a_server_builder_default_impl() {
        let builder = A2aServerBuilder::default();
        assert_eq!(builder.name, "apexe");
        assert_eq!(builder.url, "http://127.0.0.1:8000");
    }

    #[test]
    fn test_a2a_server_builder_logging_default_enabled() {
        let builder = A2aServerBuilder::new();
        assert!(builder.enable_logging);
        assert!(!builder.enable_approval);
    }

    #[tokio::test]
    async fn test_a2a_server_builder_empty_registry_errors() {
        // apcore-a2a refuses to serve an empty registry (APCoreA2AError::EmptyRegistry),
        // which build_executor surfaces as a generic internal error here.
        let result = A2aServerBuilder::new().serve().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_a2a_server_builder_enable_approval_without_store_fails_fast() {
        // apcore-a2a has no session/elicitation mechanism, so
        // ElicitationApprovalHandler(None) would reject every requires_approval
        // call. serve() must refuse to start instead of silently building a
        // permanently-broken approval handler.
        let result = A2aServerBuilder::new().enable_approval(true).serve().await;
        let err = result.expect_err("enable_approval without approval_store must fail fast");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(err
            .ai_guidance
            .as_ref()
            .expect("guidance should explain the fix")
            .contains("approval_store"));
    }

    #[tokio::test]
    async fn test_a2a_server_builder_enable_approval_with_store_does_not_fail_fast() {
        use apcore_mcp::InMemoryApprovalStore;

        let store: Arc<dyn ApprovalStore> = Arc::new(InMemoryApprovalStore::new());
        let result = A2aServerBuilder::new()
            .enable_approval(true)
            .approval_store(store)
            .serve()
            .await;
        // Still fails (empty registry), but NOT via the fail-fast validation —
        // proves the check is specific to the missing-store case.
        let err = result.expect_err("empty registry should still error");
        assert_ne!(err.code, ErrorCode::GeneralInvalidInput);
    }
}
