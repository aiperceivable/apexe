# apexe User Manual

| Field | Value |
|-------|-------|
| **Version** | 0.1.0 |
| **Date** | 2026-03-24 |
| **Platform** | macOS / Linux |

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Installation](#2-installation)
3. [Quick Start](#3-quick-start)
4. [Commands](#4-commands)
   - [scan](#41-apexe-scan)
   - [list](#42-apexe-list)
   - [serve](#43-apexe-serve)
   - [config](#44-apexe-config)
5. [Configuration](#5-configuration)
6. [How Scanning Works](#6-how-scanning-works)
7. [Governance](#7-governance)
8. [Integrating with AI Agents](#8-integrating-with-ai-agents)
9. [File Locations](#9-file-locations)
10. [Logging & Debugging](#10-logging--debugging)
11. [Troubleshooting](#11-troubleshooting)

---

## 1. Introduction

**apexe** is a CLI tool that turns any command-line program on your system into a governed, schema-enforced tool that AI agents can invoke safely. It works in three steps:

1. **Scan** a CLI tool to extract its commands, flags, and arguments.
2. **Generate** binding files that describe the tool in a machine-readable format.
3. **Serve** the bindings over the MCP protocol so AI agents (Claude Desktop, Cursor, etc.) can use them.

apexe also adds governance: it classifies commands as readonly or destructive, generates access control rules, and maintains an audit trail of every invocation.

---

## 2. Installation

### Prerequisites

- **Rust** 1.70 or later
- **Cargo** (included with Rust)
- macOS or Linux (Windows is not supported in v0.1)

### Install from source

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
```

Verify the installation:

```bash
apexe --help
```

---

## 3. Quick Start

Scan a tool, list the results, and start serving — all in three commands:

```bash
# Scan git
apexe scan git

# Check what was generated
apexe list

# Start an MCP server for Claude Desktop
apexe serve --transport stdio
```

That's it. Your AI agent can now invoke git commands through a schema-enforced, governed interface.

---

## 4. Commands

### 4.1 `apexe scan`

Scans one or more CLI tools and generates `.binding.yaml` files.

```
apexe scan <TOOL> [<TOOL>...] [OPTIONS]
```

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<TOOL>` | Name of the CLI tool(s) to scan (must be on your `$PATH`) |

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--depth <N>` | `2` | How many levels of subcommands to recurse (max: 5). For example, `git remote add` is depth 2. |
| `--no-cache` | off | Force a fresh scan, ignoring any cached results. |
| `--format <FMT>` | `yaml` | Output format: `yaml`, `json`, or `table`. |
| `--output-dir <DIR>` | `~/.apexe/modules/` | Directory to write binding files to. |

**Examples:**

```bash
# Scan a single tool
apexe scan docker

# Scan multiple tools at once
apexe scan git docker ffmpeg kubectl

# Deep scan with 3 levels of subcommands
apexe scan git --depth 3

# Force re-scan (bypass cache)
apexe scan git --no-cache

# Write bindings to a custom directory
apexe scan git --output-dir ./my-bindings

# Output scan results as JSON
apexe scan git --format json
```

### 4.2 `apexe list`

Lists all previously scanned tools and their generated modules.

```
apexe list [OPTIONS]
```

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--format <FMT>` | `table` | Output format: `table` or `json`. |

**Examples:**

```bash
# List tools in a table
apexe list

# List as JSON (useful for scripting)
apexe list --format json
```

### 4.3 `apexe serve`

Starts an MCP server that exposes scanned tools to AI agents.

```
apexe serve [OPTIONS]
```

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--transport <TYPE>` | `stdio` | Transport protocol: `stdio` or `http`. |
| `--port <PORT>` | `3000` | Port for HTTP transport. |
| `--name <NAME>` | `apexe` | Server name reported in MCP `initialize` response. |
| `--show-config <TARGET>` | — | Print integration config snippet instead of starting the server. Targets: `claude-desktop`, `cursor`. |

**Examples:**

```bash
# Start stdio server (for Claude Desktop / Cursor)
apexe serve

# Start HTTP server on port 8000
apexe serve --transport http --port 8000

# Print Claude Desktop integration config
apexe serve --show-config claude-desktop

# Print Cursor integration config
apexe serve --show-config cursor

# Custom server name
apexe serve --name my-dev-tools
```

### 4.4 `apexe config`

Shows or initializes the apexe configuration.

```
apexe config [OPTIONS]
```

**Options:**

| Option | Description |
|--------|-------------|
| `--show` | Print the current resolved configuration as YAML. |
| `--init` | Create a default config file at `~/.apexe/config.yaml`. |

**Examples:**

```bash
# View current config
apexe config --show

# Create default config file
apexe config --init
```

---

## 5. Configuration

apexe resolves configuration from three tiers. Higher tiers override lower ones:

```
CLI flags  >  Environment variables  >  Config file (~/.apexe/config.yaml)
```

### Config file

Located at `~/.apexe/config.yaml`. Create it with `apexe config --init`.

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
| `modules_dir` | path | `~/.apexe/modules` | Where binding files are stored. |
| `cache_dir` | path | `~/.apexe/cache` | Where scan cache is stored. |
| `audit_log` | path | `~/.apexe/audit.jsonl` | Path to the audit trail file. |
| `log_level` | string | `info` | Log level: `error`, `warn`, `info`, `debug`, `trace`. |
| `default_timeout` | integer | `30` | Default timeout in seconds for CLI subprocess execution. |
| `scan_depth` | integer | `2` | Default subcommand recursion depth for `apexe scan`. |
| `json_output_preference` | boolean | `true` | Prefer JSON output from scanned tools when available. |

### Environment variables

| Variable | Overrides |
|----------|-----------|
| `APEXE_MODULES_DIR` | `modules_dir` |
| `APEXE_CACHE_DIR` | `cache_dir` |
| `APEXE_LOG_LEVEL` | `log_level` |
| `APEXE_TIMEOUT` | `default_timeout` |

---

## 6. How Scanning Works

When you run `apexe scan <tool>`, the scanner engine goes through three tiers in order:

### Tier 1 — Help text parsing

Runs `<tool> --help` (or `<tool> -h`) and parses the output. Four built-in parsers handle the most common CLI frameworks:

| Parser | Detects |
|--------|---------|
| **GNU** | Standard GNU-style help (most Linux tools) |
| **Click** | Python Click / argparse output |
| **Cobra** | Go Cobra-based CLIs (kubectl, docker, gh) |
| **Clap** | Rust Clap-based CLIs |

The parser pipeline automatically selects the best-matching parser based on the help text format.

### Tier 2 — Man page parsing

If Tier 1 yields incomplete results, apexe falls back to parsing `man <tool>` output for additional descriptions.

### Tier 3 — Shell completion parsing

As a final fallback, apexe parses zsh/bash completion scripts to discover subcommands that may not appear in `--help`.

### Subcommand discovery

For tools with subcommands (e.g., `git`, `docker`), apexe recursively discovers subcommands up to the configured depth. For example, with `--depth 2`, scanning `git` discovers both `git commit` and `git remote add`.

### Caching

Scan results are cached in `~/.apexe/cache/`. Subsequent scans of the same tool are instant unless:
- You pass `--no-cache`
- The tool's binary has changed (version detection)

---

## 7. Governance

apexe automatically applies governance to every scanned tool. This happens during the `scan` phase — no extra steps required.

### 7.1 Annotation inference

Each command is classified based on its name and flags:

| Classification | Trigger patterns | Example |
|----------------|-----------------|---------|
| **Destructive** | `delete`, `remove`, `rm`, `drop`, `kill`, `destroy`, `purge`, `wipe`, `erase`, `force-push` | `git push --force` |
| **Readonly** | `list`, `show`, `status`, `info`, `get`, `cat`, `ls`, `describe`, `inspect`, `view`, `help` | `git status` |

Certain flags modify the classification:

| Flag | Effect |
|------|--------|
| `--force`, `--hard` | Escalates to `requires_approval` |
| `--dry-run` | Marks as `idempotent` |

Each annotation carries a confidence score (0.3–0.95).

### 7.2 Access control (ACL)

apexe generates an `acl.yaml` file at `~/.apexe/acl.yaml` using a default-deny model:

| Command type | Default ACL |
|-------------|-------------|
| Readonly | `allow` |
| Destructive | `deny` (requires explicit allow) |
| Unknown | `deny` |

You can edit `acl.yaml` to customize permissions. The ACL supports wildcard patterns:

```yaml
# Allow all git read commands
- pattern: "cli.git.status"
  action: allow

# Allow all docker commands
- pattern: "cli.docker.*"
  action: allow

# Deny all destructive git operations
- pattern: "cli.git.push"
  action: deny
```

### 7.3 Audit trail

Every tool invocation through `apexe serve` is logged to `~/.apexe/audit.jsonl` in append-only JSONL format. Each entry includes:

- **Timestamp** — when the invocation occurred
- **Trace ID** — UUID for correlation
- **Module ID** — which tool/command was invoked (e.g., `cli.git.commit`)
- **Inputs hash** — SHA-256 hash of the inputs (raw inputs are not logged for privacy)
- **Duration** — execution time in milliseconds
- **Result status** — success or error

The audit trail never causes execution failures — logging errors are silently ignored. Log rotation is supported with configurable size thresholds.

---

## 8. Integrating with AI Agents

### Claude Desktop

1. Scan the tools you want to expose:

   ```bash
   apexe scan git docker kubectl
   ```

2. Get the integration config:

   ```bash
   apexe serve --show-config claude-desktop
   ```

3. Copy the output into your Claude Desktop MCP configuration file (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS).

4. Restart Claude Desktop. Your tools will appear in the available MCP tools.

### Cursor

1. Scan and generate bindings as above.

2. Get the Cursor-specific config:

   ```bash
   apexe serve --show-config cursor
   ```

3. Add the config to Cursor's MCP settings.

### HTTP mode (remote agents)

For agents that connect via HTTP:

```bash
apexe serve --transport http --port 8000
```

The server exposes:
- `POST /mcp` — MCP JSON-RPC 2.0 endpoint
- `GET /health` — Health check

---

## 9. File Locations

| Path | Purpose |
|------|---------|
| `~/.apexe/config.yaml` | Configuration file |
| `~/.apexe/modules/` | Generated `.binding.yaml` files |
| `~/.apexe/modules/<tool>.binding.yaml` | Binding file for a specific tool |
| `~/.apexe/cache/` | Scan result cache |
| `~/.apexe/acl.yaml` | Access control rules |
| `~/.apexe/audit.jsonl` | Audit trail |

All directories are created automatically on first use.

---

## 10. Logging & Debugging

apexe uses structured logging via the `tracing` crate.

### Set log level

```bash
# Via environment variable
RUST_LOG=debug apexe scan git

# Via CLI flag
apexe --log-level debug scan git

# Via config file
# log_level: debug
```

### Log levels

| Level | What it shows |
|-------|--------------|
| `error` | Failures only |
| `warn` | Warnings and errors |
| `info` | Normal operation (default) |
| `debug` | Detailed internal state — parser selection, cache hits/misses, subcommand discovery |
| `trace` | Very verbose — raw help text, parsed structures |

---

## 11. Troubleshooting

### "Tool not found" during scan

The tool must be on your `$PATH`. Verify with:

```bash
which <tool>
```

### Scan produces incomplete results

Some tools have non-standard help output. Try:

1. Increasing depth: `apexe scan <tool> --depth 3`
2. Forcing a fresh scan: `apexe scan <tool> --no-cache`
3. Enabling debug logs to see which parser was selected: `RUST_LOG=debug apexe scan <tool>`

If the tool uses a non-standard help format, a custom parser plugin can be added (see the Developer Guide in the README).

### Serve command does nothing (stdio mode)

In stdio mode, apexe reads JSON-RPC messages from stdin and writes responses to stdout. It is designed to be launched by an AI agent, not run interactively. Use `--show-config` to get the agent integration snippet.

### Permission denied when invoking a tool

The ACL defaults to deny for destructive and unknown commands. Edit `~/.apexe/acl.yaml` to allow the specific command:

```yaml
- pattern: "cli.<tool>.<command>"
  action: allow
```

### Cached results are stale

Force a re-scan:

```bash
apexe scan <tool> --no-cache
```

Or clear the cache directory:

```bash
rm -rf ~/.apexe/cache/
```

### Known limitations (v0.1)

- **A2A protocol** — Stub only; not yet functional.
- **SSE transport** — Stub endpoint only; no streaming.
- **Windows** — Not supported.
- **Agent Card** — `/.well-known/agent.json` endpoint is not yet implemented.
