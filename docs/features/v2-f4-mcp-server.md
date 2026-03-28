# F4: MCP Server -- Replace Custom MCP with apcore-mcp

| Field | Value |
|---|---|
| **Feature ID** | F4 |
| **Tech Design Section** | 5.4 |
| **Priority** | P1 (Serve) |
| **Dependencies** | F2 (Module Executor), F3 (Binding Output) |
| **Depended On By** | F7 (Config Integration) |
| **New Files** | None (logic moves into `src/cli/mod.rs` ServeArgs::execute()) |
| **Deleted Files** | `src/serve/handler.rs`, `src/serve/mcp_types.rs`, `src/serve/registry.rs`, `src/serve/loader.rs`, `src/serve/stdio.rs`, `src/serve/http.rs`, `src/serve/mod.rs` |
| **Kept Files** | `src/serve/config_gen.rs` (moved to `src/cli/config_gen.rs` or kept in place) |
| **Estimated LOC** | -1,100 net (delete ~1,200, add ~100 in CLI) |
| **Estimated Tests** | ~10 (integration tests) |

---

## 1. Purpose

Replace the custom MCP JSON-RPC server (6 files, ~1,200 LOC) with apcore-mcp's `APCoreMCP` builder. This gains full MCP protocol compliance, streamable-http transport, JWT authentication, Explorer UI, and middleware integration without maintaining a custom implementation.

---

## 2. What Gets Deleted

### 2.1 `src/serve/handler.rs` -- McpHandler

The custom `McpHandler` struct that dispatches JSON-RPC requests (`initialize`, `tools/list`, `tools/call`, `ping`) is replaced entirely by apcore-mcp's internal routing.

### 2.2 `src/serve/mcp_types.rs` -- MCP Protocol Types

Custom types `JsonRpcRequest`, `JsonRpcResponse`, error codes (`METHOD_NOT_FOUND`, `INVALID_PARAMS`, etc.) are replaced by apcore-mcp's internal types.

### 2.3 `src/serve/registry.rs` -- ToolRegistry

The custom `ToolRegistry` (stores tool definitions, resolves tool names) is replaced by apcore's `Registry`.

### 2.4 `src/serve/loader.rs` -- Binding Loader

The custom binding YAML loader is replaced by `output::load_modules_from_dir()` (F3).

### 2.5 `src/serve/stdio.rs` -- Stdio Transport

The custom stdio JSON-RPC loop (reads stdin line by line, parses JSON-RPC, writes responses) is replaced by apcore-mcp's `"stdio"` transport.

### 2.6 `src/serve/http.rs` -- HTTP Transport

The custom axum-based HTTP server is replaced by apcore-mcp's `"streamable-http"` transport. This also removes the `axum` dependency from Cargo.toml.

---

## 3. New ServeArgs::execute() Implementation

```rust
// In src/cli/mod.rs

impl ServeArgs {
    pub fn execute(self, config: &ApexeConfig) -> Result<(), ModuleError> {
        // Step 1: Handle --show-config (unchanged)
        if let Some(ref format) = self.show_config {
            let output = config_gen::generate_config(
                format, &self.name, &self.transport, &self.host, self.port,
            );
            println!("{output}");
            return Ok(());
        }

        // Step 2: Load modules from binding YAML files
        let modules_dir = self.modules_dir
            .unwrap_or_else(|| config.modules_dir.clone());

        let modules = if modules_dir.is_dir() {
            let loaded = crate::output::load_modules_from_dir(&modules_dir)?;
            if loaded.is_empty() {
                tracing::warn!(
                    dir = %modules_dir.display(),
                    "No binding files found. Run `apexe scan` first."
                );
            } else {
                tracing::info!(
                    count = loaded.len(),
                    dir = %modules_dir.display(),
                    "Loaded tools"
                );
            }
            loaded
        } else {
            tracing::warn!(
                dir = %modules_dir.display(),
                "Modules directory not found. Starting with zero tools."
            );
            vec![]
        };

        // Step 3: Create governance managers
        let audit = Arc::new(AuditManager::new(&config.audit_log));
        let sandbox = if self.sandbox {
            Some(Arc::new(SandboxManager::new(true, config.default_timeout * 1000)))
        } else {
            None
        };

        // Step 4: Register modules into apcore Registry
        let registry = Registry::new();
        let registry_output = RegistryOutput::new(
            config.default_timeout * 1000,
            sandbox.clone(),
            audit.clone(),
        );
        let count = registry_output.register(&modules, &registry, false, false)?;
        tracing::info!(count, "Registered CLI modules");

        // Step 5: Create Executor with middleware
        let executor = Executor::new(registry.clone(), Default::default());

        // Load ACL if available
        let acl_path = config.config_dir.join("acl.yaml");
        if acl_path.exists() {
            let acl = AclManager::from_config(&acl_path)?;
            executor.set_acl(acl.into_inner())?;
        }

        // Add middleware
        executor.use_middleware(LoggingMiddleware::new())?;
        executor.use_middleware(TracingMiddleware::new())?;

        // Step 6: Build and start MCP server
        let transport = match self.transport.as_str() {
            "stdio" => "stdio",
            "http" => "streamable-http",
            "sse" => "sse",
            other => return Err(ModuleError {
                code: ErrorCode::ValidationFailed,
                message: format!("Unsupported transport: {}", other),
                ..Default::default()
            }),
        };

        let mut builder = APCoreMCP::builder()
            .backend(BackendSource::Executor(Arc::new(executor)))
            .name(&self.name)
            .transport(transport)
            .host(&self.host)
            .port(self.port)
            .validate_inputs(true);

        if self.explorer {
            builder = builder.include_explorer(true);
        }

        if self.require_auth {
            builder = builder.require_auth(true);
            // JWT authenticator configuration loaded from config
        }

        let server = builder.build().map_err(|e| ModuleError {
            code: ErrorCode::InternalError,
            message: format!("Failed to build MCP server: {}", e),
            ..Default::default()
        })?;

        // Step 7: Serve (blocking)
        if transport == "stdio" {
            if self.explorer {
                tracing::warn!("Explorer requires HTTP transport. Ignored.");
            }
        }

        tracing::info!(
            transport,
            host = %self.host,
            port = self.port,
            "Starting MCP server"
        );

        server.serve();
        Ok(())
    }
}
```

---

## 4. CLI Changes

### 4.1 New Flags on ServeArgs

```rust
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    // Existing flags (unchanged)
    #[arg(long, default_value = "stdio", value_parser = ["stdio", "http", "sse"])]
    pub transport: String,
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, default_value = "8000", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: u16,
    #[arg(long)]
    pub explorer: bool,
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,
    #[arg(long, default_value = "apexe")]
    pub name: String,
    #[arg(long)]
    pub show_config: Option<String>,

    // NEW flags
    /// Enable subprocess sandboxing for CLI execution.
    #[arg(long)]
    pub sandbox: bool,

    /// Require JWT authentication for all requests.
    #[arg(long)]
    pub require_auth: bool,
}
```

### 4.2 Removed Flag

The `--a2a` flag is removed (non-goal for v0.2.0; NG4 in tech design).

### 4.3 Transport Mapping

| CLI Value | apcore-mcp Transport | Notes |
|---|---|---|
| `stdio` | `"stdio"` | Direct mapping |
| `http` | `"streamable-http"` | Name change; v0.1.x used plain SSE, v0.2.0 uses streamable-http |
| `sse` | `"sse"` | Direct mapping |

---

## 5. Behavior Changes from v0.1.x

### 5.1 Protocol Compliance

v0.1.x implemented a subset of MCP (initialize, tools/list, tools/call, ping). apcore-mcp implements the full specification including:
- `resources/list`, `resources/read`
- `prompts/list`, `prompts/get`
- `logging/setLevel`
- Notifications
- Progress reporting

### 5.2 Error Responses

v0.1.x used custom error codes. apcore-mcp uses standard JSON-RPC error codes with `ModuleError` details.

### 5.3 Tool Names

v0.1.x used module_id as the tool name. apcore-mcp may apply a prefix via the builder's `.prefix()` method. By default, no prefix is applied, preserving backward compatibility.

### 5.4 Explorer UI

v0.1.x had a basic root endpoint with server metadata. apcore-mcp's Explorer UI provides a full browser-based tool exploration interface with input forms and live execution.

---

## 6. Test Scenarios

### 6.1 Integration Tests (in `tests/mcp_integration.rs`)

| Test Name | Scenario | Expected |
|---|---|---|
| `test_mcp_serve_starts_with_no_modules` | Empty modules dir | Server starts, tools/list returns empty |
| `test_mcp_serve_loads_binding_files` | Dir with git.binding.yaml | tools/list returns git modules |
| `test_mcp_serve_tools_call_executes` | Call "cli.echo.echo" with message | Returns stdout with message |
| `test_mcp_serve_tools_call_injection_blocked` | Call with shell metacharacters | Error response with ValidationFailed |
| `test_mcp_serve_explorer_http_only` | --explorer with stdio transport | Warning logged, explorer not available |
| `test_mcp_serve_invalid_transport` | --transport "invalid" | Error before server starts |

### 6.2 CLI Parse Tests (in `src/cli/mod.rs` tests)

| Test Name | Scenario | Expected |
|---|---|---|
| `test_serve_sandbox_flag` | --sandbox | args.sandbox = true |
| `test_serve_require_auth_flag` | --require-auth | args.require_auth = true |
| `test_serve_default_no_sandbox` | No flags | args.sandbox = false |
| `test_serve_default_no_auth` | No flags | args.require_auth = false |

---

## 7. Dependency Removal

### 7.1 axum

The `axum` dependency (and its transitive deps `http`, `hyper`, `tower-http`) is removed from Cargo.toml. apcore-mcp provides its own HTTP server.

### 7.2 Related Dev Dependencies

`http-body-util` and `tower` dev-dependencies may be removable if no remaining tests use them. Evaluate after deletion.

---

## 8. Config Snippet Generation

`src/serve/config_gen.rs` is the only file kept from `src/serve/`. It generates integration snippets for claude-desktop and cursor. Two options:

**Option A**: Move to `src/cli/config_gen.rs` (keeps it close to the CLI that uses it).
**Option B**: Keep at `src/serve/config_gen.rs` (minimal file moves).

Recommendation: Option A, since `src/serve/` is otherwise deleted.

The generated config format is unchanged. The only difference is that the server binary name and transport names match the new apcore-mcp conventions.

---

## 9. Migration Notes

### Test Deletion

62 serve tests are deleted. They tested custom JSON-RPC parsing, custom handler routing, custom registry lookup, and custom transport I/O. These behaviors are now tested within the apcore-mcp crate itself.

10 new integration tests replace them, testing the integration points (module loading, registration, execution through the server).

### Rollback Plan

If apcore-mcp proves incompatible during development, the custom serve implementation can be restored from git history. The scanner adapter (F1) and module executor (F2) are designed to be useful even without the MCP server replacement.
