<div align="center">
  <img src="./apexe-logo.svg" alt="apexe logo" width="200"/>
</div>

# apexe

Outside-In CLI-to-Agent Bridge — automatically wraps existing CLI tools into governed [apcore](https://github.com/aiperceivable) modules, served via MCP.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

## What is apexe?

`apexe` scans any CLI tool on your system (e.g., `git`, `docker`, `kubectl`, `ffmpeg`), deterministically extracts its command structure, flags, and arguments — then exposes them as governed MCP tools that AI agents can invoke safely.

**No LLM required for scanning. No changes to the CLI tools. Zero-config governance.**

### Key capabilities

- **Scan** — Three-tier deterministic engine (--help → man pages → shell completions) with 4 built-in parsers (GNU, Click, Cobra, Clap)
- **Schema** — Generates JSON Schema with type mapping, format hints (`path`, `uri`), defaults, enums, and required fields
- **Serve** — MCP server via [apcore-mcp](https://github.com/aiperceivable/apcore-mcp-rust) (stdio / streamable-http / SSE) with JWT auth and Explorer UI
- **Govern** — Behavioral annotations (readonly/destructive/idempotent), flag boosting (`--force` → requires_approval), default-deny ACL, audit logging
- **AI Guidance** — Every error includes `ai_guidance` to help agents self-correct; non-zero exit codes return stderr context

### Built on the apcore ecosystem

| Crate | Role |
|-------|------|
| [apcore](https://github.com/aiperceivable/apcore-rust) 0.14 | Module trait, Registry, ACL, ModuleError, Context |
| [apcore-toolkit](https://github.com/aiperceivable/apcore-toolkit-rust) 0.4 | ScannedModule, YAMLWriter, DisplayResolver |
| [apcore-mcp](https://github.com/aiperceivable/apcore-mcp-rust) 0.11 | MCP server with middleware, auth, Explorer UI |
| [apcore-cli](https://github.com/aiperceivable/apcore-cli-rust) 0.3 | AuditLogger, Sandbox |

---

## Installation

Requires **Rust 1.75+** and Cargo.

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

---

## Quick Start

```bash
# Scan git — extracts commands, flags, types, annotations
apexe scan git

# See what was generated
apexe list

# Start MCP server (Claude Desktop / Cursor)
apexe serve

# Or HTTP with browser-based tool explorer
apexe serve --transport http --port 8000 --explorer
```

### Claude Desktop integration

```bash
apexe serve --show-config claude-desktop
# Copy output to ~/Library/Application Support/Claude/claude_desktop_config.json
# Restart Claude Desktop — git commands appear as MCP tools
```

### Cursor integration

```bash
apexe serve --show-config cursor
# Add to Cursor's MCP settings
```

---

## Commands

### `apexe scan <TOOLS>...`

Scan CLI tools and generate binding files + ACL rules.

```bash
apexe scan git docker kubectl ffmpeg    # Scan multiple tools
apexe scan git --depth 3               # 3 levels of subcommands (default: 2, max: 5)
apexe scan git --no-cache              # Force re-scan
apexe scan git --format json           # Output as JSON (also: yaml, table)
apexe scan git --output-dir ./out      # Custom output directory
```

### `apexe serve`

Start MCP server for scanned tools.

```bash
apexe serve                                         # stdio (default)
apexe serve --transport http --port 8000             # HTTP
apexe serve --transport http --port 8000 --explorer  # HTTP + browser UI
apexe serve --transport sse --port 8000              # Server-Sent Events
apexe serve --show-config claude-desktop             # Print integration config
apexe serve --name my-tools                          # Custom server name
```

### `apexe list`

List registered modules.

```bash
apexe list                  # Table format
apexe list --format json    # JSON format
```

### `apexe config`

Show or initialize configuration.

```bash
apexe config --show     # Print resolved config (YAML)
apexe config --init     # Create ~/.apexe/config.yaml
```

---

## How It Works

```
CLI Tool Binary
      |
      v
+--------------------+
|   Scanner Engine   |  <-- Tier 1: --help (GNU/Click/Cobra/Clap)
|                    |  <-- Tier 2: man pages (DESCRIPTION + OPTIONS)
|                    |  <-- Tier 3: shell completions (subcommand discovery)
+---------+----------+
          |  ScannedCLITool
          v
+--------------------+
|   Adapter Layer    |  <-- module IDs, JSON Schema, annotations, display metadata
+---------+----------+
          |  ScannedModule (apcore-toolkit)
          |
    +-----+-----+
    |           |
    v           v
+--------+  +------------------+
| Output |  |   MCP Server     |
| .yaml  |  | apcore-mcp       |
| ACL    |  | stdio/http/sse   |
| Audit  |  | middleware+auth   |
+--------+  +------------------+
```

### Behavioral annotations

| Signal | Inference |
|--------|-----------|
| Command `list`, `show`, `status`, `get` | `readonly: true`, `cacheable: true` |
| Command `delete`, `rm`, `kill`, `destroy` | `destructive: true`, `requires_approval: true` |
| Flag `--force`, `-f`, `--hard` | Escalates to `requires_approval: true` |
| Flag `--dry-run`, `--check`, `--simulate` | `idempotent: true` |

### Schema generation

| CLI Type | JSON Schema |
|----------|-------------|
| `--message "hello"` | `"type": "string"` |
| `--count 5` | `"type": "integer"` |
| `--config /path` | `"type": "string", "format": "path"` |
| `--url https://...` | `"type": "string", "format": "uri"` |
| `--format json\|yaml` | `"type": "string", "enum": ["json","yaml"]` |
| `--include a --include b` | `"type": "array", "items": {"type":"string"}` |

---

## Configuration

Resolved in 4 tiers (highest wins): **CLI flags > env vars > config file > defaults**

```bash
apexe config --init    # Creates ~/.apexe/config.yaml
```

| Env Variable | Default | Description |
|-------------|---------|-------------|
| `APEXE_MODULES_DIR` | `~/.apexe/modules` | Binding file storage |
| `APEXE_CACHE_DIR` | `~/.apexe/cache` | Scan cache |
| `APEXE_LOG_LEVEL` | `info` | Log level |
| `APEXE_TIMEOUT` | `30` | CLI subprocess timeout (seconds) |
| `APEXE_SCAN_DEPTH` | `2` | Subcommand recursion depth |

---

## File Locations

| Path | Purpose |
|------|---------|
| `~/.apexe/config.yaml` | Configuration |
| `~/.apexe/modules/*.binding.yaml` | Generated tool bindings |
| `~/.apexe/cache/` | Scan result cache |
| `~/.apexe/acl.yaml` | Access control rules |
| `~/.apexe/audit.jsonl` | Audit trail |

---

## Examples

See [examples/README.md](examples/README.md) for full details.

| Example | Description | Run |
|---------|-------------|-----|
| [basic](examples/basic/) | Shell script: scan → list → serve | `./examples/basic/run.sh` |
| [programmatic](examples/programmatic.rs) | Rust library: scan → convert → export OpenAI tools → build MCP server | `cargo run --example programmatic` |

---

## Developer Guide

### Build & Test

```bash
cargo build                                             # Build
cargo test --all-features                               # Run tests (~338)
cargo test -- --include-ignored                         # Include integration tests
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo fmt --all -- --check                              # Format check
cargo run --example programmatic                        # Run example
```

### Adding a Custom Parser

Implement the `CliParser` trait in `src/scanner/protocol.rs`:

```rust
pub trait CliParser: Send + Sync {
    fn name(&self) -> &str;
    fn can_parse(&self, help_text: &str) -> bool;
    fn parse(&self, help_text: &str, tool_name: &str) -> anyhow::Result<ParsedHelp>;
    fn priority(&self) -> u32; // lower = tried first
}
```

### Logging

```bash
RUST_LOG=debug apexe scan git
apexe --log-level trace scan git
```

---

## Documentation

| Document | Description |
|----------|-------------|
| **[Quick Start](docs/quickstart.md)** | Get running in 30 seconds |
| **[User Manual](docs/user-manual.md)** | Full reference — commands, config, scanning, schema generation, annotations, governance, MCP server, AI integration, error handling |
| **[Examples](examples/README.md)** | Shell script walkthrough + Rust library API usage |
| **[Changelog](CHANGELOG.md)** | Release history and migration notes |

### Architecture & Design

| Document | Description |
|----------|-------------|
| [Technical Design](docs/apcore-integration/tech-design.md) | v0.1.0 architecture with apcore ecosystem integration |
| [Feature Manifest](docs/FEATURE_MANIFEST.md) | Module map, crate dependencies, project status |
| [Feature Specs](docs/features/v2-overview.md) | Detailed specifications for features F1-F7 |

### Feature Specs

| Spec | Description |
|------|-------------|
| [F1: Scanner Adapter](docs/features/v2-f1-scanner-adapter.md) | ScannedCLITool → ScannedModule conversion |
| [F2: Module Executor](docs/features/v2-f2-module-executor.md) | apcore Module trait for CLI subprocess execution |
| [F3: Binding Output](docs/features/v2-f3-binding-output.md) | apcore-toolkit YAMLWriter integration |
| [F4: MCP Server](docs/features/v2-f4-mcp-server.md) | apcore-mcp server builder |
| [F5: Governance](docs/features/v2-f5-governance.md) | ACL + AuditLogger + Sandbox wrappers |
| [F6: Error Migration](docs/features/v2-f6-error-migration.md) | ApexeError → ModuleError conversion |
| [F7: Config Integration](docs/features/v2-f7-config-integration.md) | apcore Config integration |

---

## License

Apache-2.0
