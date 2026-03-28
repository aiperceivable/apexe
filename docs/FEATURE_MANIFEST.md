# apexe Feature Manifest

## Project Overview

**apexe** -- Outside-In CLI-to-Agent Bridge. Automatically wraps CLI tools into governed apcore modules, served via MCP/A2A.

**Version:** 0.1.0 — Full apcore ecosystem integration.

**Status:** All features implemented. 335 tests passing, 0 failures. ~8,850 LOC Rust.

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
      └──→ [CliModule]  ──→ apcore Module trait
              |
              v
      [McpServerBuilder] ──→ apcore-mcp (stdio/http/sse)
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
├── governance/      Access control, audit, sandbox
│   ├── acl          AclManager wrapping apcore::ACL
│   ├── audit        AuditManager wrapping apcore_cli::AuditLogger
│   └── sandbox      SandboxManager wrapping apcore_cli::Sandbox
├── mcp/             MCP server integration
│   └── server       McpServerBuilder wrapping apcore_mcp::APCoreMCP
├── models/          ScannedCLITool, ScannedCommand, ScannedFlag, ScannedArg
├── module/          apcore Module trait implementation
│   ├── cli_module   CliModule (subprocess execution via Module trait)
│   └── executor     Argument building, injection prevention, spawn_blocking
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
| `apcore` | 0.14 | Module trait, Registry, ACL, ModuleError, ErrorCode, Context, Config |
| `apcore-toolkit` | 0.4 | ScannedModule, YAMLWriter, Verifier, ModuleAnnotations |
| `apcore-mcp` | 0.11 | APCoreMCP server (stdio, streamable-http, SSE, JWT auth, Explorer UI) |
| `apcore-cli` | 0.3 | AuditLogger (JSONL audit), Sandbox (subprocess isolation) |

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
- Async execution via `tokio::task::spawn_blocking` with `tokio::time::timeout`
- Shell injection prevention (`;|&$\`'"` blocked)
- Preflight validation on all string inputs

### Output Layer (v0.1.0 new, replaces v0.1.x binding generator)
- `YamlOutput`: wraps apcore-toolkit `YAMLWriter` with verification
- `load_modules_from_dir`: reads `.binding.yaml` files back as `Vec<ScannedModule>`

### MCP Server (v0.1.0 new, replaces v0.1.x self-built server)
- `McpServerBuilder`: modules_dir → Registry → Executor → APCoreMCP
- Transports: stdio, streamable-http (was "http"), SSE
- Full MCP protocol compliance via apcore-mcp
- JWT authentication support, Explorer UI (HTTP transports)

### Governance (v0.1.0 rewritten)
- `AclManager`: wraps `apcore::ACL`, generates default rules from annotations
- `AuditManager`: wraps `apcore_cli::AuditLogger`, JSONL append-only with SHA-256 hashing
- `SandboxManager`: wraps `apcore_cli::Sandbox`, subprocess isolation with timeout

## Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `apcore` | Core module system, ACL, errors |
| `apcore-toolkit` | Scanner types, YAML writer, verifiers |
| `apcore-mcp` | MCP protocol server |
| `apcore-cli` | Audit logging, sandbox isolation |
| `clap` (derive mode) | CLI argument parsing |
| `serde` + `serde_json` + `serde_yaml` | Serialization |
| `tokio` | Async runtime |
| `tracing` + `tracing-subscriber` | Structured logging |
| `thiserror` | Typed error definitions |
| `nom` | Parser combinators for help text |
| `regex` | Pattern matching for help format detection |
| `sha2` | SHA-256 hashing for audit privacy |
| `uuid` | UUID v4 for trace IDs |
| `shell-words` | Shell argument splitting |

## Open Items

1. **A2A protocol** -- Deferred to v0.3.0.
2. **CLI rewiring completion** -- `apexe scan` fully rewired; `apexe serve` uses McpServerBuilder.
3. **`apexe evo`** -- Deferred. Depends on apevo product maturity.
