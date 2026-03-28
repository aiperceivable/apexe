use std::sync::Arc;

use apcore::middleware::logging::LoggingMiddleware;
use apcore::registry::registry::ModuleDescriptor;
use apcore::{Config, ErrorCode, Executor, ModuleError, Registry};
use apcore_mcp::{APCoreMCP, BackendSource, ElicitationApprovalHandler};
use apcore_toolkit::ScannedModule;
use serde_json::Value;

use crate::module::CliModule;
use crate::output::load_modules_from_dir;

/// Builder for creating an MCP server from apexe's scanned CLI modules.
///
/// Loads `.binding.yaml` files from a modules directory, wraps each as a
/// [`CliModule`], registers them into an apcore [`Registry`], and hands the
/// resulting [`Executor`] to apcore-mcp's [`APCoreMCP`] server.
pub struct McpServerBuilder {
    name: String,
    transport: String,
    host: String,
    port: u16,
    explorer: bool,
    require_auth: bool,
    validate_inputs: bool,
    modules_dir: Option<std::path::PathBuf>,
    timeout_ms: u64,
    /// Filter exposed tools by tags (AND logic).
    tags: Option<Vec<String>>,
    /// Filter exposed tools by module ID prefix.
    prefix: Option<String>,
    /// Path to ACL YAML file for access control.
    acl_path: Option<std::path::PathBuf>,
    /// Enable LoggingMiddleware for structured execution logging.
    enable_logging: bool,
    /// Enable ElicitationApprovalHandler for destructive command approval.
    enable_approval: bool,
}

impl McpServerBuilder {
    /// Create a new builder with sensible defaults.
    pub fn new() -> Self {
        Self {
            name: "apexe".to_string(),
            transport: "stdio".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8000,
            explorer: false,
            require_auth: false,
            validate_inputs: true,
            modules_dir: None,
            timeout_ms: 30_000,
            tags: None,
            prefix: None,
            acl_path: None,
            enable_logging: true,
            enable_approval: false,
        }
    }

    /// Set the MCP server name.
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
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

    /// Enable or disable authentication requirement.
    pub fn require_auth(mut self, required: bool) -> Self {
        self.require_auth = required;
        self
    }

    /// Enable or disable input validation against tool schemas.
    pub fn validate_inputs(mut self, enabled: bool) -> Self {
        self.validate_inputs = enabled;
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

    /// Load modules from binding files, register them, and build the MCP server.
    ///
    /// Returns the configured [`APCoreMCP`] instance ready to call `serve()`.
    // ModuleError is the crate-wide domain error; boxing it would diverge from
    // the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    pub fn build(self) -> Result<APCoreMCP, ModuleError> {
        let modules = self.load_scanned_modules()?;

        let mut registry = Registry::new();
        self.register_modules(&modules, &mut registry);
        tracing::info!(count = registry.count(), "Registered CLI modules");

        let config = Config::default();
        let executor = Executor::new(registry, config);
        let executor = self.configure_executor(executor);

        let transport = self.resolve_transport()?;

        self.build_mcp_server(executor, transport)
    }

    /// Load ScannedModules from the configured modules directory.
    #[allow(clippy::result_large_err)]
    fn load_scanned_modules(&self) -> Result<Vec<ScannedModule>, ModuleError> {
        if let Some(ref dir) = self.modules_dir {
            if dir.is_dir() {
                load_modules_from_dir(dir)
            } else {
                tracing::warn!(
                    dir = %dir.display(),
                    "Modules directory not found, starting with zero tools"
                );
                Ok(vec![])
            }
        } else {
            Ok(vec![])
        }
    }

    /// Apply middleware, ACL, and approval handler to the executor.
    fn configure_executor(&self, mut executor: Executor) -> Executor {
        if self.enable_logging {
            let logging = LoggingMiddleware::with_defaults();
            if let Err(e) = executor.use_middleware(Box::new(logging)) {
                tracing::warn!(error = %e, "Failed to add LoggingMiddleware");
            }
        }

        if let Some(ref acl_path) = self.acl_path {
            if acl_path.exists() {
                match crate::governance::AclManager::from_config(acl_path) {
                    Ok(acl_mgr) => executor.set_acl(acl_mgr.into_inner()),
                    Err(e) => tracing::warn!(error = %e, "Failed to load ACL"),
                }
            }
        }

        if self.enable_approval {
            let handler = ElicitationApprovalHandler::new(None);
            executor.set_approval_handler(Box::new(handler));
            tracing::info!("ElicitationApprovalHandler enabled for destructive commands");
        }

        executor
    }

    /// Map the user-facing transport name to the apcore-mcp transport string.
    #[allow(clippy::result_large_err)]
    fn resolve_transport(&self) -> Result<&'static str, ModuleError> {
        match self.transport.as_str() {
            "stdio" => Ok("stdio"),
            "http" => Ok("streamable-http"),
            "sse" => Ok("sse"),
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
        executor: Executor,
        transport: &str,
    ) -> Result<APCoreMCP, ModuleError> {
        let mut builder = APCoreMCP::builder()
            .backend(BackendSource::Executor(Arc::new(executor)))
            .name(&self.name)
            .transport(transport)
            .host(&self.host)
            .port(self.port)
            .validate_inputs(self.validate_inputs);

        if self.explorer {
            builder = builder.include_explorer(true);
        }
        if self.require_auth {
            builder = builder.require_auth(true);
        }
        if let Some(tags) = self.tags {
            builder = builder.tags(tags);
        }
        if let Some(ref prefix) = self.prefix {
            builder = builder.prefix(prefix);
        }

        builder.build().map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to build MCP server: {e}"),
            )
        })
    }
}

impl McpServerBuilder {
    /// Register ScannedModules into a Registry as CliModules.
    fn register_modules(&self, modules: &[ScannedModule], registry: &mut Registry) {
        for scanned in modules {
            match CliModule::from_scanned(scanned, self.timeout_ms) {
                Ok(cli_module) => {
                    let module_id = scanned.module_id.clone();
                    let annotations = scanned.annotations.clone().unwrap_or_default();
                    let descriptor = ModuleDescriptor {
                        name: module_id.clone(),
                        enabled: true,
                        tags: scanned.tags.clone(),
                        dependencies: vec![],
                        annotations,
                        input_schema: scanned.input_schema.clone(),
                        output_schema: scanned.output_schema.clone(),
                    };
                    if let Err(e) = registry.register(&module_id, Box::new(cli_module), descriptor)
                    {
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

    /// Export registered tools as OpenAI-compatible function calling definitions.
    ///
    /// Loads modules, registers them, and converts to OpenAI format without
    /// starting a server.
    #[allow(clippy::result_large_err)]
    pub fn export_openai_tools(self) -> Result<Vec<Value>, ModuleError> {
        let modules = self.load_scanned_modules()?;

        let mut registry = Registry::new();
        self.register_modules(&modules, &mut registry);

        let config = Config::default();
        let executor = Executor::new(registry, config);

        let openai_config = apcore_mcp::OpenAIToolsConfig {
            embed_annotations: true,
            strict: false,
            tags: self.tags.clone(),
            prefix: self.prefix.clone(),
        };

        apcore_mcp::to_openai_tools(BackendSource::Executor(Arc::new(executor)), openai_config)
            .map_err(|e| {
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
        assert!(!builder.require_auth);
        assert!(builder.validate_inputs);
        assert!(builder.modules_dir.is_none());
        assert_eq!(builder.timeout_ms, 30_000);
    }

    #[test]
    fn test_mcp_server_builder_chain() {
        let builder = McpServerBuilder::new()
            .name("my-server")
            .transport("http")
            .host("0.0.0.0")
            .port(9090)
            .explorer(true)
            .require_auth(true)
            .validate_inputs(false)
            .modules_dir("/tmp/modules")
            .timeout_ms(60_000);

        assert_eq!(builder.name, "my-server");
        assert_eq!(builder.transport, "http");
        assert_eq!(builder.host, "0.0.0.0");
        assert_eq!(builder.port, 9090);
        assert!(builder.explorer);
        assert!(builder.require_auth);
        assert!(!builder.validate_inputs);
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
    fn test_mcp_server_builder_with_logging_and_approval() {
        let result = McpServerBuilder::new()
            .enable_logging(true)
            .enable_approval(true)
            .build();
        assert!(result.is_ok());
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
}
