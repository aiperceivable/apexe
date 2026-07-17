# apexe User Manual

| Field | Value |
|-------|-------|
| **Version** | 0.2.0 (unreleased) |
| **Date** | 2026-07-16 |
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
11. [A2A Server](#11-a2a-server)
12. [Integrating with AI Agents](#12-integrating-with-ai-agents)
13. [Error Handling & AI Guidance](#13-error-handling--ai-guidance)
14. [File Locations](#14-file-locations)
15. [Logging & Debugging](#15-logging--debugging)
16. [Troubleshooting](#16-troubleshooting)

---

## 1. Introduction

**apexe** turns any CLI tool on your system into a governed, schema-enforced service that AI agents can invoke safely via the MCP protocol. It works in three steps:

1. **Scan** — Deterministically extract commands, flags, and arguments from CLI tools (no LLM required).
2. **Govern** — Classify commands as readonly/destructive, generate ACL rules, enable audit logging.
3. **Serve** — Expose tools via MCP (stdio for Claude Desktop/Cursor, HTTP for remote agents).

apexe is built on the [apcore](https://github.com/aiperceivable/apcore-rust) ecosystem: apcore (core types), apcore-toolkit (output), apcore-mcp (MCP server), apcore-a2a (A2A agent server), apcore-cli (audit logging).

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
| `--skills-dir <DIR>` | - | Also write a Claude Skill (`SKILL.md`) per module under `<DIR>/.claude/skills/<module_id>/` |

```bash
apexe scan git                         # basic scan
apexe scan ls jq curl                  # multiple tools
apexe scan git --depth 3               # deeper subcommand discovery
apexe scan git --no-cache              # force re-scan
apexe scan git --format json           # JSON output
apexe scan git --skills-dir ./out      # also write .claude/skills/cli.git.*/SKILL.md
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
| `--no-circuit-breaker` | off | Disable CircuitBreakerMiddleware (on by default) |
| `--no-retry` | off | Disable RetryMiddleware (on by default; only ever retries idempotent timeouts) |
| `--metrics` | off | Enable `/metrics` (Prometheus) + `/usage` (JSON) — HTTP/SSE only |

```bash
apexe serve                                        # stdio (Claude Desktop/Cursor)
apexe serve --transport http --port 8000            # HTTP server
apexe serve --transport http --explorer             # HTTP + browser UI
apexe serve --show-config claude-desktop            # print integration config
apexe serve --transport http --metrics               # + /metrics and /usage
apexe serve --no-circuit-breaker --no-retry           # disable resilience middleware
```

### 4.3 `apexe a2a`

Starts an A2A agent server exposing scanned tools, sharing governance (ACL, logging, approval) with `apexe serve` via the same `Executor`.

```
apexe a2a [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--url <URL>` | `http://127.0.0.1:8000` | Base URL to bind the A2A server to |
| `--modules-dir <DIR>` | `~/.apexe/modules/` | Directory containing binding files |
| `--name <NAME>` | `apexe` | A2A agent name |
| `--explorer` | off | Enable browser-based Explorer UI |
| `--acl <PATH>` | - | Path to ACL policy YAML file |
| `--no-logging` | off | Disable structured logging middleware |
| `--no-circuit-breaker` | off | Disable CircuitBreakerMiddleware (on by default) |
| `--no-retry` | off | Disable RetryMiddleware (on by default; only ever retries idempotent timeouts) |
| `--execution-timeout <SECS>` | `300` | Per-task execution timeout in seconds |
| `--cors-origin <ORIGIN>` | - | Allowed CORS origin (repeatable) |

> **No `--enable-approval` on `apexe a2a`.** A2A has no interactive elicitation
> transport, so an approval prompt can never be resolved over it; the flag would
> only ever error. Approval on A2A is a library-only feature — construct
> `A2aServerBuilder` with an `ApprovalStore`. `apexe serve` keeps `--enable-approval`.

```bash
apexe a2a                                       # http://127.0.0.1:8000
apexe a2a --url http://0.0.0.0:9000 --explorer  # custom bind address + browser UI
apexe a2a --acl ~/.apexe/acl.yaml               # governed by an ACL policy
```

### 4.4 `apexe list`

Lists all registered modules from binding files.

```
apexe list [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--format <FMT>` | `table` | Output format: `table` or `json` |
| `--modules-dir <DIR>` | `~/.apexe/modules/` | Directory to read binding files from |

### 4.5 `apexe config`

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

### 9.3 Subprocess Isolation (always-on)

Every `CliModule` call runs through `execute_subprocess` (`src/module/executor.rs`), which applies isolation **unconditionally** — there is no `--sandbox` toggle and no "unsandboxed" mode. (This is not `apcore-cli`'s `Sandbox`, which expects the host binary to re-exec itself with an `--internal-sandbox-runner` subcommand and rediscover modules from `APCORE_EXTENSIONS_ROOT` — a model apexe's runtime-scanned CLI modules don't fit.)

- **Environment scrubbing**: the subprocess does **not** inherit apexe's full environment. The env is cleared and only a base allowlist is passed through (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TZ`, `TMPDIR`), so secrets in apexe's environment (API tokens, cloud credentials) can't leak to — or be surfaced by — a wrapped tool. File-based credentials under `$HOME` still work. (Per-tool credential-env passthrough is planned as an opt-in config knob.)
- **No shell**: arguments are passed as direct argv (no shell interpolation), and client-supplied string values containing shell metacharacters are rejected before execution.
- **Output cap**: stdout/stderr are each capped at 64 MiB (`executor::DEFAULT_MAX_OUTPUT_BYTES`). A command that exceeds it gets `stdout_truncated`/`stderr_truncated: true` in the result instead of exhausting memory.
- **Timeout kills the process**: the child is spawned with `kill_on_drop(true)`; when `--timeout`/`default_timeout` elapses, the subprocess is actually terminated rather than left running as an orphan.
- **stdin** is connected to `/dev/null`, so a tool that waits for input fails fast instead of hanging.

Stronger OS-level sandboxing (seccomp/landlock, namespaces, cgroup limits) is tracked as roadmap; v0.x relies on the governance stack (ACL + approval + preview) plus the process isolation above.

### 9.4 Preview (Dry-Run Prediction)

Destructive modules (`annotations.destructive == true`) implement apcore's `Module::preview()` hook. It does not attempt to predict the actual side effects of an arbitrary CLI binary (`apexe` has no way to know what `git push --force` will do to a remote) — it only surfaces the exact resolved command line, so an approver can see what they're about to allow. Readonly/non-destructive modules return no preview (nothing to predict). Reachable via apcore-mcp's `__apcore_module_preview` meta-tool.

### 9.5 Resilience Middleware

`build_executor` wires two middleware on by default (`--no-circuit-breaker`/`--no-retry` to disable, on both `apexe serve` and `apexe a2a`):

- **`CircuitBreakerMiddleware`** — short-circuits calls to a `(module_id, caller)` pair after repeated failures (default: ≥5 samples, ≥50% error rate), instead of letting every caller keep hammering a hanging or broken CLI tool. Auto-recovers after a cooldown window via a single probe call.
- **`RetryMiddleware`** — retries a call after a transient failure, but *only* when the error is explicitly marked `retryable`. `CliModule` marks a timeout `retryable` **only when the module is annotated `idempotent`** (§8) — a killed non-idempotent command (e.g. `rm -rf` timing out mid-delete) is never auto-retried, since it may have partially applied its side effect.

### 9.6 Pluggable Approval Store (Library Only)

By default, `--enable-approval` uses apcore-mcp's `ElicitationApprovalHandler`: a call to a `requires_approval` module blocks until the connected MCP client's user responds to an elicitation prompt. For approval flows where the approver isn't in the MCP session (e.g. a Slack bot), `apexe`'s `ExecutorOptions`/`McpServerBuilder`/`A2aServerBuilder` accept an `Arc<dyn apcore_mcp::ApprovalStore>` — when set, approvals become non-blocking (`StorageBackedApprovalHandler`): the call returns an `ApprovalPending` error immediately, and a separate mechanism you build resolves the decision later via `ApprovalStore::resolve`.

There is no CLI flag for this — `apcore-mcp`'s `InMemoryApprovalStore` is documented as unsuitable for production (state isn't shared across process invocations, so a hypothetical `apexe approval resolve` command couldn't reach a running server's store anyway). Embed apexe as a library and supply your own persistent store (Redis, a database, etc.) to use this for real.

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
| **CircuitBreakerMiddleware** | Enabled by default (`--no-circuit-breaker`) | Short-circuits a hanging/broken tool — see §9.5 |
| **RetryMiddleware** | Enabled by default (`--no-retry`) | Retries idempotent timeouts only — see §9.5 |
| **ElicitationApprovalHandler** | Opt-in (`--enable-approval`) | Sends approval request to MCP client for destructive commands |

### Observability

`apexe serve --transport http --metrics` enables two endpoints (HTTP/SSE only, ignored on stdio):

| Endpoint | Format | Content |
|----------|--------|---------|
| `/metrics` | Prometheus text | `apcore_module_calls_total`, `apcore_module_duration_seconds` histogram, per `module_id` |
| `/usage` | JSON | Per-module call count, error count, average latency, unique callers, trend |

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

## 11. A2A Server

`apexe a2a` exposes the same scanned modules as an A2A agent via
[apcore-a2a](https://github.com/aiperceivable/apcore-a2a-rust), instead of
(or alongside) MCP. It shares governance with `apexe serve` — both build
their `Executor` through the same `apexe::module::build_executor()`, so an
`--acl` policy, the logging middleware, the audit trail, and the always-on
subprocess isolation behave identically regardless of which transport a caller
uses. Human-in-the-loop **approval** is the one exception: `apexe serve` offers
`--enable-approval` (MCP elicitation), but A2A has no elicitation transport, so
approval on A2A is library-only (supply an `ApprovalStore` to `A2aServerBuilder`).

```bash
apexe a2a                                       # http://127.0.0.1:8000
apexe a2a --url http://0.0.0.0:9000 --explorer  # custom bind address + browser UI
apexe a2a --acl ~/.apexe/acl.yaml               # governed by an ACL policy
```

### Endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /.well-known/agent-card.json` | A2A Agent Card (skills derived from registered modules) |
| `GET /health` | Liveness check |
| `GET /explorer` | Browser-based Explorer UI (with `--explorer`) |

### Skill aliasing

Like MCP, A2A reads the `display` overlay `apexe scan` computes
(`metadata["display"]["a2a"]["alias"]`) to name each skill; see
[Display Names](#display-names) below.

### Current limitation: no per-caller identity over the wire

apcore's ACL engine evaluates whatever roles a caller's `Context::identity`
carries, but `apexe a2a` (and `apexe serve`) do not yet populate
`Identity.roles` from a JWT claim or request header — every call served
today runs as a single implicit anonymous caller. Role-gated ACL rules
(`conditions: {roles: [...]}`) are therefore not yet reachable over MCP/A2A;
`--acl` is most useful today for apexe's readonly-allow / destructive-deny
default policy (§9.1) and the `--enable-approval` human-in-the-loop gate.
See [`examples/acl_demo`](../examples/acl_demo/) for a library-level
demonstration of the same role-based ACL contract, driven directly against
the `Executor`.

---

## 12. Integrating with AI Agents

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

## 13. Error Handling & AI Guidance

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

## 14. File Locations

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

## 15. Logging & Debugging

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

## 16. Troubleshooting

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

- **A2A per-caller identity**: `apexe a2a` (and `apexe serve`) do not yet populate `Identity.roles` from a JWT/header, so role-gated ACL rules are unreachable over the wire — see [§11](#11-a2a-server).
- **Windows**: Not supported.
- **Interactive CLI tools**: Tools requiring stdin input (e.g., `ssh`, `vim`) cannot be wrapped.
- **Streaming output**: CLI subprocess output is collected in full, then returned. No real-time streaming.
