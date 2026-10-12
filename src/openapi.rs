use std::collections::HashMap;

use apcore::{ErrorCode, ModuleError, Registry};
use apcore_toolkit::{HTTPProxyRegistryWriter, OpenAPIScanner, ScanOptions, ScannedModule};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// An OpenAPI 3.0/3.1 source to register alongside scanned CLI modules.
#[derive(Clone, Serialize, Deserialize)]
pub struct OpenApiOptions {
    pub source: String,
    pub base_url: Option<String>,
    pub prefix: String,
    /// Sent only when fetching the specification.
    pub spec_headers: HashMap<String, String>,
    /// Sent only with calls to the described API. Never persisted to bindings.
    pub bearer_token: Option<String>,
}

impl std::fmt::Debug for OpenApiOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenApiOptions")
            .field("prefix", &self.prefix)
            .field("credentials", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Shared OpenAPI source flags for MCP and A2A.
#[derive(Clone, Default, clap::Args, Serialize, Deserialize)]
pub struct OpenApiArgs {
    /// OpenAPI 3.0/3.1 JSON or YAML file, or an HTTP(S) specification URL.
    #[arg(long = "openapi")]
    pub source: Option<String>,
    /// API origin override when the document has no absolute servers URL.
    #[arg(long, requires = "source")]
    pub openapi_base_url: Option<String>,
    /// Namespace for imported operations (default: api).
    #[arg(long, requires = "source")]
    pub openapi_prefix: Option<String>,
    /// Specification-fetch header (repeatable); never sent with API calls.
    #[arg(long, requires = "source", hide_env_values = true)]
    pub openapi_header: Vec<String>,
    /// API bearer token; separate from specification and MCP credentials.
    #[arg(long, env = "APEXE_OPENAPI_TOKEN", hide_env_values = true)]
    pub openapi_token: Option<String>,
}

impl std::fmt::Debug for OpenApiArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenApiArgs")
            .field("prefix", &self.openapi_prefix)
            .field("credentials", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl OpenApiArgs {
    pub(crate) fn options(&self) -> anyhow::Result<Option<OpenApiOptions>> {
        let Some(source) = &self.source else {
            return Ok(None);
        };
        anyhow::ensure!(!source.trim().is_empty(), "--openapi must not be empty");
        let prefix = self.openapi_prefix.as_deref().unwrap_or("api");
        anyhow::ensure!(
            !prefix.is_empty()
                && prefix
                    .split('.')
                    .all(apcore_mcp::openapi_backend::is_legal_segment),
            "--openapi-prefix must be a legal apcore namespace"
        );
        let mut headers = HashMap::new();
        for header in &self.openapi_header {
            let (name, value) = header
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("--openapi-header must use 'Name: value'"))?;
            anyhow::ensure!(
                !name.trim().is_empty(),
                "--openapi-header must name a header"
            );
            headers.insert(name.trim().into(), value.trim().into());
        }
        Ok(Some(OpenApiOptions {
            source: source.clone(),
            base_url: self.openapi_base_url.clone(),
            prefix: prefix.into(),
            spec_headers: headers,
            bearer_token: self.openapi_token.clone(),
        }))
    }
}

fn import_error(message: impl Into<String>) -> ModuleError {
    ModuleError::new(ErrorCode::GeneralInvalidInput, message)
}

#[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
pub(crate) async fn register_operations(
    registry: &Registry,
    opts: &crate::module::ExecutorOptions<'_>,
    source: &OpenApiOptions,
    audit: Option<Arc<crate::governance::AuditManager>>,
) -> Result<(), ModuleError> {
    let timeout = std::time::Duration::from_millis(opts.timeout_ms).as_secs_f64();
    if source.prefix.is_empty()
        || !source
            .prefix
            .split('.')
            .all(apcore_mcp::openapi_backend::is_legal_segment)
    {
        return Err(import_error(
            "OpenAPI prefix must be a legal apcore namespace",
        ));
    }
    let document = load_document(source, timeout).await?;
    let mut scan_options = ScanOptions::new();
    scan_options.base_path_prefix = Some(source.prefix.clone());
    let modules = OpenAPIScanner::new()
        .scan(&document, &scan_options)
        .await
        .map_err(|error| import_error(format!("Failed to scan OpenAPI: {error}")))?;
    let modules: Vec<ScannedModule> = modules
        .into_iter()
        .filter(|module| opts.filter.admits(module))
        .map(complete_annotations)
        .collect();
    validate_import(registry, &document, &modules)?;
    let writer = build_proxy_writer(source, &document, timeout)?;
    for result in writer.write(&modules, registry) {
        if let Some(error) = result.verification_error {
            return Err(import_error(format!(
                "OpenAPI module '{}' failed to register: {error}",
                result.module_id
            )));
        }
    }
    for module in &modules {
        audit_proxy(registry, module, audit.clone())?;
    }
    tracing::info!(count = modules.len(), prefix = %source.prefix, "Registered OpenAPI operations");
    Ok(())
}

#[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
async fn load_document(
    source: &OpenApiOptions,
    timeout: f64,
) -> Result<serde_json::Value, ModuleError> {
    let options = apcore_toolkit::LoadSpecOptions {
        headers: Some(source.spec_headers.clone()),
        auth_header_factory: None,
        timeout_secs: timeout,
    };
    apcore_toolkit::load_spec_with_options(&source.source, &options)
        .await
        .map_err(|error| import_error(format!("Failed to load OpenAPI: {error}")))
}

#[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
fn build_proxy_writer(
    source: &OpenApiOptions,
    document: &serde_json::Value,
    timeout: f64,
) -> Result<HTTPProxyRegistryWriter, ModuleError> {
    let base_url = source
        .base_url
        .clone()
        .or_else(|| {
            document
                .pointer("/servers/0/url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| {
            import_error("OpenAPI needs an absolute servers[0].url or --openapi-base-url")
        })?;
    let token = source.bearer_token.clone();
    let auth_factory: Option<Box<dyn Fn() -> HashMap<String, String> + Send + Sync>> =
        token.map(|token| {
            Box::new(move || HashMap::from([("Authorization".into(), format!("Bearer {token}"))]))
                as _
        });
    HTTPProxyRegistryWriter::new(base_url, auth_factory, timeout)
        .map_err(|error| import_error(error.to_string()))
}

#[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
fn validate_import(
    registry: &Registry,
    document: &serde_json::Value,
    modules: &[ScannedModule],
) -> Result<(), ModuleError> {
    for module in modules {
        if registry.has(&module.module_id) {
            return Err(import_error(format!(
                "OpenAPI module ID '{}' collides with a CLI module; change --openapi-prefix",
                module.module_id
            )));
        }
    }
    let hazards = apcore_cli::detect_proxy_hazards(document, modules);
    if let Some(hazard) = hazards.first() {
        return Err(import_error(format!("OpenAPI operation '{}' has query parameters on a body method; the HTTP proxy cannot encode this operation correctly", hazard.module_id)));
    }
    Ok(())
}

#[allow(clippy::result_large_err)] // Match the crate-wide ModuleError boundary.
fn audit_proxy(
    registry: &Registry,
    scanned: &ScannedModule,
    audit: Option<Arc<crate::governance::AuditManager>>,
) -> Result<(), ModuleError> {
    let id = &scanned.module_id;
    let module = registry
        .get(id)?
        .ok_or_else(|| import_error(format!("Imported module missing: {id}")))?;
    let descriptor = registry
        .get_definition(id)?
        .ok_or_else(|| import_error(format!("Imported descriptor missing: {id}")))?;
    registry.unregister(id)?;
    registry.register(
        id,
        Box::new(AuditedProxy {
            module,
            id: id.clone(),
            audit,
            annotations: scanned.annotations.clone().unwrap_or_default(),
        }),
        descriptor,
    )
}

struct AuditedProxy {
    module: Arc<dyn apcore::Module>,
    id: String,
    audit: Option<Arc<crate::governance::AuditManager>>,
    annotations: apcore::ModuleAnnotations,
}

#[async_trait::async_trait]
impl apcore::Module for AuditedProxy {
    fn input_schema(&self) -> serde_json::Value {
        self.module.input_schema()
    }
    fn output_schema(&self) -> serde_json::Value {
        self.module.output_schema()
    }
    fn description(&self) -> &str {
        self.module.description()
    }
    fn annotations(&self) -> apcore::ModuleAnnotations {
        self.annotations.clone()
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        context: &apcore::Context<serde_json::Value>,
    ) -> Result<serde_json::Value, ModuleError> {
        let started = std::time::Instant::now();
        let output = self.module.execute(input, context).await;
        if let Some(audit) = &self.audit {
            audit
                .log_execution(
                    &self.id,
                    &context.trace_id,
                    crate::governance::audit_caller_id(context),
                    if output.is_ok() { "success" } else { "error" },
                    if output.is_ok() { 0 } else { -1 },
                    started.elapsed().as_millis() as u64,
                )
                .await;
        }
        output
    }
}

fn complete_annotations(mut module: ScannedModule) -> ScannedModule {
    if module.description.trim().is_empty() {
        let method = module
            .metadata
            .get("http_method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("HTTP");
        let path = module
            .metadata
            .get("url_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("operation");
        module.description = format!("Invoke {method} {path}");
    }
    let annotations = module.annotations.get_or_insert_with(Default::default);
    annotations.open_world = true;
    annotations.requires_approval |= annotations.destructive;
    module
}
