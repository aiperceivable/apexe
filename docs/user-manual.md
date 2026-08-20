# apexe User Manual

| Field | Value |
|-------|-------|
| **Version** | 0.5.1 |
| **Date** | 2026-07-28 |
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
| `--overlay <PATH>` | - | Load one explicit curated overlay file (JSON/YAML). See [§6.5 Tool Overlays](#65-tool-overlays) |

```bash
apexe scan git                         # basic scan
apexe scan ls jq curl                  # multiple tools
apexe scan git --depth 3               # deeper subcommand discovery
apexe scan git --no-cache              # force re-scan
apexe scan git --format json           # JSON output
apexe scan git --skills-dir ./out      # also write .claude/skills/cli.git.*/SKILL.md
apexe scan ls --overlay ~/.apexe/overlays/ls-gnu.json  # apply a curated overlay
```

### 4.2 `apexe serve`

Starts an MCP server exposing scanned tools to AI agents.

```
apexe serve [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--transport <TYPE>` | `stdio` | Transport: `stdio`, `http`, or `sse` (`sse` is deprecated upstream — see §10) |
| `--host <HOST>` | `127.0.0.1` | Host for HTTP/SSE transports |
| `--port <PORT>` | `8000` | Port for HTTP/SSE transports (1-65535) |
| `--explorer` | off | Enable browser-based Tool Explorer UI (HTTP only) |
| `--modules-dir <DIR>` | `~/.apexe/modules/` | Directory containing binding files |
| `--name <NAME>` | `apexe` | MCP server name |
| `--show-config <TARGET>` | - | Print an integration snippet for `claude-desktop` or `cursor` and exit — see [§12](#12-integrating-with-ai-agents). Any other value is an error on stderr with a non-zero exit |
| `--prefix <PREFIX>` | - | Serve only modules whose id starts with `<PREFIX>` — excluded modules are not callable either |
| `--tags <TAGS>` | - | Serve only modules carrying every listed tag (comma-separated, AND) |
| `--acl <PATH>` | - | Path to ACL policy YAML file (the per-caller boundary) |
| `--auth <MODE>` | per-transport | `token` (default for HTTP/SSE), `jwt`, or `none`. Ignored for stdio |
| `--auth-token <VALUE>` | `APEXE_AUTH_TOKEN` | Bearer token for `--auth token`; one is generated and written to **stderr** at startup if unset |
| `--jwt-secret <VALUE>` | `APEXE_JWT_SECRET` | Signing secret for `--auth jwt` |
| `--allow-unauthenticated-bind` | off | Acknowledge `--auth none` on a non-loopback bind (otherwise refused) |
| `--allow-deprecated-sse` | off | Accepted and ignored; the defect it acknowledged was fixed in apcore-mcp 0.18. Hidden from `--help`, removal planned |
| `--enable-approval` | off | **Deny** every call to a `requires_approval` module — see §9.6 |
| `--no-logging` | off | Disable structured logging middleware entirely |
| `--no-log-arguments` | off | Drop `inputs`/`output` from every log event, error records included; failures keep a payload-free record |
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
| `--tags <T1,T2>` | - | Serve only modules carrying every listed tag. Excluded modules are neither advertised nor callable |
| `--prefix <PREFIX>` | - | Serve only modules whose ID starts with this prefix. Excluded modules are neither advertised nor callable |
| `--no-logging` | off | Disable structured logging middleware entirely |
| `--no-log-arguments` | off | Drop `inputs`/`output` from every log event, error records included; failures keep a payload-free record |
| `--no-circuit-breaker` | off | Disable CircuitBreakerMiddleware (on by default) |
| `--no-retry` | off | Disable RetryMiddleware (on by default; only ever retries idempotent timeouts) |
| `--execution-timeout <SECS>` | `300` | Per-task execution timeout in seconds |
| `--cors-origin <ORIGIN>` | - | Allowed CORS origin (repeatable) |
| `--allow-unauthenticated-bind` | off | Acknowledge a non-loopback `--url` (otherwise refused). A2A has no authenticator, so there is no credential to opt into |

> **No `--enable-approval` on `apexe a2a`.** A2A has no interactive elicitation
> transport, so an approval prompt can never be resolved over it; the flag would
> only ever error. Approval on A2A is a library-only feature — construct
> `A2aServerBuilder` with an `ApprovalStore`. `apexe serve` keeps
> `--enable-approval`, but note that there it is an unconditional **deny** gate
> rather than a prompt — see §9.6.

> **`apexe a2a` has no transport authentication.** The `--auth*` flags are
> `apexe serve` only. Bind A2A to loopback, or put it behind a reverse proxy
> that authenticates. Because there is no credential to grant or
> withhold, `--prefix`/`--tags` carry more weight here than on `apexe serve`:
> narrowing the registered surface is the only mechanism that limits what an
> unauthenticated caller can reach, short of an `--acl` keyed on an identity
> A2A never establishes.

```bash
apexe a2a                                       # http://127.0.0.1:8000
apexe a2a --url http://127.0.0.1:9000 --explorer  # custom port + browser UI
# A non-loopback bind needs the acknowledgement, because A2A has no authenticator:
apexe a2a --url http://0.0.0.0:9000 --allow-unauthenticated-bind
apexe a2a --acl ~/.apexe/acl.yaml               # governed by an ACL policy
apexe a2a --prefix cli.git.                     # serve only the git modules
apexe a2a --tags readonly                       # serve only readonly modules
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

Runs `<tool> --help` and auto-detects the help format. Six built-in parsers, tried in the order below:

| Parser | Detects | Examples |
|--------|---------|---------|
| **Man** | A `--help` that is actually a man page (`NAME` + `SYNOPSIS` at column 0) | every `git <subcommand>` — `git log --help` runs `man git-log` |
| **BSD Usage** | Single-line bundled usage (BSD/macOS built-ins that reject `--help`) | ls, cat, chmod, sort (macOS) |
| **GNU** | Standard GNU-style help | ls, grep, curl, git (GNU/Linux) |
| **Click** | Python Click / argparse | aws, pip |
| **Cobra** | Go Cobra framework | kubectl, docker, gh |
| **Clap** | Rust Clap framework | ripgrep, fd, bat |

Extracts: subcommands, flags (long/short), positional args, types, defaults, enum values, descriptions. Each extracted flag also records a `confidence` level (`verified` > `high` > `medium` > `low`) reflecting how many independent sources (parsers, man page, overlay) agree on it.

### Tier 2: Man Page Enrichment

Parses `man <tool>` output — both GNU (`OPTIONS` section) and BSD (options listed inside `DESCRIPTION`) layouts:

- **DESCRIPTION section**: Enriches commands that have sparse descriptions (< 20 chars).
- **OPTIONS section**: Contributes flags directly to `global_flags` (not just enrichment) — this is what makes tools whose `--help` is a single bundled usage line (most BSD/macOS built-ins) scan to a full flag list instead of zero.
- **EXAMPLES section**: Hand-written invocations are extracted into `ScannedCLITool.examples` / `CommandContract.examples` — the only human-reviewed usage a scan can reach, since flag parsing alone can't say which flag combinations make sense together.

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

Scan results are cached in `~/.apexe/cache/`. Cache entries are keyed by tool name + **variant** + version (`<name>@<variant>_<version>.scan.json`), so a machine with both BSD `/bin/ls` and Homebrew's GNU `ls` caches them separately instead of one overwriting the other. Use `--no-cache` to force a fresh scan.

### 6.4 Tool Variant Detection

Every scan probes the binary (`<binary> --version`) and classifies it into `ScannedCLITool.variant`:

| Variant | Example |
|---------|---------|
| `bsd` | macOS system `/bin/ls`, `/usr/bin/grep` |
| `gnu` | Linux coreutils, Homebrew GNU tools on macOS |
| `apple` | Apple-authored ports (`sort`, `git` shipped with Xcode CLT) |
| `busybox` | BusyBox-based Linux (Alpine, embedded) |
| `unknown` | Version probe inconclusive |

The same command name can be a different program depending on the host, and BSD/GNU/Apple builds of the same tool frequently expose different flag sets — variant detection is what lets a scan (and a matching overlay, see below) pick the right one.

### 6.5 Tool Overlays

An overlay is a curated, human-reviewed description of one tool variant, keyed by `(command, variant, version_range)`. 42 ship built in, covering the 21-command POSIX core (`cat chmod cp cut df diff du find grep head ln ls mkdir mv rm sort tail touch uniq wc xargs`) across their BSD/GNU/Apple variants.

- **`mode: authoritative`** replaces the scan result for that command entirely.
- **`mode: merge`** keeps the scan as the base and only overrides the flags the overlay declares — a gap in the overlay degrades to the scanner's answer instead of erasing a real flag.
- **`confidence: verified`** requires a `provenance` block (platform, version, source document, date) recording how the overlay was checked; the schema rejects a `verified` overlay without it.
- Overlays are the only source that can express `conflicts_with` (mutually exclusive flags) and `long_running` (a flag that may block indefinitely, e.g. `tail -f`) — no `--help`/man format expresses either machine-readably.
- Overlays can also override behavioral annotations (`readonly`/`destructive`/`idempotent`/`requires_approval`) for a specific command.

Load one explicit overlay with `apexe scan <tool> --overlay <PATH>` (JSON or YAML), or install multiple overlays by dropping files under `~/.apexe/overlays/`. The format is defined by `schemas/tool-overlay.schema.json`. See [`docs/overlays.md`](overlays.md) for the full authoring and verification procedure — writing a `verified` overlay from memory instead of a real installation is exactly what it warns against.

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

### Risk / `open_world`

Risk is derived from annotations plus an `open_world` signal — the executable itself (`curl`, `wget`, `ssh`, `scp`, `rsync`, …) or a networked subcommand of an otherwise local tool (`push`, `pull`, `fetch`, `clone`, `deploy`, `login`, …). Precedence when a command matches more than one: `destructive` > `open-world` > `readonly`. This is name-based, so it's a floor rather than a guarantee — an overlay's `annotation_overrides` is the way to assert the truth for a specific tool that doesn't fit the pattern.

---

## 9. Governance

### 9.1 Access Control (ACL)

`apexe scan` automatically generates `~/.apexe/acl.yaml` using a **default-deny** model:

| Module type | Default rule |
|-------------|-------------|
| Readonly modules | `effect: allow` |
| Destructive modules | `effect: deny`, unconditional (no `conditions:` key) |
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
```

> **A deny rule must be unconditional to deny.** apcore registers exactly five
> condition keys — `identity_types`, `roles`, `max_call_depth`, `$or`, `$not`.
> Any other key is treated as *unsatisfied*, so the rule never matches and the
> call falls through to the next rule or to `default_effect`. Earlier versions
> of this manual showed the destructive-deny rule carrying
> `conditions: {require_approval: true}`; copied verbatim under
> `default_effect: allow`, that rule denies nothing and the destructive command
> runs. apexe logs the reason when it happens:
>
> ```
> WARN apcore::acl: Unknown ACL condition 'require_approval' — treated as unsatisfied
> ```
>
> The `~/.apexe/acl.yaml` that `apexe scan` generates has always been correct;
> only the manual was wrong. There is **no ACL condition that means "ask a
> human first"** — approval is a separate layer (§9.6), not an ACL condition.

Rule ordering is **first match wins**, not most-specific-wins: an
`allow` rule for `cli.*` placed before a `deny` rule for `cli.rm` lets `cli.rm`
through. Put the narrow denials above the broad allows.

### 9.2 Audit Trail

`~/.apexe/audit.jsonl` records both the calls that ran and the calls that were
refused. Discriminate on `event`; `trace_id` joins a record to the corresponding
`tracing` line and to the ACL entries below.

A call that reached the wrapped binary — `event: "execution"`:

```json
{
  "timestamp": "2026-08-20T01:24:04.535Z",
  "event": "execution",
  "trace_id": "a09dfb2ab24d4faab69b222c52aac5e4",
  "caller_id": "@external",
  "module_id": "cli.cp",
  "status": "success",
  "exit_code": 0,
  "duration_ms": 3
}
```

A call the governance stack stopped — `event: "refusal"`. `error_code` replaces
`exit_code`, since nothing ever ran to exit:

```json
{
  "timestamp": "2026-08-20T01:20:34.532Z",
  "event": "refusal",
  "trace_id": "71a7bbc6e5f44437a97441952755d9a5",
  "caller_id": "@external",
  "module_id": "cli.cp",
  "status": "refused",
  "error_code": "APPROVAL_DENIED",
  "duration_ms": 0
}
```

- **Refusals are recorded regardless of `--no-logging`.** That flag governs the
  `tracing` stream; turning the operational log down must not turn the audit
  trail off. A caller probing the argv guards produces one `refusal` row per
  attempt either way — the sequence an audit exists to capture.
- **`caller_id` is the authenticated principal**, or `@external` for an
  unauthenticated inbound request (apcore's canonical name for one). It is
  omitted entirely rather than guessed when no identity is attached at all.
  Note this is *not* apcore's `Context::caller_id`, which names the calling
  *module* in a nested chain and is `None` for every inbound request.
- **`duration_ms` is 0 for a refusal that short-circuited** ahead of the
  middleware phase (ACL, approval gate) — no clock had started.
- **No input values, hashed or otherwise.** Earlier versions wrote an
  `input_hash`; it was salted with random bytes that were then discarded, so
  nothing could ever be checked against it. A field that cannot be verified is
  not a privacy control, so it was dropped rather than kept for appearance.
- **Resilience**: audit logging never causes execution failures. Write errors
  are reported via tracing and the call proceeds.
- **Permissions**: the file is created `0600`.

**A third shape appears in the same file.** ACL decisions are written by apcore
itself, not by apexe, and carry its richer entry — `decision`, `reason`,
`identity_type`, `roles`, `call_depth`, and an RFC 3339 timestamp with a
`+00:00` offset rather than `Z`:

```json
{
  "timestamp": "2026-07-30T01:24:35.300132+00:00",
  "caller_id": "@external",
  "target_id": "cli.ls",
  "decision": "deny",
  "reason": "default_effect",
  "identity_type": "external",
  "roles": [],
  "call_depth": 1,
  "trace_id": "36df7e553f354ffcb572e5b961ec4627"
}
```

apexe deliberately does **not** also emit a `refusal` row for an ACL denial: it
would double-count the same event with strictly less detail. A consumer counting
refusals should count `event == "refusal"` plus `decision == "deny"`.

> **Breaking change (unreleased).** The `user` and `input_hash` fields are gone,
> and `event`, `trace_id` and `caller_id` are new. `user` came from `getlogin()`,
> which returns the owner of the controlling terminal — under a service manager
> that is the terminal's owner or `root`, not whoever made the call, so it
> attributed every request to the wrong principal. Consumers keying on either
> removed field need updating.

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

- **`HealthOnlyCircuitBreaker`** — short-circuits calls to a `(module_id, caller)` pair after repeated failures (default: ≥5 samples, ≥50% error rate), instead of letting every caller keep hammering a hanging or broken CLI tool. Auto-recovers after a cooldown window via a single probe call. Only outcomes that say the *wrapped binary* is unhealthy feed the window — spawn failure, timeout, signal death, internal error. Input-validation rejections, conflict refusals and governance decisions (ACL denial, approval denied/timeout/pending, cancellation, depth and frequency limits) do **not** count: the module was reachable and enforced its contract exactly as designed, and schema trial-and-error is the normal behaviour of an LLM caller. Counting those meant five malformed calls disabled a tool for every caller while a binary that failed every time kept its circuit closed.
- **`RetryMiddleware`** — retries a call after a transient failure, but *only* when the error is explicitly marked `retryable`. `CliModule` marks a timeout `retryable` **only when the module is annotated `idempotent`** (§8) — a killed non-idempotent command (e.g. `rm -rf` timing out mid-delete) is never auto-retried, since it may have partially applied its side effect.

### 9.6 Approval: `--enable-approval` is a deny gate, not a prompt

On a CLI-launched server, `--enable-approval` **denies every call to a module
annotated `requires_approval`**. It does not prompt anyone. Read that before
turning it on: enabling it bricks every gated tool.

Why: apcore-mcp's `ElicitationApprovalHandler` needs an `ElicitCallback`, and
that callback only exists inside apcore-mcp's own router, in a side-channel map
of `Box<dyn Any>`. The `ApprovalRequest` that reaches an externally-constructed
`ApprovalHandler` carries only the JSON `Context`, which holds the string
`"available"` rather than the callback itself — apcore-mcp documents this as a
Rust-specific limitation. There is no path from a handler apexe can build to the
transport, so no interactive approval is possible from `apexe serve`.

apexe therefore installs its own `DenyApprovalHandler`, which says exactly that
in the rejection reason and logs a warning at startup, rather than returning an
opaque "No elicitation callback available". For a real per-caller boundary, use
`--acl` (§9.2), which is enforced on `tools/call`.

For approval flows where the approver isn't in the session (e.g. a Slack bot),
`apexe`'s `ExecutorOptions`/`McpServerBuilder`/`A2aServerBuilder` accept an
`Arc<dyn apcore_mcp::ApprovalStore>` — when set, approvals become non-blocking
(`StorageBackedApprovalHandler`): the call returns an `ApprovalPending` error
immediately, and a separate mechanism you build resolves the decision later via
`ApprovalStore::resolve`.

> **Known limitation.** `requires_approval` is derived from whether a tool's
> *help text* mentions a flag in `APPROVAL_FLAGS`, not from the arguments a
> caller actually sent. So `cli.git.log` is gated (because `git log` accepts
> `--all`) while `cli.ssh` and `cli.curl` are not. Evaluating the gate against
> the rendered argv is tracked separately.

There is no CLI flag for this — `apcore-mcp`'s `InMemoryApprovalStore` is documented as unsuitable for production (state isn't shared across process invocations, so a hypothetical `apexe approval resolve` command couldn't reach a running server's store anyway). Embed apexe as a library and supply your own persistent store (Redis, a database, etc.) to use this for real.

---

## 10. MCP Server

### Transport Options

| Transport | Use case | Command |
|-----------|----------|---------|
| **stdio** | Claude Desktop, Cursor (default) | `apexe serve` |
| **streamable-http** | Remote agents, browser UI | `apexe serve --transport http --port 8000` |
| **sse** | ⚠️ Deprecated upstream, but served | `apexe serve --transport sse --port 8000` |

> **`--transport sse` is deprecated, but it works again and no longer needs an acknowledgement.**
> apexe used to refuse it: apcore-mcp shared one process-global channel across
> every connection, so responses went round-robin to whichever stream was next
> and, with two clients connected, **one client received the other's tool
> output**. apcore-mcp 0.18 scopes a session per connection and emits the
> `event: endpoint` a spec-compliant MCP SSE client waits for, so the defect is
> gone and the refusal went with it. apexe requires `apcore-mcp = "0.18"`, so a
> build cannot quietly resolve back to the affected 0.17.
>
> SSE is still **deprecated upstream** and apexe warns at startup. Prefer
> `--transport http` (streamable HTTP) for anything new.
>
> `--allow-deprecated-sse` is still accepted so existing invocations keep
> parsing, but it decides nothing and is hidden from `--help`. It will be
> removed in a later release.

### Transport Authentication

The three transports have different trust boundaries, so they get different
defaults:

| Transport / bind | Default | Why |
|---|---|---|
| **stdio** | no auth | The boundary is the parent/child process relationship — whatever can spawn apexe already holds your privileges. A token adds nothing and would break every Claude Desktop / Cursor config. |
| **HTTP/SSE on `127.0.0.1`** | bearer token, **generated and written to stderr at startup** | Any local process can reach the port, including a page in your browser. You should not have to manage a secret for a local dev server. |
| **HTTP/SSE on any other host** | authentication required | apexe wraps arbitrary local binaries. An unauthenticated non-loopback bind is a remote-execution entry point for every executable on the host. |

> **apexe terminates no TLS.** On a non-loopback bind the bearer token or JWT
> travels in cleartext, and anything on the path can replay it to reach the same
> remote-execution surface the credential exists to close. apexe warns at
> startup — both through `tracing` and in the token notice on stderr, since the
> two can be filtered differently — but it cannot refuse: apexe behind a
> TLS-terminating reverse proxy is the correct deployment and looks identical
> from inside the process. Put one in front, or bind to `127.0.0.1` and reach it
> through an SSH tunnel.

```bash
apexe serve --transport http                              # token generated + printed
apexe serve --transport http --auth-token "$MY_TOKEN"     # or APEXE_AUTH_TOKEN
apexe serve --transport http --auth jwt --jwt-secret "$S" # or APEXE_JWT_SECRET
apexe serve --transport http --auth none                  # loopback only
apexe serve --transport http --host 0.0.0.0 --auth none \
            --allow-unauthenticated-bind                  # states that you mean it
```

A generated token is written **directly to stderr**, not through the log
pipeline. Two consequences worth relying on: it appears at every `--log-level`,
including `warn` and `error`, so the server can never demand a credential you
have no way to read back (`--show-config` omits credentials by construction);
and it is never written into a log file, journald, or a log aggregator you have
pointed `tracing` at. The log itself only records *that* token auth is on.

Clients send `Authorization: Bearer <token>`. The Explorer UI's `Authorization`
field is wired to this — browsing (GET) is open, execution (POST, including
`POST /explorer/tools/{tool}/call`) requires the credential.

`/health` is exempt so container and load-balancer probes work. `/metrics` is
**not** exempt: its `module_id` labels and per-module call volumes are
reconnaissance about what this host wraps and what actually gets used.

`--auth none` on a non-loopback bind refuses to start without the separate
`--allow-unauthenticated-bind` acknowledgement — a `--disable-*` flag gets
copied out of a tutorial once and then lives in everyone's startup script
forever.

### Built-in Middleware

| Middleware | Status | Effect |
|-----------|--------|--------|
| **LoggingMiddleware** | Enabled by default | Structured logging of inputs/outputs, redacting properties the scanner marked `x-sensitive` — see below |
| **FailureLogMiddleware** | Automatic with `--no-log-arguments` | One payload-free `ERROR` record per failed call — see below |
| **CircuitBreakerMiddleware** | Enabled by default (`--no-circuit-breaker`) | Short-circuits a hanging/broken tool — see §9.5 |
| **RetryMiddleware** | Enabled by default (`--no-retry`) | Retries idempotent timeouts only — see §9.5 |
| **DenyApprovalHandler** | Opt-in (`--enable-approval`) | Denies every call to a `requires_approval` module — see §9.6 |

> **Credentials in tool arguments.** The logging middleware records each call's
> `inputs` and `output` at INFO. Redaction is schema-driven: the scanner marks
> credential-bearing options (`curl --user`, `--oauth2-bearer`, `--header`, key
> and certificate paths, and their equivalents) with `x-sensitive: true`, and
> those values are replaced before anything is written. A heuristic cannot be
> exhaustive over every wrapped tool's option set — a request body (`curl
> --data`) and a key sitting in a URL's query string announce themselves in no
> schema — so if you pass secrets through options apexe may not recognize, run
> with `--no-log-arguments`.
>
> `--no-log-arguments` drops the payload from **every** log event, the error
> record included. That matters because apcore's error record renders the same
> partially-redacted argument object as the `START` line, so a call rejected by
> schema validation used to print the body a successful call would have hidden.
> The operational record survives in a different shape: apexe installs its own
> `FailureLogMiddleware`, which emits one `ERROR` per failed call carrying
> `module_id`, `trace_id`, `caller_id`, `error_code` and `duration_ms` — and
> nothing the caller sent, not even the error message, since a validation
> message quotes the value it rejected. So a refusal is still visible, and
> `error_code` is what an alert keys on.
>
> `--no-logging` remains available to drop logging altogether, failure records
> included. The `audit.jsonl` trail records no raw argument values in any case.

### Observability

`apexe serve --transport http --metrics` enables two endpoints (HTTP/SSE only, ignored on stdio):

| Endpoint | Format | Content |
|----------|--------|---------|
| `/metrics` | Prometheus text | `apcore_module_calls_total`, `apcore_module_duration_seconds` histogram, per `module_id` |
| `/usage` | JSON | Per-module call count, error count, average latency, unique callers, trend |

### Tool Filtering

`--tags` and `--prefix` (and their `McpServerBuilder` equivalents) restrict
which scanned modules the server exposes. The filter is applied at
**registration** time, so an excluded module is not merely absent from
`tools/list` — it does not exist on this server at all, and `tools/call`,
`resources/list` and `resources/read` all return `ModuleNotFound` for it.

```bash
apexe serve --prefix cli.git            # a git-only server
apexe serve --tags readonly             # every listed tag must match (AND)
```

```rust
McpServerBuilder::new()
    .tags(vec!["readonly".to_string()])     // only expose readonly tools
    .prefix("cli.git")                       // only expose git tools
    .build()?;
```

This is coarse-grained: it selects a subset of the tool surface for everyone.
For per-caller rules, use `--acl` (§9.2).

> **A stray comma is refused, not applied.** `--tags readonly,` splits to
> `["readonly", ""]`, and every listed tag must match — no module carries an
> empty tag, so the filter would admit nothing and the server would start with
> an empty registry and no callable tools at all. `apexe serve` exits with an
> error naming the empty tag instead. A tag that is merely unknown to *this*
> host is not an error (one invocation is meant to be portable across
> differently scanned machines), but if the filter excludes every loaded
> module apexe logs a **warning** saying so, rather than the `admitted=0` info
> line that looked identical to an empty modules directory.

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
uses. **Approval** is the one exception: `apexe serve` offers
`--enable-approval`, which is an unconditional deny gate rather than a prompt
(§9.6), while `apexe a2a` has no such flag at all — approval on A2A is
library-only (supply an `ApprovalStore` to `A2aServerBuilder`).
Transport authentication (§10) is likewise `apexe serve` only.

```bash
apexe a2a                                       # http://127.0.0.1:8000
apexe a2a --url http://127.0.0.1:9000 --explorer  # custom port + browser UI
# A non-loopback bind needs the acknowledgement, because A2A has no authenticator:
apexe a2a --url http://0.0.0.0:9000 --allow-unauthenticated-bind
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
default policy (§9.1), alongside `--enable-approval`, which is an
unconditional deny gate over `requires_approval` modules rather than a prompt
(§9.6).
See [`examples/acl_demo`](../examples/acl_demo/) for a library-level
demonstration of the same role-based ACL contract, driven directly against
the `Executor`.

---

## 12. Integrating with AI Agents

### `--show-config` reproduces the invocation you type

`--show-config` renders the *rest of the command line* into the snippet, so add
every flag you intend to serve with:

```bash
apexe serve --show-config claude-desktop \
  --modules-dir /srv/apexe/modules --prefix cli.git --acl /etc/apexe/acl.yaml
```

```json
{
  "mcpServers": {
    "apexe": {
      "command": "apexe",
      "args": ["serve", "--transport", "stdio",
               "--modules-dir", "/srv/apexe/modules",
               "--prefix", "cli.git",
               "--acl", "/etc/apexe/acl.yaml"]
    }
  }
}
```

`--modules-dir`, `--tags`, `--prefix`, `--acl`, `--name`, `--enable-approval`,
`--no-logging`, `--no-log-arguments`, `--no-circuit-breaker` and `--no-retry`
are all carried through. `--modules-dir` matters most: a client launching
`apexe serve` without it reads the default `~/.apexe/modules`, so anyone who
scanned elsewhere would get a server with **no tools at all**.

**Paths are made absolute.** A relative `--modules-dir ./modules` is written
into the snippet as `/abs/path/to/cwd/modules`, resolved against the directory
you ran `--show-config` in. The client that later runs the snippet launches
`apexe` from its own working directory, so a relative path would resolve
somewhere else entirely — the server would log `Modules directory not found,
starting with zero tools` and exit 0, and a relative `--acl` would silently
apply no policy at all.

> **Credentials are never written into a snippet.** `--auth-token` and
> `--jwt-secret` are excluded by construction, because a config file is shared
> and often committed. For an HTTP server behind `--auth token`, the snippet
> carries only the URL — configure the `Authorization: Bearer` header in the
> client, from your own secret store.

An unrecognised target (`--show-config vscode`) is rejected on **stderr** with
a non-zero exit, so `apexe serve --show-config … > mcp.json` cannot write an
error message into the config file.

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

Add the JSON to Cursor's MCP settings. `cursor` honours `--transport`: with
`--transport http` it emits the `url` form rather than a command block.

### HTTP Mode (Remote Agents)

```bash
apexe serve --transport http --host 0.0.0.0 --port 8000
apexe serve --show-config claude-desktop --transport http --port 8000
```

The MCP endpoint is at `POST /mcp` (the deprecated SSE transport is served at
`GET /sse` instead, and the generated snippet uses whichever path matches
`--transport`).

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

### A non-zero exit is **not** an MCP error — key on `exit_code`

A wrapped command that runs and exits non-zero comes back as an ordinary,
successful tool result: **`isError: false`**, with `exit_code`, `stdout`,
`stderr` and `ai_guidance` in the payload.

`exit_code` is **not** a sibling of `isError`. MCP's `tools/call` result has a
fixed shape — `content` plus `isError` — and apcore-mcp puts the whole payload
into `content[0].text` as a **JSON string**:

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"exit_code\":2,\"stdout\":\"\",\"stderr\":\"curl: option --max-time=1: is unknown\",\"ai_guidance\":\"Command 'cli.curl' exited with code 2. stderr: ...\"}"
    }
  ],
  "isError": false
}
```

So a client reads `exit_code` by JSON-parsing `content[0].text` first:

```js
const payload = JSON.parse(result.content[0].text);
if (payload.exit_code !== 0) { /* the command ran and answered */ }
```

Reading `result.exit_code` directly yields `undefined`, and a client that then
falls back to `isError` lands exactly in the trap the rest of this section
exists to prevent.

This is deliberate and will not change. In a CLI bridge, a non-zero exit is
frequently the *answer*, not a failure:

| Command | Exit 1 means |
|---------|--------------|
| `grep pattern file` | no match found |
| `diff a b` | the files differ |
| `test -f path` | the predicate is false |

Setting `isError: true` on a non-zero exit would report all three as failed
tool calls, and an agent would retry or abandon a tool that answered its
question correctly.

`isError: true` is reserved for **the call never reaching the binary**:

- schema validation rejected the arguments,
- the ACL denied the caller,
- `--enable-approval` blocked a `requires_approval` module,
- the circuit breaker was open, the timeout killed the process, or the binary
  could not be spawned at all.

On that path `content[0].text` is a plain diagnostic sentence rather than a
JSON payload, so parsing it is only meaningful when `isError` is `false`.

**So: an MCP client must not treat `isError: false` as "the command
succeeded".** Parse `content[0].text` and read `exit_code` from it (it is
`required` in every generated output schema, alongside `stdout` and `stderr`),
then apply the wrapped tool's own convention.

The A2A surface follows the same split: a non-zero exit yields
`TASK_STATE_COMPLETED` with the `exit_code` inside the artifact, while a
governance or validation refusal yields `TASK_STATE_FAILED` (or
`TASK_STATE_INPUT_REQUIRED` when an approval is pending, and
`TASK_STATE_CANCELED` on cancellation). There too, the task state answers "did
the command run", not "did it succeed".

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
