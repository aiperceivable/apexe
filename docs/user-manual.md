# apexe User Manual

| Field | Value |
|-------|-------|
| **Version** | 0.1.0 |
| **Date** | 2026-03-28 |
| **Platform** | macOS / Linux |

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Installation](#2-installation)
3. [Quick Start](#3-quick-start)
4. [Commands Reference](#4-commands-reference)
5. [Configuration](#5-configuration)
6. [Scanning Engine](#6-scanning-engine)
7. [Schema Generation](#7-schema-generation)
8. [Behavioral Annotations](#8-behavioral-annotations)
9. [Governance](#9-governance)
10. [MCP Server](#10-mcp-server)
11. [Integrating with AI Agents](#11-integrating-with-ai-agents)
12. [Error Handling & AI Guidance](#12-error-handling--ai-guidance)
13. [File Locations](#13-file-locations)
14. [Logging & Debugging](#14-logging--debugging)
15. [Troubleshooting](#15-troubleshooting)

---

## 1. Introduction

**apexe** turns any CLI tool on your system into a governed, schema-enforced service that AI agents can invoke safely via the MCP protocol. It works in three steps:

1. **Scan** — Deterministically extract commands, flags, and arguments from CLI tools (no LLM required).
2. **Govern** — Classify commands as readonly/destructive, generate ACL rules, enable audit logging.
3. **Serve** — Expose tools via MCP (stdio for Claude Desktop/Cursor, HTTP for remote agents).

apexe is built on the [apcore](https://github.com/aiperceivable/apcore-rust) ecosystem: apcore (core types), apcore-toolkit (output), apcore-mcp (server), apcore-cli (audit/sandbox).

---

## 2. Installation

### Prerequisites

- **Rust** 1.75 or later (uses async fn in traits)
- **Cargo** (included with Rust)
- macOS or Linux

### Install from source

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

---

## 3. Quick Start

See [Quick Start Guide](quickstart.md) for the fastest path to a working setup.

```bash
apexe scan git curl grep         # scan tools
apexe list                       # verify modules
apexe serve                      # start MCP server (stdio)
```

---

## 4. Commands Reference

### 4.1 `apexe scan`

Scans one or more CLI tools and generates `.binding.yaml` files + ACL rules.

```
apexe scan <TOOLS>... [OPTIONS]
```

| Argument / Option | Default | Description |
|-------------------|---------|-------------|
| `<TOOLS>...` | (required) | CLI tool names to scan (must be on `$PATH`) |
| `--output-dir <DIR>` | `~/.apexe/modules/` | Directory to write binding files |
| `--depth <N>` | `2` | Subcommand recursion depth (1-5). `git remote add` = depth 2 |
| `--no-cache` | off | Force fresh scan, bypass cache |
| `--format <FMT>` | `table` | Output format: `json`, `yaml`, or `table` |

```bash
apexe scan git                         # basic scan
apexe scan ls jq curl                  # multiple tools
apexe scan git --depth 3               # deeper subcommand discovery
apexe scan git --no-cache              # force re-scan
apexe scan git --format json           # JSON output
```

### 4.2 `apexe serve`

Starts an MCP server exposing scanned tools to AI agents.

```
apexe serve [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--transport <TYPE>` | `stdio` | Transport: `stdio`, `http`, or `sse` |
| `--host <HOST>` | `127.0.0.1` | Host for HTTP/SSE transports |
| `--port <PORT>` | `8000` | Port for HTTP/SSE transports (1-65535) |
| `--explorer` | off | Enable browser-based Tool Explorer UI (HTTP only) |
| `--modules-dir <DIR>` | `~/.apexe/modules/` | Directory containing binding files |
| `--name <NAME>` | `apexe` | MCP server name |
| `--show-config <TARGET>` | - | Print config snippet: `claude-desktop` or `cursor` |

```bash
apexe serve                                        # stdio (Claude Desktop/Cursor)
apexe serve --transport http --port 8000            # HTTP server
apexe serve --transport http --explorer             # HTTP + browser UI
apexe serve --show-config claude-desktop            # print integration config
```

### 4.3 `apexe list`

Lists all registered modules from binding files.

```
apexe list [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--format <FMT>` | `table` | Output format: `table` or `json` |
| `--modules-dir <DIR>` | `~/.apexe/modules/` | Directory to read binding files from |

### 4.4 `apexe config`

Shows or initializes apexe configuration.

```
apexe config [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--show` | Print resolved configuration as YAML |
| `--init` | Create default config at `~/.apexe/config.yaml` |

---

## 5. Configuration

Configuration resolves in 4 tiers (highest priority wins):

```
CLI flags  >  Environment variables  >  Config file  >  Defaults
```

### Config file

Located at `~/.apexe/config.yaml`. Create with `apexe config --init`.

```yaml
modules_dir: ~/.apexe/modules
cache_dir: ~/.apexe/cache
audit_log: ~/.apexe/audit.jsonl
log_level: info
default_timeout: 30
scan_depth: 2
json_output_preference: true
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `modules_dir` | path | `~/.apexe/modules` | Binding file storage |
| `cache_dir` | path | `~/.apexe/cache` | Scan result cache |
| `audit_log` | path | `~/.apexe/audit.jsonl` | Audit trail file |
| `log_level` | string | `info` | Log level: error, warn, info, debug, trace |
| `default_timeout` | integer | `30` | CLI subprocess timeout (seconds) |
| `scan_depth` | integer | `2` | Default subcommand recursion depth |
| `json_output_preference` | boolean | `true` | Prefer JSON output from CLI tools when available |

### Environment variables

| Variable | Overrides | Example |
|----------|-----------|---------|
| `APEXE_MODULES_DIR` | `modules_dir` | `/opt/apexe/modules` |
| `APEXE_CACHE_DIR` | `cache_dir` | `/tmp/apexe-cache` |
| `APEXE_LOG_LEVEL` | `log_level` | `debug` |
| `APEXE_TIMEOUT` | `default_timeout` | `120` |
| `APEXE_SCAN_DEPTH` | `scan_depth` | `3` |

---

## 6. Scanning Engine

apexe uses a three-tier deterministic scanning engine. No LLM is involved.

### Tier 1: `--help` Parsing

Runs `<tool> --help` and auto-detects the help format. Four built-in parsers:

| Parser | Detects | Examples |
|--------|---------|---------|
| **GNU** | Standard GNU-style help | ls, grep, curl, git |
| **Click** | Python Click / argparse | aws, pip |
| **Cobra** | Go Cobra framework | kubectl, docker, gh |
| **Clap** | Rust Clap framework | ripgrep, fd, bat |

Extracts: subcommands, flags (long/short), positional args, types, defaults, enum values, descriptions.

### Tier 2: Man Page Enrichment

Parses `man <tool>` output to supplement Tier 1:

- **DESCRIPTION section**: Enriches commands that have sparse descriptions (< 20 chars).
- **OPTIONS section**: Extracts flag descriptions and merges into flags that have sparse descriptions (< 10 chars) from Tier 1.

### Tier 3: Shell Completion Discovery

Parses zsh/bash completion scripts from standard paths:
- `/usr/share/zsh/functions/Completion/_<tool>`
- `/usr/local/share/zsh/site-functions/_<tool>`
- `/etc/bash_completion.d/<tool>`

Discovers subcommands that Tier 1 missed and merges them into the result (added as stubs with a warning).

### Subcommand Discovery

For tools with subcommands, apexe recursively runs `--help` on each subcommand up to `--depth` levels. For example, with `--depth 2`:

```
git --help          → discovers: commit, push, remote, ...
git remote --help   → discovers: add, remove, show, ...
```

### Caching

Scan results are cached in `~/.apexe/cache/`. Cache is keyed by tool name + version. Use `--no-cache` to force a fresh scan.

---

## 7. Schema Generation

Each scanned flag/argument becomes a JSON Schema property.

### Type Mapping

| CLI Type | JSON Schema | Example |
|----------|-------------|---------|
| String | `"type": "string"` | `--message "hello"` |
| Integer | `"type": "integer"` | `--count 5` |
| Float | `"type": "number"` | `--ratio 0.5` |
| Boolean | `"type": "boolean"` | `--verbose` |
| Path | `"type": "string", "format": "path"` | `--config /etc/app.yaml` |
| URL | `"type": "string", "format": "uri"` | `--url https://...` |
| Enum | `"type": "string", "enum": [...]` | `--format json\|yaml\|table` |

### Special handling

- **Required flags**: Added to the schema's `required` array.
- **Repeatable flags** (`--include a --include b`): Wrapped as `"type": "array", "items": {...}`.
- **Default values**: Included with type-correct coercion (`"10"` becomes `10` for integers).
- **Boolean defaults**: `false` unless explicitly set.
- **Format hints**: `Path` and `URL` types emit `"format"` so AI agents can distinguish paths from plain strings.

### Output Schema

Tools with detected JSON output flags get an enhanced output schema:

```json
{
  "type": "object",
  "properties": {
    "stdout": { "type": "string" },
    "stderr": { "type": "string" },
    "exit_code": { "type": "integer" },
    "json_output": { "type": "object" }
  }
}
```

---

## 8. Behavioral Annotations

apexe automatically infers behavioral annotations from command names and flags.

### Command Name Patterns

| Annotation | Trigger Patterns |
|------------|-----------------|
| **readonly** | list, ls, show, get, status, info, version, help, describe, view, cat, log, diff, search, find, check, inspect, display, print, whoami, env, top, ps |
| **destructive** + **requires_approval** | delete, rm, remove, destroy, purge, drop, kill, prune, clean, reset, format, wipe, erase |
| **idempotent** | get, list, show, status, info, describe, version, help, check |
| **cacheable** | (readonly AND idempotent) |

### Flag Boosting

Certain flags escalate the annotation regardless of command name:

| Flags | Effect |
|-------|--------|
| `--force`, `-f`, `--hard`, `--recursive`, `-r`, `--all`, `--prune`, `--no-preserve-root`, `--cascade`, `--purge`, `--yes`, `-y` | `requires_approval = true` |
| `--dry-run`, `--check`, `--diff`, `--noop`, `--simulate`, `--whatif`, `--plan` | `idempotent = true` |

**Example**: `git push` has flag `--force`, so it gets `requires_approval = true` even though "push" is not in the destructive list.

---

## 9. Governance

### 9.1 Access Control (ACL)

`apexe scan` automatically generates `~/.apexe/acl.yaml` using a **default-deny** model:

| Module type | Default rule |
|-------------|-------------|
| Readonly modules | `effect: allow` |
| Destructive modules | `effect: deny` with `require_approval: true` |
| All others | Default deny (no explicit rule) |

ACL format (editable):

```yaml
default_effect: deny
rules:
  - callers: ["*"]
    targets: ["cli.git.status", "cli.git.log", "cli.git.diff"]
    effect: allow
    description: "Auto-allow readonly git commands"
  - callers: ["*"]
    targets: ["cli.git.push"]
    effect: deny
    description: "Block destructive git commands"
    conditions:
      require_approval: true
```

### 9.2 Audit Trail

Every tool invocation via `apexe serve` is logged to `~/.apexe/audit.jsonl`:

```json
{
  "timestamp": "2026-03-28T10:30:00.123Z",
  "user": "tercelyi",
  "module_id": "cli.git.commit",
  "input_hash": "a3f2b8...",
  "status": "success",
  "exit_code": 0,
  "duration_ms": 42
}
```

- **Privacy**: Inputs are SHA-256 hashed with a random salt. Raw input values are never logged.
- **Resilience**: Audit logging never causes execution failures. Write errors are silently logged via tracing.

### 9.3 Sandbox (Optional)

The `SandboxManager` wraps `apcore-cli`'s subprocess isolation with environment variable whitelisting and timeout enforcement. Available programmatically via the library API.

---

## 10. MCP Server

### Transport Options

| Transport | Use case | Command |
|-----------|----------|---------|
| **stdio** | Claude Desktop, Cursor (default) | `apexe serve` |
| **streamable-http** | Remote agents, browser UI | `apexe serve --transport http --port 8000` |
| **sse** | Server-Sent Events transport | `apexe serve --transport sse --port 8000` |

### Built-in Middleware

| Middleware | Status | Effect |
|-----------|--------|--------|
| **LoggingMiddleware** | Enabled by default | Structured logging of inputs/outputs with sensitive field redaction |
| **ElicitationApprovalHandler** | Opt-in (programmatic) | Sends approval request to MCP client for destructive commands |

### Tool Filtering

The `McpServerBuilder` API supports filtering which tools are exposed:

```rust
McpServerBuilder::new()
    .tags(vec!["readonly".to_string()])     // only expose readonly tools
    .prefix("cli.git")                       // only expose git tools
    .build()?;
```

### Explorer UI

Enable with `--explorer` (HTTP transport only):

```bash
apexe serve --transport http --port 8000 --explorer
```

Provides a browser-based interface to explore available tools, view schemas, and test invocations.

### OpenAI Tools Export

Export tool definitions in OpenAI function calling format (programmatic API):

```rust
let tools = McpServerBuilder::new()
    .modules_dir("~/.apexe/modules")
    .export_openai_tools()?;
```

---

## 11. Integrating with AI Agents

### Claude Desktop

```bash
apexe scan git curl grep
apexe serve --show-config claude-desktop
```

Copy the JSON output into:
- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Linux**: `~/.config/claude/claude_desktop_config.json`

Restart Claude Desktop. Scanned tools appear as MCP tools.

### Cursor

```bash
apexe serve --show-config cursor
```

Add the JSON to Cursor's MCP settings.

### HTTP Mode (Remote Agents)

```bash
apexe serve --transport http --host 0.0.0.0 --port 8000
```

The MCP endpoint is at `POST /mcp`.

### Display Names

apexe generates display metadata for MCP clients:

| Module ID | MCP Display Alias |
|-----------|------------------|
| `cli.git.commit` | `git_commit` |
| `cli.docker.container.ls` | `docker_container_ls` |
| `cli.curl` | `curl` |

Aliases are auto-sanitized for MCP compatibility (dots replaced with underscores, digit prefixes escaped).

---

## 12. Error Handling & AI Guidance

Every error includes `ai_guidance` to help AI agents self-correct:

| Error | ai_guidance |
|-------|-------------|
| Tool not found | "The tool 'xyz' is not installed. Install it and try again." |
| Command timeout | "The command took too long. Try with simpler arguments or increase timeout." |
| Shell injection detected | "Remove shell metacharacters (;, \|) from parameter 'file'." |
| Permission denied | "Permission denied. Check file permissions or run with appropriate privileges." |
| Non-zero exit code | "Command 'git push' exited with code 1. stderr: (first 200 chars)" |

Additionally, each execution response includes:
- `trace_id` for end-to-end correlation
- `duration_ms` for performance tracking
- `exit_code` for programmatic error detection

---

## 13. File Locations

| Path | Purpose | Created by |
|------|---------|------------|
| `~/.apexe/config.yaml` | Configuration | `apexe config --init` |
| `~/.apexe/modules/*.binding.yaml` | Tool binding files | `apexe scan` |
| `~/.apexe/cache/` | Scan result cache | `apexe scan` |
| `~/.apexe/acl.yaml` | Access control rules | `apexe scan` |
| `~/.apexe/audit.jsonl` | Audit trail | `apexe serve` (runtime) |
| `~/.apexe/apcore.yaml` | apcore ecosystem config (optional) | Manual |

All directories are created automatically on first use.

---

## 14. Logging & Debugging

apexe uses structured logging via the `tracing` crate.

```bash
# Via CLI flag (global)
apexe --log-level debug scan git

# Via environment variable
RUST_LOG=debug apexe scan git

# Via config file
# log_level: debug
```

| Level | Shows |
|-------|-------|
| `error` | Failures only |
| `warn` | Warnings (e.g., failed to write ACL, cache miss) |
| `info` | Normal operation: tool loaded, modules registered, server started |
| `debug` | Internal detail: parser selection, cache hits, enrichment decisions |
| `trace` | Very verbose: raw help text, parsed structures |

---

## 15. Troubleshooting

### "Tool not found" during scan

The tool must be on `$PATH`:
```bash
which <tool>
```

### Scan produces incomplete results

1. Increase depth: `apexe scan <tool> --depth 3`
2. Force re-scan: `apexe scan <tool> --no-cache`
3. Check parser selection: `RUST_LOG=debug apexe scan <tool>`

### Serve command does nothing (stdio mode)

Stdio mode reads JSON-RPC from stdin and writes to stdout. It is launched by AI agents, not run interactively. Use `--show-config` to get the agent integration snippet.

### Tool invocation fails with ACL denied

The default ACL denies destructive and unknown commands. Edit `~/.apexe/acl.yaml`:

```yaml
rules:
  - callers: ["*"]
    targets: ["cli.<tool>.<command>"]
    effect: allow
```

### Stale scan results

```bash
apexe scan <tool> --no-cache
# Or clear cache entirely:
rm -rf ~/.apexe/cache/
```

### Known Limitations

- **A2A protocol**: Not yet implemented. Architecture supports future addition (~150 LOC).
- **Windows**: Not supported.
- **Interactive CLI tools**: Tools requiring stdin input (e.g., `ssh`, `vim`) cannot be wrapped.
- **Streaming output**: CLI subprocess output is collected in full, then returned. No real-time streaming.
