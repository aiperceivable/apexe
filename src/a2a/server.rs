use std::sync::Arc;

use apcore::{ErrorCode, Executor, ModuleError};
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
    /// Include call arguments and output in the structured log. See
    /// [`ExecutorOptions::log_arguments`](crate::module::ExecutorOptions::log_arguments).
    log_arguments: bool,
    /// Gate calls to modules annotated `requires_approval` on a human decision. See
    /// [`ApprovalGate`](crate::module::ApprovalGate).
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
    /// Restrict the served skills to a subset of the scanned modules.
    ///
    /// Enforced at registration, exactly as on the MCP side: an excluded
    /// module does not exist on the server, so it is absent from the agent
    /// card *and* uncallable. See [`ModuleFilter`](crate::module::ModuleFilter).
    filter: crate::module::ModuleFilter,
    /// Acknowledge binding A2A to a non-loopback address. A2A has no
    /// authenticator at all, so this is the `apexe serve --auth none`
    /// configuration with no way to opt back in — see [`validate_bind_url`].
    allow_unauthenticated_bind: bool,
}

/// The address `apcore_a2a::async_serve` will actually listen on.
#[derive(Debug, PartialEq, Eq)]
struct BindAddress {
    host: String,
    port: u16,
}

/// The default `--url`, and the only value that needs no acknowledgement.
const DEFAULT_A2A_URL: &str = "http://127.0.0.1:8000";

/// Reject a `--url` value, saying what is wrong and what to give instead.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn refuse_bind_url(url: &str, detail: &str) -> ModuleError {
    ModuleError::new(
        ErrorCode::GeneralInvalidInput,
        format!(
            "Refusing to start: --url '{url}' {detail} Give a full \
             `http://<host>:<port>` with no path, for example `{DEFAULT_A2A_URL}`."
        ),
    )
}

/// Strip the scheme, refusing anything that is not a bare `http://` authority.
///
/// The A2A server derives its listen address by splitting this value on `://`
/// and passing everything after it verbatim to the socket bind, which is why a
/// missing scheme and a trailing path are both fatal rather than cosmetic.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn http_authority(url: &str) -> Result<&str, ModuleError> {
    let Some((scheme, authority)) = url.split_once("://") else {
        return Err(refuse_bind_url(
            url,
            "has no scheme, and the A2A server derives its listen address by \
             splitting on '://' — with no scheme it falls back to 0.0.0.0:8000 and \
             serves every wrapped binary on every interface, unauthenticated.",
        ));
    };
    if scheme != "http" {
        return Err(refuse_bind_url(
            url,
            &format!(
                "uses the '{scheme}' scheme, but the A2A server both serves plain HTTP \
                 and derives its listen address from this value, so '{scheme}' describes \
                 neither the listener nor a reachable endpoint."
            ),
        ));
    }
    if authority.contains(['/', '?', '#']) {
        return Err(refuse_bind_url(
            url,
            "carries a path, query or fragment. Everything after '://' is passed \
             verbatim to the socket bind, so it would fail to start.",
        ));
    }
    Ok(authority)
}

/// Split a validated authority into the address the server binds.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn bind_address(url: &str, authority: &str) -> Result<BindAddress, ModuleError> {
    let (host, port) = split_host_port(authority).ok_or_else(|| {
        refuse_bind_url(
            url,
            "names no port. The whole authority is passed verbatim to the socket \
             bind, which needs an explicit `host:port`.",
        )
    })?;
    if host.is_empty() {
        return Err(refuse_bind_url(url, "names no host."));
    }
    let port: u16 = port
        .parse()
        .map_err(|_| refuse_bind_url(url, &format!("has '{port}' where a port number belongs.")))?;
    if port == 0 {
        return Err(refuse_bind_url(
            url,
            "asks for port 0, which binds an arbitrary port the agent card \
             would then misreport.",
        ));
    }
    Ok(BindAddress {
        host: host.to_string(),
        port,
    })
}

/// Parse `--url` into the address apcore-a2a will bind, refusing anything whose
/// listener would not be the one the operator described.
///
/// apcore-a2a derives its socket as `url.split("://").nth(1)` with a fallback of
/// `"0.0.0.0:8000"`, and hands the result straight to `TcpListener::bind`. Three
/// consequences, all silent:
///
/// * A scheme-less value takes the fallback. `apexe a2a --url 127.0.0.1:18999`
///   listens on **every interface, on port 8000**, and serves the full agent
///   card to any host. An operator typing a loopback address got the exact
///   opposite of what they asked for.
/// * A trailing path or slash is passed through to `bind`, which fails with an
///   address-parse error that names neither the URL nor the slash.
/// * A missing port does the same.
///
/// None of that can be fixed downstream, so it is refused here. The `https`
/// case is refused for the same reason rather than a different one: apcore-a2a
/// serves plain HTTP and derives the bind address from this same field, so a
/// TLS URL describes neither the listener nor a reachable endpoint.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn parse_bind_url(url: &str) -> Result<BindAddress, ModuleError> {
    bind_address(url, http_authority(url)?)
}

/// Split an authority into host and port, keeping a bracketed IPv6 literal
/// whole. Returns `None` when no port is present.
fn split_host_port(authority: &str) -> Option<(&str, &str)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        return after.strip_prefix(':').map(|port| (host, port));
    }
    let (host, port) = authority.rsplit_once(':')?;
    // An unbracketed IPv6 literal has several colons; `rsplit_once` would take
    // the last hextet for a port. Refuse rather than guess.
    if host.contains(':') {
        return None;
    }
    Some((host, port))
}

/// Refuse a non-loopback bind unless it was explicitly acknowledged.
///
/// `apexe serve` already applies this to `--auth none`, on the grounds that
/// apexe wraps arbitrary local binaries and an unauthenticated non-loopback
/// bind is a remote-execution entry point rather than an open API. A2A is that
/// configuration with no way out of it: apcore-a2a has no `Authenticator`, so
/// there is no `--auth token` to reach for, and an unauthenticated
/// `GET /.well-known/agent-card.json` returns the full skill list with
/// `securitySchemes: {}` while an unauthenticated `POST /` reaches the JSON-RPC
/// handler. Applying the weaker rule to the weaker transport would be backwards,
/// so the same acknowledgement flag governs both.
#[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
fn validate_bind_url(url: &str, acknowledged: bool) -> Result<BindAddress, ModuleError> {
    let bind = parse_bind_url(url)?;
    if crate::auth::is_loopback_host(&bind.host) {
        return Ok(bind);
    }
    if !acknowledged {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Refusing to start: --url '{url}' binds the non-loopback host '{}', and \
                 the A2A server has no transport authentication of any kind — the agent \
                 card and every wrapped binary would be reachable from the network with \
                 no credential. Bind to loopback (`{DEFAULT_A2A_URL}`) and put a \
                 reverse proxy that authenticates in front, or pass \
                 `--allow-unauthenticated-bind` to state that you mean it.",
                bind.host
            ),
        ));
    }
    tracing::warn!(
        host = %bind.host,
        port = bind.port,
        "Serving A2A with NO authentication on a non-loopback bind, as explicitly acknowledged"
    );
    Ok(bind)
}

impl A2aServerBuilder {
    /// Create a new builder with sensible defaults.
    pub fn new() -> Self {
        Self {
            name: "apexe".to_string(),
            url: DEFAULT_A2A_URL.to_string(),
            explorer: false,
            modules_dir: None,
            timeout_ms: 30_000,
            acl_path: None,
            filter: crate::module::ModuleFilter::default(),
            audit_path: None,
            enable_logging: true,
            log_arguments: true,
            enable_approval: false,
            enable_circuit_breaker: true,
            enable_retry: true,
            approval_store: None,
            execution_timeout: 300,
            cors_origins: vec![],
            allow_unauthenticated_bind: false,
        }
    }

    /// Set the A2A agent name.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set the base URL to bind the A2A server to.
    ///
    /// Must be a full `http://<host>:<port>` with no path — see
    /// [`validate_bind_url`] for why a scheme-less or path-bearing value is
    /// refused rather than normalized. A non-loopback host additionally needs
    /// [`Self::allow_unauthenticated_bind`]. Both are checked when the server
    /// is prepared, not here, so the builder stays infallible.
    pub fn url(mut self, url: &str) -> Self {
        self.url = url.to_string();
        self
    }

    /// Acknowledge binding to a non-loopback address despite A2A having no
    /// transport authentication.
    pub fn allow_unauthenticated_bind(mut self, acknowledged: bool) -> Self {
        self.allow_unauthenticated_bind = acknowledged;
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

    /// The governed `Executor` this builder will hand to apcore-a2a.
    ///
    /// Runs the exact `serve` assembly — ACL, audit sink, middleware stack and
    /// the [`DenialReasonRelay`](crate::module::DenialReasonRelay) — without
    /// binding a port, so a caller (or a test) can drive a real
    /// `Executor::call` through the same governance a live A2A request meets.
    /// The in-process counterpart of [`agent_card`](Self::agent_card), which
    /// exercises the same assembly from the discovery side.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    pub fn executor(&self) -> Result<Arc<Executor>, ModuleError> {
        self.prepare().map(|(executor, _config)| executor)
    }

    /// The surface filter this builder will register modules through.
    ///
    /// Exposed so a test can pin the CLI-to-builder wiring without binding a
    /// port; the filter itself is enforced at registration.
    pub fn module_filter(&self) -> &crate::module::ModuleFilter {
        &self.filter
    }

    /// Serve only modules whose `module_id` starts with `prefix`.
    ///
    /// Excluded modules are absent from the agent card and uncallable, because
    /// the filter runs at registration rather than over the card. On A2A that
    /// distinction carries more weight than on MCP: `apexe a2a` has no
    /// authenticator, so narrowing the surface is the only mechanism available
    /// for limiting what an unauthenticated caller can reach.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.filter.prefix = Some(prefix.into());
        self
    }

    /// Serve only modules carrying every tag in `tags`.
    ///
    /// See [`prefix`](Self::prefix) for why this is enforced at registration.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.filter.tags = Some(tags);
        self
    }

    /// Options shared with the MCP builder for assembling a governed `Executor`.
    fn executor_options(&self) -> ExecutorOptions<'_> {
        ExecutorOptions {
            modules_dir: self.modules_dir.as_deref(),
            timeout_ms: self.timeout_ms,
            acl_path: self.acl_path.as_deref(),
            filter: self.filter.clone(),
            audit_path: self.audit_path.as_deref(),
            enable_logging: self.enable_logging,
            log_arguments: self.log_arguments,
            enable_approval: self.enable_approval,
            enable_circuit_breaker: self.enable_circuit_breaker,
            enable_retry: self.enable_retry,
            approval_store: self.approval_store.clone(),
            // apcore-a2a reports an ACL denial as `Task not found` and an
            // approval denial as `Internal server error`, both of which send an
            // agent off to retry a call that will never succeed. See
            // [`DenialReasonRelay`](crate::module::DenialReasonRelay).
            relay_denial_reason: true,
        }
    }

    /// Load modules, register them, and serve as an A2A agent until the
    /// server stops or errors. Must be driven from a Tokio runtime.
    // ModuleError is the crate-wide domain error; boxing it would diverge from
    // the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub async fn serve(self) -> Result<(), ModuleError> {
        let (executor, config) = self.prepare()?;

        apcore_a2a::async_serve(BackendSource::Executor(executor), config)
            .await
            .map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("A2A server error: {e}"),
                )
            })
    }

    /// Build the A2A agent card without binding a port.
    ///
    /// Assembles the same governed `Executor` and config `serve` would, then
    /// asks apcore-a2a to build the app in-process and returns the resulting
    /// agent card as JSON. Symmetric with
    /// [`McpServerBuilder::export_openai_tools`](crate::mcp::McpServerBuilder::export_openai_tools):
    /// an embedding/test affordance that exercises the serve assembly without a
    /// live server. Returns `Err` when no modules are present (empty registry).
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    pub async fn agent_card(self) -> Result<serde_json::Value, ModuleError> {
        let (executor, config) = self.prepare()?;
        let (_router, card) = apcore_a2a::build_app(BackendSource::Executor(executor), config)
            .await
            .map_err(|e| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("Failed to build A2A app: {e}"),
                )
            })?;
        serde_json::to_value(&card).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to serialize agent card: {e}"),
            )
        })
    }

    /// Shared assembly for `serve` and `agent_card`: enforce the approval
    /// precondition, build the governed executor, and construct the A2A config.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    fn prepare(&self) -> Result<(Arc<Executor>, APCoreA2AConfig), ModuleError> {
        // Before anything else: a bad `--url` is the one failure that would
        // otherwise succeed loudly on the wrong socket.
        validate_bind_url(&self.url, self.allow_unauthenticated_bind)?;

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
            // The agent card's `version` describes THIS agent, not the
            // framework serving it. `APCoreA2AConfig::default()` fills it from
            // `apcore_a2a::VERSION`, which is a hard-coded constant in that
            // crate and is stale besides ("0.4.1" on apcore-a2a 0.4.4), so a
            // client reading the card learned neither which apexe nor which
            // apcore-a2a it was talking to. Set it explicitly.
            version: crate::VERSION.to_string(),
            url: self.url.clone(),
            execution_timeout: self.execution_timeout,
            explorer: self.explorer,
            sys_modules: false,
            cors_origins: self.cors_origins.clone(),
            ..APCoreA2AConfig::default()
        };

        Ok((executor, config))
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

    #[test]
    fn test_a2a_prepare_sets_apexe_version_on_the_agent_card() {
        // #35 item 4: the card reported apcore-a2a's `VERSION` constant
        // ("0.4.1", itself stale on apcore-a2a 0.4.4) because `config.version`
        // was never set. `prepare` must stamp apexe's own version.
        let builder = A2aServerBuilder::new();
        let (_executor, config) = builder.prepare().expect("prepare should succeed");
        assert_eq!(config.version, crate::VERSION);
        assert_eq!(config.version, env!("CARGO_PKG_VERSION"));
        assert_ne!(
            config.version,
            APCoreA2AConfig::default().version,
            "the card must not report the framework's version as its own"
        );
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
