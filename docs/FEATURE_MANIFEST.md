# apexe Feature Manifest

## Project Overview

**apexe** -- Outside-In CLI-to-Agent Bridge. Automatically wraps CLI tools into governed apcore modules, served via MCP/A2A.

**Version:** 0.3.0 — Full apcore ecosystem integration (MCP + A2A).

**Status:** All features implemented. 407 tests passing, 0 failures. ~11,300 LOC Rust.

> The section headings below tagged "(v0.1.0 …)" record when each piece first
> landed; the crate is now **0.3.0**. For the authoritative current state see
> [`CHANGELOG.md`](../CHANGELOG.md) and [`docs/user-manual.md`](user-manual.md).

## Architecture (v0.1.0)

```
CLI Tool Binary
      |
      v
[Scanner Engine] ──→ ScannedCLITool
      |
      v
[Adapter Layer] ──→ ScannedModule (apcore-toolkit)
      |
      ├──→ [YamlOutput] ──→ .binding.yaml files
      ├──→ [AclManager] ──→ acl.yaml (apcore ACL)
      └──→ [CliModule]  ──→ apcore Module trait (governed Executor)
              |
              ├──→ [McpServerBuilder] ──→ apcore-mcp  (stdio/http/sse)
              └──→ [A2aServerBuilder] ──→ apcore-a2a  (HTTP agent)
```

## Module Map

```
src/
├── adapter/         ScannedCLITool → ScannedModule conversion
│   ├── converter    CliToolConverter (tree flattening, module ID generation)
│   ├── schema       JSON Schema from flags/args (extracted from v0.1.x)
│   └── annotations  ModuleAnnotations inference (readonly/destructive/idempotent)
├── cli/             clap CLI entry point (scan/serve/list/config)
│   └── config_gen   Claude Desktop / Cursor config snippet generation
├── config           ApexeConfig + apcore CoreConfig integration
├── errors           ApexeError + From<ApexeError> for ModuleError
├── governance/      Access control, audit
│   ├── acl          AclManager wrapping apcore::ACL
│   └── audit        AuditManager wrapping apcore_cli::AuditLogger
│                    (subprocess isolation is always-on in module/executor.rs)
├── mcp/             MCP server integration
│   └── server       McpServerBuilder wrapping apcore_mcp::APCoreMCP
├── models/          ScannedCLITool, ScannedCommand, ScannedFlag, ScannedArg
├── module/          apcore Module trait implementation
│   ├── cli_module   CliModule (subprocess execution via Module trait)
│   └── executor     Argument building, injection prevention, env scrubbing,
│                     tokio::process (timeout + kill_on_drop)
├── output/          Binding file I/O
│   ├── yaml         YamlOutput wrapping apcore_toolkit::YAMLWriter
│   └── loader       load_modules_from_dir (reads .binding.yaml)
└── scanner/         3-tier deterministic CLI scanner engine
    ├── orchestrator  ScanOrchestrator (top-level coordinator)
    ├── pipeline      ParserPipeline (priority-based parser selection)
    ├── parsers/      GNU, Click, Cobra, Clap format parsers
    ├── discovery     SubcommandDiscovery (recursive subcommand scanning)
    ├── cache         ScanCache (JSON filesystem caching)
    └── resolver      ToolResolver (binary path + version + format detection)
```

## apcore Ecosystem Integration

| Crate | Version | Usage |
|-------|---------|-------|
| `apcore` | 0.26 | Module trait, Registry, ACL, ModuleError, ErrorCode, Context, Config |
| `apcore-toolkit` | 0.10 | ScannedModule, YAMLWriter, Verifier, ModuleAnnotations, `deduplicate_ids` |
| `apcore-mcp` | 0.17 | APCoreMCP server (stdio, streamable-http, SSE, Explorer UI) |
| `apcore-a2a` | 0.4 | A2A agent server (`async_serve` / `build_app`, `Authenticator`) |
| `apcore-cli` | 0.10 | AuditLogger (JSONL audit) |

## v0.1.0 Features

### Scanner Engine (preserved from v0.1.x)
Three-tier deterministic scanner with plugin system:

1. **Tier 1 -- `--help` parser** (4 built-in parsers: GNU, Click, Cobra, Clap)
2. **Tier 2 -- Man page parser** (DESCRIPTION extraction)
3. **Tier 3 -- Shell completion parser** (zsh/bash subcommand discovery)

Additional: ParserPipeline, SubcommandDiscovery, ScanCache, ToolResolver, plugin system.

### Adapter Layer (v0.1.0 new)
- `CliToolConverter`: flattens subcommand trees → `Vec<ScannedModule>`
- `schema::build_input_schema/output_schema`: JSON Schema from flags/args
- `annotations::infer`: readonly/destructive/idempotent inference from command names

### Module Executor (v0.1.0 new)
- `CliModule`: implements apcore `Module` trait for CLI subprocess execution
- Async execution via `tokio::process::Command` with `tokio::time::timeout` and `kill_on_drop(true)`
- Shell injection prevention (`;|&$\`'"` blocked)
- Preflight validation on all string inputs

### Output Layer (v0.1.0 new, replaces v0.1.x binding generator)
- `YamlOutput`: wraps apcore-toolkit `YAMLWriter` with verification
- `load_modules_from_dir`: reads `.binding.yaml` files back as `Vec<ScannedModule>`

### MCP Server (v0.1.0 new, replaces v0.1.x self-built server)
- `McpServerBuilder`: modules_dir → Registry → Executor → APCoreMCP
- Transports: stdio, streamable-http (was "http"), SSE
- Full MCP protocol compliance via apcore-mcp
- Explorer UI (HTTP transports); transport authentication via the apcore library API (localhost bind by default)

### Governance (v0.1.0 rewritten)
- `AclManager`: wraps `apcore::ACL`, generates default rules from annotations; fails closed when a configured ACL is missing/malformed
- `AuditManager`: wraps `apcore_cli::AuditLogger`, JSONL append-only with SHA-256 hashing; records executions and ACL allow/deny decisions (log `0o600`)
- Subprocess isolation: always-on in `module/executor.rs` — env scrubbing, no-shell argv, output cap, timeout + `kill_on_drop` (no separate SandboxManager)

## Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `apcore` | Core module system, ACL, errors |
| `apcore-toolkit` | Scanner types, YAML writer, verifiers |
| `apcore-mcp` | MCP protocol server |
| `apcore-cli` | Audit logging (JSONL) |
| `clap` (derive mode) | CLI argument parsing |
| `serde` + `serde_json` + `serde_yaml` | Serialization |
| `tokio` | Async runtime |
| `tracing` + `tracing-subscriber` | Structured logging |
| `thiserror` | Typed error definitions |
| `regex` | Help-text parsing and format detection (all parsers) |
| `sha2` | SHA-256 hashing for audit privacy |
| `uuid` | UUID v4 for trace IDs |
| `shell-words` | Shell argument splitting |

## Open Items

1. **A2A protocol** -- Implemented in 0.3.0 (`apexe a2a`, `A2aServerBuilder`).
2. **CLI rewiring completion** -- `apexe scan` fully rewired; `apexe serve` uses McpServerBuilder.
3. **`apexe evo`** -- Deferred. Depends on apevo product maturity.
