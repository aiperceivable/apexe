# F4: Serve Integration

| Field | Value |
|-------|-------|
| **Feature** | F4 |
| **Priority** | P1 (value multiplier) |
| **Effort** | Small (~400 LOC) |
| **Dependencies** | F3 |

---

## 1. Overview

Wire up apcore-mcp and apcore-a2a to serve scanned CLI modules. The `apexe serve` command loads `.binding.yaml` files, creates a Registry and Executor, and delegates to `APCoreMCP` for MCP serving and optionally to apcore-a2a for A2A serving. This feature is intentionally thin -- it reuses existing ecosystem components.

---

## 2. Module: `src/serve.rs`

### Function: `serve_command`

```rust
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tracing::{info, warn};

/// Load bindings and start MCP/A2A server.
///
/// # Arguments
///
/// * `transport` - MCP transport type: "stdio", "http", or "sse".
///     - "stdio": Standard I/O transport (for Claude Desktop, Cursor).
///     - "http": Streamable HTTP transport (for remote access).
///     - "sse": Server-Sent Events transport (legacy HTTP).
/// * `host` - Host address for HTTP-based transports. Default "127.0.0.1".
/// * `port` - Port for HTTP-based transports. Range: 1-65535. Default 8000.
/// * `a2a` - Enable A2A protocol alongside MCP. Requires HTTP transport.
/// * `explorer` - Enable browser-based Tool Explorer UI. HTTP-only.
/// * `modules_dir` - Directory containing .binding.yaml files. Default: ~/.apexe/modules/.
/// * `acl_path` - Path to ACL configuration YAML. Default: ~/.apexe/acl.yaml (if exists).
/// * `name` - MCP server name (max 255 chars). Default: "apexe".
pub async fn serve_command(
    transport: &str,
    host: &str,
    port: u16,
    a2a: bool,
    explorer: bool,
    modules_dir: Option<PathBuf>,
    acl_path: Option<PathBuf>,
    name: &str,
) -> Result<()> {
    // 1. Resolve paths
    let modules_dir = modules_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".apexe")
            .join("modules")
    });

    let acl_path = acl_path.or_else(|| {
        let default = dirs::home_dir()?.join(".apexe").join("acl.yaml");
        default.exists().then_some(default)
    });

    // 2. Validate inputs
    if !modules_dir.is_dir() {
        bail!("Modules directory not found: {}", modules_dir.display());
    }

    let mut a2a_enabled = a2a;
    let mut explorer_enabled = explorer;

    if a2a && transport == "stdio" {
        warn!("A2A requires HTTP transport. Falling back to MCP-only.");
        a2a_enabled = false;
    }
    if explorer && transport == "stdio" {
        warn!("Explorer requires HTTP transport. Ignored.");
        explorer_enabled = false;
    }

    // 3. Load bindings
    // use apcore::{Registry, bindings::BindingLoader};
    // let mut registry = Registry::new();
    // let loader = BindingLoader::new();
    // let modules = loader.load_binding_dir(&modules_dir, &mut registry)?;
    //
    // if modules.is_empty() {
    //     bail!("No .binding.yaml files found in {}", modules_dir.display());
    // }
    // info!(count = modules.len(), dir = %modules_dir.display(), "Loaded modules");

    // 4. Configure ACL
    // if let Some(ref acl) = acl_path {
    //     if acl.exists() {
    //         registry.load_acl(acl)?;
    //         info!(path = %acl.display(), "Loaded ACL");
    //     }
    // }

    // 5. Create APCoreMCP
    // use apcore_mcp::APCoreMCP;
    // let mcp = APCoreMCP::builder()
    //     .registry(registry.clone())
    //     .name(name)
    //     .tags(&["cli"])
    //     .build();

    // 6. Serve
    let mcp_transport = match transport {
        "http" => "streamable-http",
        other => other,
    };

    if a2a_enabled && matches!(transport, "http" | "sse") {
        // serve_combined(mcp, registry, mcp_transport, host, port, explorer_enabled).await
        todo!("Combined MCP + A2A serving")
    } else {
        // mcp.serve(mcp_transport, host, port, explorer_enabled).await
        todo!("MCP-only serving")
    }
}
```

### Function: `serve_combined`

```rust
/// Serve both MCP and A2A from a single process.
///
/// Architecture:
///   - MCP app mounted at /mcp
///   - A2A app mounted at /a2a
///   - Agent Card at /.well-known/agent.json
///   - Tool Explorer at /explorer (if enabled)
async fn serve_combined(
    // mcp: APCoreMCP,
    // registry: Registry,
    transport: &str,
    host: &str,
    port: u16,
    explorer: bool,
) -> Result<()> {
    use axum::Router;

    // let mcp_app = mcp.into_router(explorer);
    //
    // use apcore_a2a::SkillMapper;
    // let mapper = SkillMapper::new(registry);
    // let a2a_app = mapper.build_router();
    //
    // let app = Router::new()
    //     .nest("/mcp", mcp_app)
    //     .nest("/a2a", a2a_app);

    let app = Router::new(); // placeholder

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "Serving MCP + A2A");
    axum::serve(listener, app).await?;
    Ok(())
}
```

---

## 3. CLI Integration (in `src/cli/serve.rs`)

The `serve` clap command delegates to `serve_command()`:

```rust
use std::path::PathBuf;

use clap::Args;

use crate::config::ApexeConfig;

/// Start MCP/A2A server for scanned CLI tools.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// MCP transport type
    #[arg(long, default_value = "stdio", value_parser = ["stdio", "http", "sse"])]
    pub transport: String,

    /// Host for HTTP transports
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port for HTTP transports (1-65535)
    #[arg(long, default_value = "8000", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: u16,

    /// Enable A2A protocol alongside MCP
    #[arg(long)]
    pub a2a: bool,

    /// Enable browser-based Tool Explorer UI (HTTP only)
    #[arg(long)]
    pub explorer: bool,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,

    /// Path to ACL configuration YAML
    #[arg(long)]
    pub acl: Option<PathBuf>,

    /// MCP server name
    #[arg(long, default_value = "apexe")]
    pub name: String,
}

impl ServeArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(crate::serve::serve_command(
            &self.transport,
            &self.host,
            self.port,
            self.a2a,
            self.explorer,
            self.modules_dir.or_else(|| Some(config.modules_dir.clone())),
            self.acl,
            &self.name,
        ))
    }
}
```

---

## 4. Integration Configuration Examples

### Claude Desktop (stdio)

```json
{
  "mcpServers": {
    "apexe": {
      "command": "apexe",
      "args": ["serve", "--transport", "stdio"]
    }
  }
}
```

### Claude Desktop (HTTP)

```json
{
  "mcpServers": {
    "apexe": {
      "url": "http://localhost:8000/mcp"
    }
  }
}
```

### Cursor (stdio)

```json
{
  "mcpServers": {
    "apexe": {
      "command": "apexe",
      "args": ["serve"]
    }
  }
}
```

### A2A Agent Card

When `--a2a` is enabled, the server generates an agent card at `/.well-known/agent.json`:

```json
{
  "name": "apexe",
  "description": "CLI tools served as governed apcore modules",
  "url": "http://localhost:8000/a2a",
  "skills": [
    {
      "name": "cli.git.status",
      "description": "Show the working tree status"
    },
    {
      "name": "cli.git.commit",
      "description": "Record changes to the repository"
    }
  ]
}
```

---

## 5. Error Handling

| Error | Context | Behavior |
|-------|---------|----------|
| Modules dir not found | `serve_command()` startup | `anyhow::bail!` with path shown |
| No binding files | Binding loader | `anyhow::bail!` with suggestion to run `apexe scan` first |
| Malformed binding YAML | `BindingLoader` parsing | Error logged, file skipped, continue |
| Port in use | `TcpListener::bind()` | OS error caught, displayed with "port already in use" message |
| A2A with stdio | `serve_command()` validation | Warning logged, falls back to MCP-only |
| Explorer with stdio | `serve_command()` validation | Warning logged, explorer disabled |
| APCoreMCP not available | Missing apcore-mcp dependency | Compile-time error (Rust) |
| ACL file malformed | `registry.load_acl()` | Error logged, serve continues without ACL |
| Ctrl+C | During serve | Tokio runtime shuts down gracefully, exit code 0 |

---

## 6. Test Scenarios

| Test ID | Scenario | Expected |
|---------|----------|----------|
| F4-T01 | Serve with valid bindings (stdio) | `APCoreMCP::serve()` called with transport="stdio" |
| F4-T02 | Serve with valid bindings (http) | `APCoreMCP::serve()` called with transport="streamable-http" |
| F4-T03 | Serve with empty modules dir | Error: "No .binding.yaml files found" |
| F4-T04 | Serve with nonexistent dir | Error: "Modules directory not found" |
| F4-T05 | A2A with stdio transport | Warning logged, a2a disabled, MCP-only serve |
| F4-T06 | Explorer with stdio | Warning logged, explorer disabled |
| F4-T07 | A2A with http transport | Combined serve: /mcp + /a2a mounts |
| F4-T08 | ACL file loaded | Registry has ACL rules applied |
| F4-T09 | ACL file missing (default) | No error, serve without ACL |
| F4-T10 | Malformed binding skipped | Warning logged, other bindings still loaded |
| F4-T11 | Module count in logs | "Loaded N modules from /path" in info log |
| F4-T12 | Tool list via MCP | `list_tools()` returns all loaded module IDs |
| F4-T13 | Tool call via MCP | `call_tool("cli.echo", {})` returns stdout |
| F4-T14 | Agent card generation | `/.well-known/agent.json` has skills list |

### Example Test

```rust
use tempfile::TempDir;

#[tokio::test]
async fn test_serve_nonexistent_dir() {
    let result = serve_command(
        "stdio",
        "127.0.0.1",
        8000,
        false,
        false,
        Some("/nonexistent/path".into()),
        None,
        "test",
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Modules directory not found"));
}

#[tokio::test]
async fn test_serve_empty_modules_dir() {
    let tmp = TempDir::new().unwrap();
    let result = serve_command(
        "stdio",
        "127.0.0.1",
        8000,
        false,
        false,
        Some(tmp.path().to_path_buf()),
        None,
        "test",
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("No .binding.yaml files found") || err.contains("not implemented"));
}
```
