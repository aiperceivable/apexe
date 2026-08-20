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
    /// Gate calls to modules annotated `requires_approval` on a human decision. See
    /// [`ApprovalGate`](crate::module::ApprovalGate).
    enable_approval: bool,
    /// Credential required on the HTTP-family transports. See [`crate::auth`].
    auth: AuthOptions,
    /// Accepted and ignored. The cross-client delivery defect it acknowledged
    /// was fixed in apcore-mcp 0.18; see [`Self::allow_deprecated_sse`].
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

    /// Set the JSONL governance audit-log path; `None` disables auditing.
    pub fn audit_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.audit_path = Some(path.into());
        self
    }

    /// Set the ACL policy file path for access control on the Executor.
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

    /// Gate every call to a module annotated `requires_approval` on a human
    /// decision from the connected MCP client (default: disabled). A client
    /// that cannot be prompted is refused; see
    /// [`ApprovalGate`](crate::module::ApprovalGate).
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

    /// Deprecated no-op, kept so an existing call still compiles.
    ///
    /// It used to be the acknowledgement `--transport sse` required, back when
    /// apcore-mcp delivered one client's responses to another. apcore-mcp 0.18
    /// scopes a session per connection, so SSE needs no acknowledgement and
    /// this setting decides nothing. Passing `true` warns; the value is
    /// otherwise recorded and unused, and the setter will be removed in a later
    /// release.
    pub fn allow_deprecated_sse(mut self, allowed: bool) -> Self {
        if allowed {
            tracing::warn!(
                "allow_deprecated_sse no longer decides anything: the defect it acknowledged \
                 was fixed in apcore-mcp 0.18, and SSE is served (with a deprecation warning) \
                 without it."
            );
        }
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
    /// `sse` was refused outright while apcore-mcp shared one process-global
    /// channel across every connection: responses went round-robin to whichever
    /// stream was next, so with two clients connected one received the other's
    /// tool output. apcore-mcp 0.18 scopes a session per connection and emits
    /// the `event: endpoint` a spec-compliant client waits for, so the
    /// confidentiality defect that justified refusing is gone — and the refusal
    /// went with it. The `apcore-mcp = "0.18"` requirement is what makes that
    /// safe to assume: a build that could resolve the broken 0.17 would need a
    /// manifest change, not a lockfile drift.
    ///
    /// SSE stays *deprecated* upstream — apcore-mcp still says so at startup —
    /// so it is served with a warning rather than silently, and streamable HTTP
    /// (`--transport http`) remains the recommendation.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    fn resolve_transport(&self) -> Result<&'static str, ModuleError> {
        match self.transport.as_str() {
            "stdio" => Ok("stdio"),
            "http" => Ok("streamable-http"),
            "sse" => {
                tracing::warn!(
                    "The SSE transport is deprecated upstream; prefer --transport http \
                     (streamable HTTP). The cross-client response delivery defect that made \
                     apexe refuse this transport was fixed in apcore-mcp 0.18."
                );
                Ok("sse")
            }
            other => Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                format!("Unsupported transport: {other}"),
            )),
        }
    }

    /// Assemble the APCoreMCP server from a configured executor and transport.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
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

        let auth = resolve_auth(&self.transport, &self.host, &self.auth)?;
        Self::announce_auth(&auth, &self.host, self.port, &mut std::io::stderr());

        let builder = APCoreMCP::builder()
            .backend(BackendSource::Executor(executor))
            .name(&self.name)
            .version(&self.version)
            .transport(transport)
            .host(&self.host)
            .port(self.port)
            // Always on; see the `validate_inputs` note on `McpServerBuilder`.
            .validate_inputs(true)
            .require_auth(auth.require_auth())
            .exempt_paths(exempt_paths())
            .observability(self.enable_metrics && transport != "stdio");

        self.apply_optional_wiring(builder, &auth)
            .build()
            .map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("Failed to build MCP server: {e}"),
                )
            })
    }

    /// Apply the settings that are only present for some configurations.
    ///
    /// Split out of [`Self::build_mcp_server`] so the unconditional server
    /// identity (backend, name, transport, bind address) reads as one block
    /// and every "only when the operator asked for it" decision reads as
    /// another.
    fn apply_optional_wiring(
        self,
        mut builder: apcore_mcp::APCoreMCPBuilder,
        auth: &ResolvedAuth,
    ) -> apcore_mcp::APCoreMCPBuilder {
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
        if let Some(store) =
            Self::effective_approval_store(self.enable_approval, &self.approval_store)
        {
            builder = builder.approval_store(store);
        }
        builder
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
                    Self::announce_cleartext(host, notice_sink);
                }
            }
            ResolvedAuth::Jwt { .. } => {
                tracing::info!("JWT authentication enabled");
                Self::announce_cleartext(host, notice_sink);
            }
        }
    }

    /// The cleartext warning for a bind that carries a credential over plain
    /// HTTP, or `None` on loopback where the traffic reaches no interface.
    fn cleartext_caveat(host: &str) -> Option<String> {
        if crate::auth::is_loopback_host(host) {
            return None;
        }
        Some(
            "\nWARNING: this is a non-loopback bind serving plain HTTP, so the credential it \
             requires travels in cleartext and anything on the path can replay it. Put apexe \
             behind a TLS-terminating reverse proxy before using it across a network."
                .to_string(),
        )
    }

    /// Write the cleartext caveat on its own, for the arms that build no notice.
    ///
    /// A supplied `--auth-token` and `--auth jwt` are in exactly the same
    /// hazard as a generated token — `resolve_auth` calls it "the one failure
    /// that silently undoes the whole of the auth default" — but neither has a
    /// value to print, so neither reached `notice_sink` and the only warning
    /// left for them was the `tracing::warn!`. That is the channel this notice
    /// exists to route around: at `--log-level error` it is dropped, and the
    /// manual promises both.
    fn announce_cleartext(host: &str, notice_sink: &mut dyn Write) {
        if let Some(caveat) = Self::cleartext_caveat(host) {
            let _ = writeln!(notice_sink, "{}", caveat.trim_start());
        }
    }

    /// The startup notice for a generated bearer token.
    ///
    /// On a non-loopback bind the notice carries the cleartext caveat itself,
    /// rather than leaving it to the `tracing::warn!` that `resolve_auth`
    /// emits. This notice is the line an operator copies, and it is on stderr
    /// precisely because `tracing` can be redirected or dropped by log level —
    /// at `--log-level error` the operator would read "connect to http://..."
    /// with the warning nowhere in sight.
    fn generated_token_notice(token: &str, host: &str, port: u16) -> String {
        let caveat = Self::cleartext_caveat(host).unwrap_or_default();
        format!(
            "Bearer token authentication enabled. Connect to http://{host}:{port} with header:\n\
             \x20   Authorization: Bearer {token}\n\
             Pass --auth-token or set APEXE_AUTH_TOKEN to pin your own value, or --auth none to \
             disable.{caveat}"
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
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
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
    fn test_mcp_server_builder_serves_sse_without_an_acknowledgement() {
        // apexe refused SSE outright while apcore-mcp delivered one client's
        // responses to another (#27). apcore-mcp 0.18 scopes a session per
        // connection, so the refusal — and the flag that opted out of it — no
        // longer have a defect to guard.
        let result = McpServerBuilder::new().transport("sse").build();
        assert!(
            result.is_ok(),
            "sse must build without an acknowledgement: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_mcp_server_builder_still_accepts_the_retired_sse_acknowledgement() {
        // The setter shipped, so an existing call must keep compiling and
        // building; it simply decides nothing now.
        let result = McpServerBuilder::new()
            .transport("sse")
            .allow_deprecated_sse(true)
            .build();
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[test]
    fn test_mcp_server_builder_still_rejects_an_unknown_transport() {
        // Relaxing sse must not turn `resolve_transport` into a passthrough.
        let err = McpServerBuilder::new()
            .transport("carrier-pigeon")
            .build()
            .err()
            .expect("an unknown transport must still be refused");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(err.message.contains("Unsupported transport"));
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

    #[test]
    fn test_generated_token_notice_warns_that_a_public_bind_is_cleartext() {
        // `resolve_auth` warns through `tracing`, which the operator can
        // redirect or filter out by level. This notice is the line they
        // copy — at `--log-level error` it would otherwise read "connect to
        // http://..." with nothing saying the token crosses the wire in the
        // clear, which is exactly the gap that makes the auth default moot.
        let notice = McpServerBuilder::generated_token_notice("deadbeef", "0.0.0.0", 9000);
        assert!(notice.contains("cleartext"), "{notice}");
        assert!(notice.contains("reverse proxy"), "{notice}");
    }

    #[test]
    fn test_a_supplied_token_on_a_public_bind_still_warns_about_cleartext() {
        // The hazard is the transport, not who minted the credential. This arm
        // builds no notice — there is no value to print — so before this the
        // only warning was the `tracing::warn!` in resolve_auth, which is the
        // channel the stderr notice exists to route around: at `--log-level
        // error` the operator saw nothing at all.
        let opts = AuthOptions {
            token: Some("operator-secret".to_string()),
            ..AuthOptions::default()
        };
        let auth = resolve_auth("http", "0.0.0.0", &opts).unwrap();
        let mut sink: Vec<u8> = Vec::new();
        McpServerBuilder::announce_auth(&auth, "0.0.0.0", 9000, &mut sink);
        let notice = String::from_utf8(sink).unwrap();

        assert!(notice.contains("cleartext"), "{notice}");
        assert!(
            !notice.contains("operator-secret"),
            "a supplied token is still never echoed: {notice}"
        );
    }

    #[test]
    fn test_jwt_on_a_public_bind_still_warns_about_cleartext() {
        let opts = AuthOptions {
            mode: Some(crate::auth::AuthMode::Jwt),
            jwt_secret: Some("s3cret".to_string()),
            ..AuthOptions::default()
        };
        let auth = resolve_auth("http", "0.0.0.0", &opts).unwrap();
        let mut sink: Vec<u8> = Vec::new();
        McpServerBuilder::announce_auth(&auth, "0.0.0.0", 9000, &mut sink);
        let notice = String::from_utf8(sink).unwrap();

        assert!(notice.contains("cleartext"), "{notice}");
        assert!(
            !notice.contains("s3cret"),
            "the secret is never echoed: {notice}"
        );
    }

    #[test]
    fn test_a_supplied_token_on_loopback_stays_silent() {
        let opts = AuthOptions {
            token: Some("operator-secret".to_string()),
            ..AuthOptions::default()
        };
        let auth = resolve_auth("http", "127.0.0.1", &opts).unwrap();
        let mut sink: Vec<u8> = Vec::new();
        McpServerBuilder::announce_auth(&auth, "127.0.0.1", 9000, &mut sink);
        assert!(String::from_utf8(sink).unwrap().is_empty());
    }

    #[test]
    fn test_generated_token_notice_stays_quiet_on_loopback() {
        // A local dev server is the case the generated token exists to make
        // painless; its traffic reaches no network interface.
        let notice = McpServerBuilder::generated_token_notice("deadbeef", "127.0.0.1", 9000);
        assert!(!notice.contains("cleartext"), "{notice}");
        assert!(
            notice.contains("Authorization: Bearer deadbeef"),
            "{notice}"
        );
    }
}
