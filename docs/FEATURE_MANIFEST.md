# apexe Feature Manifest

## Project Overview

**apexe** -- Outside-In CLI-to-Agent Bridge. Automatically wraps CLI tools into governed apcore modules, served via MCP/A2A.

**Status:** All 5 features implemented. 393 tests passing, 0 failures. ~9,500 LOC Rust.

## Feature Decomposition

```
F1: Project Skeleton & CLI Framework         [DONE]
 |
F2: CLI Scanner Engine                        [DONE]
 |
F3: Binding Generator                         [DONE]
 |
F4: Serve Integration (self-built MCP)        [DONE]
 |
F5: Governance Defaults (ACL + Annotations)   [DONE]
```

## Features

### F1: Project Skeleton & CLI Framework
**Priority:** P0 | **Status:** DONE | **Actual LOC:** 1,424

Rust crate with clap-based CLI:
- `apexe scan <tool> [<tool>...]` -- scans CLI tools, generates .binding.yaml
- `apexe serve [--transport stdio|http] [--a2a]` -- starts MCP server
- `apexe list [--format json|table]` -- lists scanned tools and modules
- `apexe config [--show] [--init]` -- shows/initializes configuration
- Config resolution: `~/.apexe/config.yaml` → env vars → CLI flags (three-tier)
- Output directory: `~/.apexe/modules/` (default) or `--output-dir`

**Verified:**
- `cargo install --path .` ✓
- `apexe --help` shows scan, serve, list, config ✓
- `apexe scan --help` shows TOOLS, --output-dir, --depth, --no-cache, --format ✓
- 61 tests passing ✓

---

### F2: CLI Scanner Engine
**Priority:** P0 | **Status:** DONE | **Actual LOC:** 3,537

Three-tier deterministic scanner with plugin system:

1. **Tier 1 -- `--help` parser** (4 built-in parsers)
   - GnuHelpParser (regex + nom-based flag line parser)
   - ClickHelpParser (Click/argparse patterns)
   - CobraHelpParser (Go Cobra patterns)
   - ClapHelpParser (Rust Clap patterns)

2. **Tier 2 -- Man page parser**
   - Extracts DESCRIPTION section from `man -P cat <tool>`

3. **Tier 3 -- Shell completion parser**
   - Parses zsh/bash completion scripts for subcommand discovery

**Additional components:**
- `ParserPipeline` with priority routing + user YAML override
- `SubcommandDiscovery` with recursive discovery + max depth + stderr fallback
- `ScanCache` with JSON filesystem caching + invalidation
- `ToolResolver` with binary path resolution + version detection + help format detection
- Plugin system via `CliParser` trait (programmatic registration)

**Verified:**
- 138 tests passing ✓
- Integration tests for git/docker scan (marked #[ignore], require real tools) ✓
- Graceful degradation on unparseable help ✓

---

### F3: Binding Generator
**Priority:** P0 | **Status:** DONE | **Actual LOC:** 993

- **Module ID generator:** `git commit` → `cli.git.commit` (sanitized, 128-char limit)
- **JSON Schema generator:** flags → properties with type mapping, enums, defaults, arrays for repeatable
- **Binding YAML writer:** one `.binding.yaml` per tool, multiple bindings per file
- **CLI executor:** `std::process::Command` (no shell), command injection prevention via character validation
- **Structured output:** auto-detect JSON output flags (`--format json`, `--json`), parse JSON stdout

**Verified:**
- `apexe scan <tool> --output-dir ./modules` produces valid `.binding.yaml` ✓
- Command injection blocked (`;|&$` backtick etc.) ✓
- 63 tests passing ✓

---

### F4: Serve Integration
**Priority:** P1 | **Status:** DONE | **Actual LOC:** 1,544

Self-built MCP JSON-RPC 2.0 server (apcore-mcp-rust does not exist yet):

1. **Binding loader:** discovers and parses `.binding.yaml` files from modules directory
2. **Tool registry:** HashMap-backed tool lookup with register/get/list
3. **MCP handler:** dispatches `initialize`, `tools/list`, `tools/call` JSON-RPC methods
4. **Stdio transport:** one JSON-RPC message per line (Claude Desktop / Cursor compatible)
5. **HTTP transport:** axum server with POST `/mcp` + GET `/health`
6. **Config generator:** `--show-config claude-desktop|cursor` prints integration snippets

**Known limitations:**
- A2A protocol: **stub only** (prints warning, falls back to MCP-only)
- Agent Card (`/.well-known/agent.json`): **not yet implemented**
- SSE transport: **stub endpoint only**
- Will migrate to apcore-mcp-rust when available

**Verified:**
- `apexe serve --transport stdio` works with Claude Desktop ✓
- `apexe serve --transport http --port 8000` exposes MCP endpoint ✓
- 62 tests passing ✓

---

### F5: Governance Defaults
**Priority:** P1 | **Status:** DONE | **Actual LOC:** 1,492

1. **Annotation inference engine:**
   - Destructive patterns: delete, remove, rm, drop, kill, destroy, purge, wipe, erase, force-push
   - Readonly patterns: list, show, status, info, get, cat, ls, describe, inspect, view, display, print, find, search, help, version
   - Flag boosting: `--force`/`--hard` → requires_approval, `--dry-run` → idempotent
   - Confidence scoring [0.3, 0.95]

2. **ACL generation:**
   - Default-deny model
   - Readonly → auto-allow, destructive → deny (needs explicit allow), unknown → deny
   - Wildcard pattern matching (`*`, `cli.*`, `cli.git.*`)
   - YAML output compatible with apcore ACL format

3. **Audit trail:**
   - JSONL append-only logging to `~/.apexe/audit.jsonl`
   - SHA-256 input hashing (privacy: raw inputs not logged)
   - Error-resilient (never causes execution failure)
   - Log rotation with configurable size threshold

**Verified:**
- `git push` → destructive, `git status` → readonly ✓
- ACL generated and loaded ✓
- Audit entries written with trace_id, inputs_hash, duration_ms ✓
- 68 tests passing ✓

---

## Actual Scope

| Feature | Estimated LOC | Actual LOC | Tests |
|---------|--------------|------------|-------|
| F1: Skeleton | ~700 | 1,424 | 61 |
| F2: Scanner | ~2,500 | 3,537 | 138 |
| F3: Binding Gen | ~1,000 | 993 | 63 |
| F4: Serve | ~400 | 1,544 | 62 |
| F5: Governance | ~900 | 1,492 | 68 |
| **Total** | **~5,500** | **9,543** | **393** |

LOC exceeded estimate by ~73%, primarily because F4 required a self-built MCP implementation (apcore-mcp-rust not yet available).

## Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `clap` (derive mode) | CLI argument parsing |
| `serde` + `serde_json` + `serde_yaml` | Serialization/deserialization |
| `tokio` | Async runtime (serve) |
| `axum` | HTTP server (serve) |
| `tracing` + `tracing-subscriber` | Structured logging |
| `thiserror` | Typed error definitions |
| `anyhow` | Application-level error propagation |
| `nom` | Parser combinators for help text parsing |
| `regex` | Pattern matching for help format detection |
| `sha2` | SHA-256 hashing for audit privacy |
| `uuid` | UUID v4 generation for trace IDs |
| `chrono` | Timestamps for audit entries |
| `dirs` | Platform-specific home directory resolution |
| `which` | Binary path resolution on $PATH |
| `shell-words` | Shell argument splitting |
| `tempfile` | (dev) Temporary directories |
| `assert_cmd` | (dev) CLI integration testing |
| `rstest` | (dev) Parameterized test cases |
| `predicates` | (dev) Assertion helpers |

## Open Items

1. **A2A protocol** -- Stub only. Implement when apcore-a2a-rust is available or self-build.
2. **Agent Card** -- `/.well-known/agent.json` endpoint not yet implemented.
3. **SSE transport** -- Stub endpoint, no streaming implementation.
4. **apcore-mcp-rust migration** -- When published, replace self-built MCP with apcore-mcp-rust.
5. **`apexe evo`** -- Deferred. Depends on apevo product maturity.
