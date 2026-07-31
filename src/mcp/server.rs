use std::collections::HashSet;
use std::io::Write;
use std::sync::Arc;

use apcore::{ErrorCode, Executor, ModuleError};
use apcore_mcp::{APCoreMCP, ApprovalStore, BackendSource};
use serde_json::Value;

use crate::auth::{resolve_auth, AuthOptions, ResolvedAuth};
use crate::module::{build_executor, ExecutorOptions, ModuleFilter};

/// Paths served without a credential when authentication is on.
///
/// Only `/health`: container and load-balancer probes need it and it reveals
/// nothing. `/metrics` is deliberately absent — apcore-mcp's middleware exempts
/// it by default, but its `module_id` labels and per-module call volumes are
/// reconnaissance about what this host wraps and what actually gets used.
fn exempt_paths() -> HashSet<String> {
    HashSet::from(["/health".to_string()])
}

/// Builder for creating an MCP server from apexe's scanned CLI modules.
///
/// Loads `.binding.yaml` files from a modules directory via
/// [`build_executor`](crate::module::build_executor), which wraps each as a
/// `CliModule`, registers them into an apcore `Registry`, and hands the
/// resulting [`Executor`] to apcore-mcp's [`APCoreMCP`] server.
///
/// # There is no `validate_inputs` toggle
///
/// Input validation is always on and cannot be turned off — it runs in
/// apcore's `input_validation` pipeline step inside `Executor::call`, which
/// apexe never removes. The builder used to expose a `validate_inputs(bool)`
/// method that forwarded to apcore-mcp's separate pre-dispatch hook; that hook
/// calls `McpExecutor::validate`, which `ApcoreExecutorAdapter` does not
/// implement, so the method toggled nothing. `apexe serve --skip-validation`
/// was deleted for the same reason and the library twin is now gone too.
pub struct McpServerBuilder {
    name: String,
    /// Value reported as `serverInfo.version` in the `initialize` response.
    /// Defaults to [`crate::VERSION`]; see [`Self::version`].
    version: String,
    transport: String,
    host: String,
    port: u16,
    explorer: bool,
    modules_dir: Option<std::path::PathBuf>,
    timeout_ms: u64,
    /// Filter exposed tools by tags (AND logic).
    tags: Option<Vec<String>>,
    /// Filter exposed tools by module ID prefix.
    prefix: Option<String>,
    /// Path to ACL YAML file for access control.
    acl_path: Option<std::path::PathBuf>,
    /// Path to the JSONL governance audit log (F5 §4.3). None disables auditing.
    audit_path: Option<std::path::PathBuf>,
    /// Enable LoggingMiddleware for structured execution logging.
    enable_logging: bool,
    /// Include call arguments and output in the structured log. See
    /// [`ExecutorOptions::log_arguments`](crate::module::ExecutorOptions::log_arguments).
    log_arguments: bool,
    /// Deny calls to modules annotated `requires_approval`. See
    /// [`DenyApprovalHandler`](crate::module::DenyApprovalHandler).
    enable_approval: bool,
    /// Credential required on the HTTP-family transports. See [`crate::auth`].
    auth: AuthOptions,
    /// Permit the deprecated SSE transport despite its known cross-client
    /// response delivery defect. See [`Self::resolve_transport`].
    allow_deprecated_sse: bool,
    /// Enable CircuitBreakerMiddleware (short-circuit a hanging/broken tool).
    enable_circuit_breaker: bool,
    /// Enable RetryMiddleware (retries only ever fire on idempotent timeouts).
    enable_retry: bool,
    /// Enable the `/metrics` (Prometheus) and `/usage` (JSON) observability
    /// endpoints. Only takes effect for HTTP/SSE transports; ignored (with a
    /// warning) for stdio, which has no HTTP surface to serve them on.
    enable_metrics: bool,
    /// Optional pluggable approval store (library-only, no CLI flag). See
    /// [`ExecutorOptions::approval_store`](crate::module::ExecutorOptions::approval_store).
    /// When set, also registers apcore-mcp's `__apcore_approval_check`
    /// meta-tool so an MCP client can poll a pending approval's status.
    approval_store: Option<Arc<dyn ApprovalStore>>,
}

impl McpServerBuilder {
    /// Create a new builder with sensible defaults.
    pub fn new() -> Self {
        Self {
            name: "apexe".to_string(),
            version: crate::VERSION.to_string(),
            transport: "stdio".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8000,
            explorer: false,
            modules_dir: None,
            timeout_ms: 30_000,
            tags: None,
            prefix: None,
            acl_path: None,
            audit_path: None,
            enable_logging: true,
            log_arguments: true,
            enable_approval: false,
            auth: AuthOptions::default(),
            allow_deprecated_sse: false,
            enable_circuit_breaker: true,
            enable_retry: true,
            enable_metrics: false,
            approval_store: None,
        }
    }

    /// Set the MCP server name.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set the version reported as `serverInfo.version`.
    ///
    /// Defaults to [`crate::VERSION`]. Left unset, apcore-mcp fills the field
    /// from its own crate version, so `initialize` told a client the apcore-mcp
    /// release rather than which apexe it was talking to.
    pub fn version(mut self, version: &str) -> Self {
        self.version = version.to_string();
        self
    }

    /// Set the transport protocol (`"stdio"`, `"http"`, or `"sse"`).
    pub fn transport(mut self, transport: &str) -> Self {
        self.transport = transport.to_string();
        self
    }

    /// Set the host address for HTTP/SSE transports.
    pub fn host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    /// Set the port for HTTP/SSE transports.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Enable or disable the built-in tool explorer UI.
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

    /// Filter exposed tools by tags (all must match).
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Filter exposed tools by module ID prefix.
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.to_string());
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

    /// Include call arguments and output in the structured log (default:
    /// enabled). Turning this off drops the payload from every log event, the
    /// error record included, and swaps in a payload-free failure record so a
    /// refused call is still announced. See
    /// [`ExecutorOptions::log_arguments`](crate::module::ExecutorOptions::log_arguments).
    pub fn log_arguments(mut self, enabled: bool) -> Self {
        self.log_arguments = enabled;
        self
    }

    /// Deny every call to a module annotated `requires_approval` (default:
    /// disabled). Interactive approval is not reachable from a CLI-launched
    /// server; see [`DenyApprovalHandler`](crate::module::DenyApprovalHandler).
    pub fn enable_approval(mut self, enabled: bool) -> Self {
        self.enable_approval = enabled;
        self
    }

    /// Set the credential required on the HTTP-family transports. Ignored for
    /// stdio. See [`crate::auth`] for the per-transport defaults.
    pub fn auth(mut self, options: AuthOptions) -> Self {
        self.auth = options;
        self
    }

    /// Permit `--transport sse` despite its known defects (default: refused).
    pub fn allow_deprecated_sse(mut self, allowed: bool) -> Self {
        self.allow_deprecated_sse = allowed;
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

    /// Enable the `/metrics` + `/usage` observability endpoints (default:
    /// disabled — this exposes internal call statistics over HTTP, so it's
    /// opt-in rather than on by default).
    pub fn enable_metrics(mut self, enabled: bool) -> Self {
        self.enable_metrics = enabled;
        self
    }

    /// Set a pluggable approval store, switching approvals to the
    /// non-blocking `StorageBackedApprovalHandler` and registering the
    /// `__apcore_approval_check` meta-tool. Library-only; see
    /// [`ExecutorOptions::approval_store`](crate::module::ExecutorOptions::approval_store).
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approval_store = Some(store);
        self
    }

    /// Load modules from binding files, register them, and build the MCP server.
    ///
    /// Returns the configured [`APCoreMCP`] instance ready to call `serve()`.
    // ModuleError is the crate-wide domain error; boxing it would diverge from
    // the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub fn build(self) -> Result<APCoreMCP, ModuleError> {
        let executor = build_executor(&self.executor_options())?;
        let transport = self.resolve_transport()?;
        self.build_mcp_server(executor, transport)
    }

    /// Options shared with the A2A builder for assembling a governed `Executor`.
    fn executor_options(&self) -> ExecutorOptions<'_> {
        ExecutorOptions {
            modules_dir: self.modules_dir.as_deref(),
            timeout_ms: self.timeout_ms,
            acl_path: self.acl_path.as_deref(),
            // The filter is applied at registration rather than being handed to
            // apcore-mcp, which only applies it to `tools/list`. See
            // [`ModuleFilter`].
            filter: ModuleFilter {
                prefix: self.prefix.clone(),
                tags: self.tags.clone(),
            },
            audit_path: self.audit_path.as_deref(),
            enable_logging: self.enable_logging,
            log_arguments: self.log_arguments,
            enable_approval: self.enable_approval,
            enable_circuit_breaker: self.enable_circuit_breaker,
            enable_retry: self.enable_retry,
            approval_store: self.approval_store.clone(),
        }
    }

    /// Map the user-facing transport name to the apcore-mcp transport string.
    ///
    /// `sse` is refused unless explicitly permitted. apcore-mcp's SSE handler
    /// shares one process-global channel across every connection, so responses
    /// are delivered round-robin to whichever stream is next: with two clients
    /// connected, one receives the other's tool output. It also drops one
    /// queued message per past disconnect while still answering
    /// `202 Accepted`, and never emits the `event: endpoint` a spec-compliant
    /// MCP SSE client waits for. The framework itself logs
    /// `SSE transport is deprecated` at startup; given the confidentiality
    /// impact, refusing by default is the honest reading of that. Streamable
    /// HTTP (`--transport http`) is unaffected.
    #[allow(clippy::result_large_err)]
    fn resolve_transport(&self) -> Result<&'static str, ModuleError> {
        match self.transport.as_str() {
            "stdio" => Ok("stdio"),
            "http" => Ok("streamable-http"),
            "sse" if self.allow_deprecated_sse => {
                tracing::warn!(
                    "SSE transport enabled despite a known confidentiality defect: with more \
                     than one concurrent connection, one client receives another client's tool \
                     output. Use a single connection only, or switch to --transport http."
                );
                Ok("sse")
            }
            "sse" => Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                "The SSE transport is deprecated and unsafe with more than one concurrent \
                 connection: responses are delivered round-robin across every open stream, so \
                 one client receives another client's tool output. Use `--transport http` \
                 (streamable HTTP), which is unaffected. If you accept the risk on a \
                 single-client server, pass `--allow-deprecated-sse`."
                    .to_string(),
            )),
            other => Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                format!("Unsupported transport: {other}"),
            )),
        }
    }

    /// Assemble the APCoreMCP server from a configured executor and transport.
    #[allow(clippy::result_large_err)]
    fn build_mcp_server(
        self,
        executor: Arc<Executor>,
        transport: &str,
    ) -> Result<APCoreMCP, ModuleError> {
        if self.enable_metrics && transport == "stdio" {
            tracing::warn!(
                "--metrics has no effect on stdio transport (no HTTP surface to serve \
                 /metrics or /usage on)"
            );
        }

        let effective_approval_store =
            Self::effective_approval_store(self.enable_approval, &self.approval_store);
        let auth = resolve_auth(&self.transport, &self.host, &self.auth)?;
        Self::announce_auth(&auth, &self.host, self.port, &mut std::io::stderr());

        let mut builder = APCoreMCP::builder()
            .backend(BackendSource::Executor(executor))
            .name(&self.name)
            .version(&self.version)
            .transport(transport)
            .host(&self.host)
            .port(self.port)
            // Left on unconditionally, with no builder method to turn it off.
            // The hook it gates calls `McpExecutor::validate`, which
            // apcore-mcp's `ApcoreExecutorAdapter` does not implement, so it
            // never fires for apexe's backend and there is nothing here for a
            // caller to toggle. `apexe serve --skip-validation` was deleted
            // for exactly this reason. Actual schema validation is unaffected:
            // it runs in apcore's `input_validation` pipeline step inside
            // `Executor::call`, which apexe never removes. Passing `true`
            // keeps the safe behaviour in place if a future adapter does
            // implement the hook.
            .validate_inputs(true)
            .require_auth(auth.require_auth())
            .exempt_paths(exempt_paths())
            .observability(self.enable_metrics && transport != "stdio");

        if let Some(authenticator) = auth.authenticator() {
            builder = builder.authenticator_arc(authenticator);
        }
        if self.explorer {
            builder = builder.include_explorer(true);
        }
        // The filter is already enforced in the registry (see
        // `executor_options`), so nothing excluded reaches apcore-mcp at all.
        // Handing it the same values keeps its own listing path consistent with
        // ours rather than relying on the registry being pre-filtered.
        if let Some(tags) = self.tags {
            builder = builder.tags(tags);
        }
        if let Some(ref prefix) = self.prefix {
            builder = builder.prefix(prefix);
        }
        // Only wire the store into APCoreMCP (which registers the
        // `__apcore_approval_check` meta-tool) when approval is actually
        // enabled on the Executor side (see `build_executor` in
        // `crate::module::registry`) — otherwise the meta-tool would be
        // exposed to MCP clients while the Executor never gates any call on
        // approval, so nothing would ever create a pending approval to check.
        if let Some(store) = effective_approval_store {
            builder = builder.approval_store(store);
        }

        builder.build().map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to build MCP server: {e}"),
            )
        })
    }

    /// Tell the operator what credential the server expects.
    ///
    /// The *fact* that a credential is required is an event, so it goes through
    /// `tracing`. The value of a generated token does not: it is written
    /// straight to `notice_sink` — in production, stderr. A token supplied by
    /// the operator is never echoed back at all.
    ///
    /// The direct write is deliberate, for two reasons.
    ///
    /// It has to be unconditional. `resolve_auth` installs the token whatever
    /// the log level is, so at `--log-level warn` a `tracing::info!` would be
    /// dropped while the server still demanded the credential — and no apexe
    /// command can recover the value afterwards, since `--show-config` omits
    /// credentials by construction. That is a server nothing can ever call.
    ///
    /// It also has to stay off the log pipeline. `tracing` goes wherever the
    /// operator pointed it — a file, journald, an aggregator — so routing the
    /// credential that guards a remote-execution surface through it persists
    /// the secret and often ships it off-host. That is the same concern
    /// `--no-log-arguments` exists for one layer down.
    fn announce_auth(auth: &ResolvedAuth, host: &str, port: u16, notice_sink: &mut dyn Write) {
        match auth {
            ResolvedAuth::Disabled => {}
            ResolvedAuth::Token {
                token, generated, ..
            } => {
                if *generated {
                    tracing::info!("Bearer token authentication enabled (token generated)");
                    // Best-effort: a server that cannot reach stderr should
                    // still start, and there is no better channel left to
                    // report the failure on.
                    let _ = writeln!(
                        notice_sink,
                        "{}",
                        Self::generated_token_notice(token, host, port)
                    );
                } else {
                    tracing::info!("Bearer token authentication enabled (token supplied)");
                }
            }
            ResolvedAuth::Jwt { .. } => {
                tracing::info!("JWT authentication enabled");
            }
        }
    }

    /// The startup notice for a generated bearer token.
    fn generated_token_notice(token: &str, host: &str, port: u16) -> String {
        format!(
            "Bearer token authentication enabled. Connect to http://{host}:{port} with header:\n\
             \x20   Authorization: Bearer {token}\n\
             Pass --auth-token or set APEXE_AUTH_TOKEN to pin your own value, or --auth none to \
             disable."
        )
    }

    /// Whether `approval_store` should actually be threaded into
    /// `APCoreMCP` — kept in sync with `build_executor`'s own
    /// `opts.enable_approval` gate so the MCP-side and Executor-side
    /// approval wiring can never disagree about whether approval is on.
    fn effective_approval_store(
        enable_approval: bool,
        approval_store: &Option<Arc<dyn ApprovalStore>>,
    ) -> Option<Arc<dyn ApprovalStore>> {
        if enable_approval {
            approval_store.clone()
        } else {
            None
        }
    }
}

impl McpServerBuilder {
    /// Export registered tools as OpenAI-compatible function calling definitions.
    ///
    /// Loads modules, registers them, and converts to OpenAI format without
    /// starting a server.
    #[allow(clippy::result_large_err)]
    pub fn export_openai_tools(self) -> Result<Vec<Value>, ModuleError> {
        let executor = build_executor(&self.executor_options())?;

        let openai_config = apcore_mcp::OpenAIToolsConfig {
            embed_annotations: true,
            strict: false,
            tags: self.tags.clone(),
            prefix: self.prefix.clone(),
        };

        apcore_mcp::to_openai_tools(BackendSource::Executor(executor), openai_config).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to export OpenAI tools: {e}"),
            )
        })
    }
}

impl Default for McpServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::YamlOutput;
    use apcore_toolkit::ScannedModule;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_mcp_server_builder_defaults() {
        let builder = McpServerBuilder::new();
        assert_eq!(builder.name, "apexe");
        assert_eq!(builder.transport, "stdio");
        assert_eq!(builder.host, "127.0.0.1");
        assert_eq!(builder.port, 8000);
        assert!(!builder.explorer);
        assert!(builder.modules_dir.is_none());
        assert_eq!(builder.timeout_ms, 30_000);
    }

    #[test]
    fn test_mcp_server_builder_reports_apexe_version_by_default() {
        // #35 item 4: `serverInfo.version` used to report apcore-mcp's crate
        // version (0.17.2), so an MCP client had no way to learn which apexe it
        // was talking to.
        let builder = McpServerBuilder::new();
        assert_eq!(builder.version, crate::VERSION);
        assert_eq!(builder.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_mcp_server_builder_version_override() {
        let builder = McpServerBuilder::new().version("9.9.9");
        assert_eq!(builder.version, "9.9.9");
    }

    #[test]
    fn test_mcp_server_builder_chain() {
        let builder = McpServerBuilder::new()
            .name("my-server")
            .transport("http")
            .host("0.0.0.0")
            .port(9090)
            .explorer(true)
            .modules_dir("/tmp/modules")
            .timeout_ms(60_000);

        assert_eq!(builder.name, "my-server");
        assert_eq!(builder.transport, "http");
        assert_eq!(builder.host, "0.0.0.0");
        assert_eq!(builder.port, 9090);
        assert!(builder.explorer);
        assert_eq!(
            builder.modules_dir,
            Some(std::path::PathBuf::from("/tmp/modules"))
        );
        assert_eq!(builder.timeout_ms, 60_000);
    }

    #[test]
    fn test_mcp_server_builder_invalid_transport() {
        let result = McpServerBuilder::new().transport("invalid").build();
        assert!(result.is_err());
        let err = result.err().expect("expected Err variant");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(
            err.message.contains("Unsupported transport"),
            "error should mention unsupported transport: {}",
            err.message
        );
    }

    #[test]
    fn test_mcp_server_builder_no_modules_dir() {
        let result = McpServerBuilder::new().build();
        assert!(result.is_ok(), "build without modules_dir should succeed");
    }

    #[test]
    fn test_mcp_server_builder_with_modules() {
        let dir = TempDir::new().unwrap();

        let modules = vec![ScannedModule::new(
            "echo.hello".to_string(),
            "Echo hello".to_string(),
            json!({"type": "object", "properties": {"message": {"type": "string"}}}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo hello".to_string(),
        )];

        let output = YamlOutput::without_verification();
        output.write(&modules, dir.path(), false).unwrap();

        let result = McpServerBuilder::new().modules_dir(dir.path()).build();
        assert!(
            result.is_ok(),
            "build with valid modules should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_mcp_server_builder_nonexistent_modules_dir() {
        let result = McpServerBuilder::new()
            .modules_dir("/nonexistent/path/xyz_12345")
            .build();
        // Should succeed with zero tools (warns but does not error)
        assert!(
            result.is_ok(),
            "nonexistent dir should warn but succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_mcp_server_builder_default_impl() {
        let builder = McpServerBuilder::default();
        assert_eq!(builder.name, "apexe");
        assert_eq!(builder.transport, "stdio");
    }

    #[test]
    fn test_mcp_server_builder_tags_filter() {
        let builder = McpServerBuilder::new().tags(vec!["readonly".to_string()]);
        assert_eq!(builder.tags, Some(vec!["readonly".to_string()]));
    }

    #[test]
    fn test_mcp_server_builder_prefix_filter() {
        let builder = McpServerBuilder::new().prefix("cli.git");
        assert_eq!(builder.prefix, Some("cli.git".to_string()));
    }

    #[test]
    fn test_mcp_server_builder_logging_default_enabled() {
        let builder = McpServerBuilder::new();
        assert!(builder.enable_logging);
    }

    #[test]
    fn test_mcp_server_builder_approval_default_disabled() {
        let builder = McpServerBuilder::new();
        assert!(!builder.enable_approval);
    }

    #[test]
    fn test_mcp_server_builder_metrics_default_disabled() {
        let builder = McpServerBuilder::new();
        assert!(!builder.enable_metrics);
    }

    #[test]
    fn test_mcp_server_builder_metrics_ignored_on_stdio() {
        // Should still build successfully; observability is silently
        // skipped for stdio (with a warning), not an error.
        let result = McpServerBuilder::new().enable_metrics(true).build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_mcp_server_builder_with_logging_and_approval() {
        let result = McpServerBuilder::new()
            .enable_logging(true)
            .enable_approval(true)
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_effective_approval_store_requires_enable_approval() {
        // Regression for the WARNING finding: approval_store used to be
        // threaded into APCoreMCP unconditionally, while build_executor only
        // wires an ApprovalHandler into the Executor when enable_approval is
        // true. That asymmetry would advertise the __apcore_approval_check
        // meta-tool to MCP clients while the Executor never actually gates
        // any call on approval -- nothing would ever create a pending
        // approval for the meta-tool to check.
        use apcore_mcp::InMemoryApprovalStore;
        let store: Arc<dyn ApprovalStore> = Arc::new(InMemoryApprovalStore::new());

        assert!(
            McpServerBuilder::effective_approval_store(false, &Some(store.clone())).is_none(),
            "approval_store must be dropped when enable_approval is false"
        );
        assert!(
            McpServerBuilder::effective_approval_store(true, &Some(store.clone())).is_some(),
            "approval_store must be kept when enable_approval is true"
        );
        assert!(McpServerBuilder::effective_approval_store(true, &None).is_none());
    }

    #[test]
    fn test_mcp_server_builder_approval_store_without_enable_approval_still_builds() {
        use apcore_mcp::InMemoryApprovalStore;
        let store: Arc<dyn ApprovalStore> = Arc::new(InMemoryApprovalStore::new());
        // enable_approval left at its default (false) — the store must be
        // silently ignored rather than producing an inconsistent server.
        let result = McpServerBuilder::new().approval_store(store).build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_mcp_server_builder_refuses_sse_by_default() {
        // Regression for #27: SSE shares one process-global channel across
        // every connection, so one client receives another's tool output.
        let result = McpServerBuilder::new().transport("sse").build();
        let err = result.err().expect("sse must be refused by default");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(
            err.message.contains("--allow-deprecated-sse"),
            "error should name the opt-out: {}",
            err.message
        );
    }

    #[test]
    fn test_mcp_server_builder_allows_sse_when_acknowledged() {
        let result = McpServerBuilder::new()
            .transport("sse")
            .allow_deprecated_sse(true)
            .build();
        assert!(
            result.is_ok(),
            "acknowledged sse should build: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_mcp_server_builder_refuses_unauthenticated_public_bind() {
        // Regression for #31: an unauthenticated non-loopback bind exposes
        // every wrapped binary on the host.
        use crate::auth::{AuthMode, AuthOptions};
        let result = McpServerBuilder::new()
            .transport("http")
            .host("0.0.0.0")
            .auth(AuthOptions {
                mode: Some(AuthMode::None),
                ..AuthOptions::default()
            })
            .build();
        assert!(
            result.is_err(),
            "--auth none on a public bind must refuse to start"
        );
    }

    #[test]
    fn test_mcp_server_builder_stdio_needs_no_auth() {
        // stdio's boundary is the parent/child process relationship; requiring
        // a token there would break every desktop MCP client config.
        let result = McpServerBuilder::new().transport("stdio").build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_exempt_paths_covers_health_but_not_metrics() {
        let paths = exempt_paths();
        assert!(paths.contains("/health"));
        assert!(
            !paths.contains("/metrics"),
            "/metrics exposes module_id labels and call volumes"
        );
    }

    #[test]
    fn test_mcp_server_builder_log_arguments_default_enabled() {
        assert!(McpServerBuilder::new().log_arguments);
    }

    #[test]
    fn test_export_openai_tools_empty() {
        let result = McpServerBuilder::new().export_openai_tools();
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_export_openai_tools_with_modules() {
        let dir = TempDir::new().unwrap();
        let modules = vec![ScannedModule::new(
            "echo.test".to_string(),
            "Test tool".to_string(),
            json!({"type": "object", "properties": {"msg": {"type": "string"}}}),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo test".to_string(),
        )];

        let output = YamlOutput::without_verification();
        output.write(&modules, dir.path(), false).unwrap();

        let result = McpServerBuilder::new()
            .modules_dir(dir.path())
            .export_openai_tools();
        assert!(result.is_ok());
        let tools = result.unwrap();
        assert!(!tools.is_empty());
        // OpenAI format has "type": "function" and "function" key
        assert_eq!(tools[0]["type"], "function");
        assert!(tools[0]["function"]["name"].is_string());
    }

    #[test]
    fn test_generated_token_notice_carries_the_token_and_endpoint() {
        let notice = McpServerBuilder::generated_token_notice("deadbeef", "127.0.0.1", 9000);
        assert!(
            notice.contains("Authorization: Bearer deadbeef"),
            "{notice}"
        );
        assert!(notice.contains("http://127.0.0.1:9000"), "{notice}");
        assert!(notice.contains("--auth-token"), "{notice}");
        assert!(notice.contains("APEXE_AUTH_TOKEN"), "{notice}");
    }

    fn announce(auth: &ResolvedAuth) -> String {
        let mut sink: Vec<u8> = Vec::new();
        McpServerBuilder::announce_auth(auth, "127.0.0.1", 8000, &mut sink);
        String::from_utf8(sink).expect("the notice is UTF-8")
    }

    #[test]
    fn test_announce_auth_writes_a_generated_token_to_the_notice_sink() {
        // The token must not depend on `tracing`: `resolve_auth` installs it
        // whatever the log level is, so an `info!` dropped at `--log-level
        // warn` leaves a server whose credential nothing can recover.
        let auth = resolve_auth("http", "127.0.0.1", &AuthOptions::default()).unwrap();
        let ResolvedAuth::Token {
            ref token,
            generated,
            ..
        } = auth
        else {
            panic!("http on loopback defaults to a generated token");
        };
        assert!(generated);
        let expected = token.clone();
        assert!(announce(&auth).contains(&expected));
    }

    #[test]
    fn test_announce_auth_never_echoes_a_supplied_token() {
        let opts = AuthOptions {
            token: Some("operator-secret".to_string()),
            ..AuthOptions::default()
        };
        let auth = resolve_auth("http", "127.0.0.1", &opts).unwrap();
        let notice = announce(&auth);
        assert!(!notice.contains("operator-secret"), "{notice}");
        assert!(notice.is_empty(), "{notice}");
    }

    #[test]
    fn test_announce_auth_writes_nothing_when_auth_is_disabled() {
        let auth = resolve_auth("stdio", "127.0.0.1", &AuthOptions::default()).unwrap();
        assert!(announce(&auth).is_empty());
    }
}
