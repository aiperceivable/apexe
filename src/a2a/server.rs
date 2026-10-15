use std::sync::Arc;

use apcore::{ErrorCode, Executor, ModuleError};
use apcore_a2a::{APCoreA2AConfig, BackendSource};
use apcore_mcp::ApprovalStore;

use crate::module::{build_executor, ExecutorOptions};

/// Builder for serving CLI modules and imported OpenAPI operations through A2A.
///
/// Shares [`build_executor`](crate::module::build_executor) with
/// [`crate::mcp::McpServerBuilder`], so an ACL policy, the logging
/// middleware, and the approval handler apply identically whether a module
/// is served over MCP or A2A.
/// [`Self::prepare_with_openapi`] also registers HTTP operations in that executor.
pub struct A2aServerBuilder {
    auth_options: Option<crate::auth::AuthOptions>,
    name: String,
    url: String,
    explorer: bool,
    bindings_dir: Option<std::path::PathBuf>,
    bindings_pattern: Option<String>,
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
    /// Acknowledge a non-loopback bind only when authentication is disabled.
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

/// Refuse an explicitly unauthenticated non-loopback bind without acknowledgement.
/// Authenticated CLI configurations use `validate_endpoint` instead.
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
                 this A2A configuration has no transport authentication — the agent \
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
            auth_options: None,
            name: "apexe".to_string(),
            url: DEFAULT_A2A_URL.to_string(),
            explorer: false,
            bindings_dir: None,
            bindings_pattern: None,
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

    /// Configure token, JWT or explicitly unauthenticated A2A transport.
    pub fn auth(mut self, options: crate::auth::AuthOptions) -> Self {
        self.allow_unauthenticated_bind = options.allow_unauthenticated_bind;
        self.auth_options = Some(options);
        self
    }

    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    pub fn transport_auth(
        &self,
    ) -> Result<Option<Arc<dyn apcore_a2a::Authenticator>>, ModuleError> {
        match &self.auth_options {
            Some(options) => super::auth::resolve(&parse_bind_url(&self.url)?.host, options),
            None => Ok(None),
        }
    }

    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    fn validate_endpoint(&self) -> Result<BindAddress, ModuleError> {
        if self
            .auth_options
            .as_ref()
            .is_some_and(|options| options.mode != Some(crate::auth::AuthMode::None))
        {
            parse_bind_url(&self.url)
        } else {
            validate_bind_url(&self.url, self.allow_unauthenticated_bind)
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
    /// Select binding files by a filename glob instead of the default.
    ///
    /// Normally left unset: `None` reaches apcore-toolkit's loader, which
    /// applies apcore's canonical `bindings.pattern` default itself. Set this
    /// only to honour a configuration that *declares* the key -- see
    /// `ApexeConfig::bindings_pattern`.
    #[must_use]
    pub fn bindings_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.bindings_pattern = Some(pattern.into());
        self
    }

    pub fn bindings_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.bindings_dir = Some(dir.into());
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
    /// Runs the exact `serve` assembly — ACL, audit sink and middleware stack
    /// — without binding a port, so a caller (or a test) can drive a real
    /// `Executor::call` through the same governance a live A2A request meets.
    /// The in-process counterpart of [`agent_card`](Self::agent_card), which
    /// exercises the same assembly from the discovery side.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    pub fn executor(&self) -> Result<Arc<Executor>, ModuleError> {
        self.prepare().map(|(executor, _config)| executor)
    }

    /// The `APCoreA2AConfig` this builder will hand to apcore-a2a.
    ///
    /// The executor is only half the assembly: since apcore-a2a 0.6 the
    /// refusal-disclosure policy lives on the *config*, so a test that builds
    /// its own config exercises a server apexe would never serve. Exposed for
    /// the same reason [`executor`](Self::executor) is — to pin the wiring
    /// without binding a port.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    pub fn config(&self) -> Result<APCoreA2AConfig, ModuleError> {
        self.prepare().map(|(_executor, config)| config)
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
    /// Excluded modules are absent from the agent card and uncallable because
    /// the filter runs at registration rather than over the card.
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
            bindings_dir: self.bindings_dir.as_deref(),
            bindings_pattern: self.bindings_pattern.as_deref(),
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
        }
    }

    /// Load modules, register them, and serve as an A2A agent until the
    /// server stops or errors. Must be driven from a Tokio runtime.
    // ModuleError is the crate-wide domain error; boxing it would diverge from
    // the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub async fn serve(self) -> Result<(), ModuleError> {
        let (router, _) = self.app(None).await?;
        let listener = self.listener().await?;
        Self::serve_router(listener, router).await
    }

    /// Import OpenAPI operations alongside CLI bindings and serve A2A.
    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    pub async fn serve_with_openapi(
        self,
        openapi: &crate::openapi::OpenApiOptions,
    ) -> Result<(), ModuleError> {
        let (router, _) = self.app(Some(openapi)).await?;
        let listener = self.listener().await?;
        Self::serve_router(listener, router).await
    }

    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    async fn listener(&self) -> Result<tokio::net::TcpListener, ModuleError> {
        let bind = self.validate_endpoint()?;
        tokio::net::TcpListener::bind((bind.host.as_str(), bind.port))
            .await
            .map_err(|error| {
                ModuleError::new(
                    ErrorCode::GeneralInternalError,
                    format!("A2A bind failed: {error}"),
                )
            })
    }

    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    async fn serve_router(
        listener: tokio::net::TcpListener,
        router: axum::Router,
    ) -> Result<(), ModuleError> {
        axum::serve(listener, router).await.map_err(|error| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("A2A server failed: {error}"),
            )
        })
    }

    /// Assemble the exact router used by both A2A serving paths.
    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    pub async fn app(
        &self,
        openapi: Option<&crate::openapi::OpenApiOptions>,
    ) -> Result<(axum::Router, serde_json::Value), ModuleError> {
        let (executor, config) = match openapi {
            Some(options) => self.prepare_with_openapi(options).await?,
            None => self.prepare()?,
        };
        self.assemble_app(executor, config).await
    }

    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    async fn assemble_app(
        &self,
        executor: Arc<Executor>,
        config: APCoreA2AConfig,
    ) -> Result<(axum::Router, serde_json::Value), ModuleError> {
        let logging = executor.find_middleware("logging");
        // Avoid duplicate-name warnings while the SDK installs its own logger.
        if logging.is_some() {
            executor.remove_middleware("logging");
        }
        let (router, card) = apcore_a2a::build_app_with_auth(
            BackendSource::Executor(executor.clone()),
            config,
            self.transport_auth()?,
        )
        .await
        .map_err(|error| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("A2A assembly failed: {error}"),
            )
        })?;
        Self::restore_host_logging(&executor, logging)?;
        let router = router.layer(axum::middleware::from_fn(
            super::notifications::suppress_reply,
        ));
        let card = serde_json::to_value(card).map_err(|error| {
            ModuleError::new(ErrorCode::GeneralInternalError, error.to_string())
        })?;
        Ok((router, card))
    }

    /// Keep the host's payload policy instead of the SDK's automatic logger.
    #[allow(clippy::result_large_err)] // Preserve the shared ModuleError boundary.
    fn restore_host_logging(
        executor: &Executor,
        logging: Option<Arc<dyn apcore::middleware::Middleware>>,
    ) -> Result<(), ModuleError> {
        // Both loggers use "logging". Clear the SDK registrations, then restore
        // the original instance with its priority and configured payload flags.
        // This executor is newly prepared by apexe and has no active calls.
        while executor.remove_middleware("logging") {}
        if let Some(logging) = logging {
            executor.use_middleware_shared(logging)?;
        }
        Ok(())
    }

    /// Assemble the same governed HTTP and CLI surface without binding a port.
    #[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
    pub async fn prepare_with_openapi(
        &self,
        openapi: &crate::openapi::OpenApiOptions,
    ) -> Result<(Arc<Executor>, APCoreA2AConfig), ModuleError> {
        self.validate_endpoint()?;
        let executor =
            crate::module::build_executor_with_openapi(&self.executor_options(), openapi).await?;
        self.finish_prepare(executor)
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
        self.app(None).await.map(|(_, card)| card)
    }

    /// Shared assembly for `serve` and `agent_card`: enforce the approval
    /// precondition, build the governed executor, and construct the A2A config.
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    fn prepare(&self) -> Result<(Arc<Executor>, APCoreA2AConfig), ModuleError> {
        self.validate_endpoint()?;
        let executor = build_executor(&self.executor_options())?;
        self.finish_prepare(executor)
    }

    #[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
    fn finish_prepare(
        &self,
        executor: Arc<Executor>,
    ) -> Result<(Arc<Executor>, APCoreA2AConfig), ModuleError> {
        // Before anything else: a bad `--url` is the one failure that would
        // otherwise succeed loudly on the wrong socket.
        self.validate_endpoint()?;

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

        let config = APCoreA2AConfig {
            name: self.name.clone(),
            description: format!("apexe A2A agent '{}'", self.name),
            // The agent card's `version` describes THIS agent, not the
            // framework serving it. The default is apcore-a2a's package
            // version, so override it with apexe's package version.
            version: crate::VERSION.to_string(),
            url: self.url.clone(),
            execution_timeout: self.execution_timeout,
            explorer: self.explorer,
            sys_modules: false,
            cors_origins: self.cors_origins.clone(),
            // Authenticated transports mask detailed reasons across principals.
            // Explicit unauthenticated surfaces retain actionable refusal details.
            disclose_refusal_reason: self
                .auth_options
                .as_ref()
                .is_none_or(|options| options.mode == Some(crate::auth::AuthMode::None)),
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
        assert!(builder.bindings_dir.is_none());
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
            .bindings_dir("/tmp/modules")
            .timeout_ms(60_000)
            .execution_timeout(600)
            .cors_origins(vec!["https://example.com".to_string()]);

        assert_eq!(builder.name, "my-agent");
        assert_eq!(builder.url, "http://0.0.0.0:9090");
        assert!(builder.explorer);
        assert_eq!(
            builder.bindings_dir,
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
    async fn test_a2a_app_preserves_host_logging_configuration() {
        use apcore_toolkit::ScannedModule;
        use serde_json::json;
        let directory = tempfile::TempDir::new().unwrap();
        let module = ScannedModule::new(
            "cli.echo".into(),
            "Echo".into(),
            json!({"type":"object"}),
            json!({"type":"object"}),
            vec![],
            "exec:///bin/echo".into(),
        );
        crate::output::YamlOutput::without_verification()
            .write(&[module], directory.path(), false)
            .unwrap();
        for (logging, arguments) in [(true, true), (true, false), (false, true), (false, false)] {
            let builder = A2aServerBuilder::new()
                .bindings_dir(directory.path())
                .enable_logging(logging)
                .log_arguments(arguments);
            let (executor, config) = builder.prepare().unwrap();
            let mut before = executor.middlewares();
            let original = executor.find_middleware("logging");
            let _app = builder
                .assemble_app(executor.clone(), config)
                .await
                .unwrap();
            let mut after = executor.middlewares();
            before.sort();
            after.sort();
            assert_eq!(
                after, before,
                "A2A assembly changed the configured logging pipeline"
            );
            match (original, executor.find_middleware("logging")) {
                (Some(original), Some(current)) => assert!(Arc::ptr_eq(&original, &current)),
                (None, None) => {}
                _ => panic!("A2A assembly changed the configured logging instance"),
            }
        }
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

        // There used to be an `assert_ne!` against
        // `APCoreA2AConfig::default().version` here, on the reasoning that the
        // card must not carry the framework's constant. It was removed when
        // apexe 0.7.0 met apcore-a2a 0.7.0: two independently versioned crates
        // are free to collide, and on that day the guard failed while the
        // behaviour it guards was correct. It could not have caught the
        // regression then either -- if `prepare` had forgotten to set
        // `config.version`, the fallback would have been "0.7.0" and the
        // assertion would have passed. The equality assertions above are the
        // load-bearing ones: they pin the value to apexe's own version
        // whatever the framework's happens to be.
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
