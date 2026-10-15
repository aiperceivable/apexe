# apexe Feature Manifest

## Project Overview

**apexe** is a governed execution layer for CLI tools and HTTP APIs. It
turns discoverable CLI commands into typed apcore modules, runs CLI commands
locally, and serves CLI and imported OpenAPI operations through MCP and A2A.
Configured access controls, approval checks and audit records accompany the
execution path.

**Version:** 0.9.0.

**Status:** CLI discovery, initialization, binding/Skill generation, local
module execution, reviewed native argv and PATH shims, MCP/A2A serving, OpenAPI
import and CLI contract verification are implemented. Native coverage remains
a bounded reviewed-overlay pilot; verification evidence covers explicit cases.
Run `cargo test --all-features` for the current test count. Host-dependent
git/docker scan tests and the macOS native example are opt-in.

## Current capability inventory

| Capability | Implementation / entry point | Scope |
|---|---|---|
| CLI discovery and schemas | `scanner`, `adapter`; `scan` | Help, man pages, completions, variant-aware overlays, JSON Schema and annotations |
| Reviewed source initialization | `corpus`, `cli::init`; `init` | Cached Git rules, revision pinning, offline use, refresh and binding/policy preservation |
| Terminal-agent execution routing | `skills/apexe/SKILL.md`, `scripts/install_agent_skill.py` | Shared Claude Code/Codex/pi/OpenCode Skill; external CLI commands default to native exec from the first terminal call in configured environments, optional env and governed JSON alternatives |
| Binding and agent guidance output | `output`; `scan --skills-dir` | YAML bindings and optional Claude Skills; Rust API tool-definition export |
| Discovery and policy inspection | `list --verbose`, `policy` | Module schemas, annotations, ACL context, validated evidence and filesystem boundary checks |
| Local execution | `cli::exec`, `module`; `exec` | Module JSON and reviewed native argv; preflight dry-run, child status and native streams |
| Shell integration | `cli::env`, `shim`, `shell_init`; `env` | Eligible external-command PATH shims, startup activation, refresh, removal and diagnostics |
| MCP serving | `mcp`; `serve` | stdio, streamable HTTP, legacy SSE, structured results, Explorer, client snippets and optional metrics |
| A2A serving | `a2a`; `a2a` | Authenticated agent server and agent card over the shared executor |
| HTTP API import | `openapi`; `serve/a2a --openapi` | OpenAPI 3.0/3.1 file/URL import at startup; supported JSON proxy operations |
| Execution governance | `governance`, `module::approval`, `auth` | Configured ACL/approval, token/JWT identities and payload-free governance audit records |
| CLI process controls | `module::executor`, `module::io`, `governance::path_guard` | Typed-path checks, direct argv, scrubbed environment, timeout and capped captured output |
| CLI contract verification | `verification`, `cli::verify`; `verify` | Source drift and explicit readonly/isolated/authorized dry-run cases with digest-bound evidence |
| Configuration and diagnostics | `config`, `logging`; `config` | CLI/env/file defaults, structured errors, rotating diagnostics and argument-log controls |

Local execution loads the default ACL when present and installs an approval
gate. Servers require `--acl` to enforce access control; MCP interactive approval
requires `--enable-approval`, while A2A approval stores are library-only. OpenAPI
modules use shared schema validation, configured ACLs and auditing, but not CLI
filesystem checks. HTTP import is unavailable to local exec/shims. Normal
execution is not OS-sandboxed. The current native BSD pilot covers cat/head/ls;
builtins and absolute paths can bypass PATH shims.

> The section headings below tagged "(v0.1.0 …)" record when each piece first
> landed; the crate is now **0.9.0**. For the authoritative current state see
> [`CHANGELOG.md`](../CHANGELOG.md) and [`docs/user-manual.md`](user-manual.md).

## Architecture

Scan path — a binary becomes governed bindings on disk:

```
CLI Tool Binary
      |
      v
[Scanner Engine] ──→ ScannedCLITool   (tiers 1-3: --help, man, completions)
      |                                (tier 4: curated overlay, if one matches)
      v
[Adapter Layer] ──→ ScannedModule (apcore-toolkit)
      |
      ├──→ [YamlOutput]  ──→ .binding.yaml files (one per module)
      ├──→ [AclManager]  ──→ acl.yaml (apcore ACL, default_effect: deny)
      └──→ [SkillOutput] ──→ .claude/skills/<id>/SKILL.md  (with --skills-dir)
```

Call path — MCP, A2A, local execution and the library use shared executor
construction. Servers enable ACL and approval explicitly; local execution
loads the default ACL when present and always installs the approval gate.
Local execution disables retry and circuit breaking and suppresses argument logs:

```text
MCP client       A2A client       exec / PATH shims       Rust library
     |                |                  |                   |
     +----------------+------------------+-------------------+
                                      |
                              [apcore Executor]
                                      |
                   schema validation / configured ACL
                    approval handler / middleware chain
                                      |
                         +------------+------------+
                         |                         |
                    [CliModule]             [HTTP proxy module]
                         |                   server import only
                typed-path checks                  |
                argv validation                    |
                         |                         |
              direct argv / scrubbed env       HTTP API
              timeout / captured output cap    upstream timeout
                         |                         |
                    CLI binary                     |
                         +------------+------------+
                                      |
                      audit.jsonl: execution / refusal
                            and ACL decision records
```

CLI process controls do not apply to the HTTP proxy branch. Audit records cover
both module types; the API must enforce its own data authorization.

## Module Map

```
src/
├── a2a/             A2A agent server integration
│   └── server       A2aServerBuilder wrapping apcore_a2a; bind-URL validation
├── adapter/         ScannedCLITool → ScannedModule conversion
│   ├── converter    CliToolConverter (tree flattening, module ID generation,
│   │                 mark_escalating_params for per-call approval)
│   ├── schema       JSON Schema from flags/args, incl. the x-apexe-* keywords
│   ├── contract     CommandContract: the flattened per-command scan report
│   ├── annotations  ModuleAnnotations inference (readonly/destructive/
│   │                 idempotent/open_world, EXEC_WRAPPER_TOOLS)
│   └── overlay      ToolOverlay: curated flag/arg descriptions, apply_overlay
├── auth             Transport authentication for the HTTP-family MCP/A2A
│                    transports (token/JWT, loopback-bind detection)
├── cli/             clap CLI entry point (scan/serve/a2a/exec/env/verify/init/list/config/policy)
│   ├── init         Reviewed source bootstrap, binding ownership and starter ACL
│   ├── exec         Module JSON and native argv through shared governance
│   ├── config_set   Persistent log-level updates preserving other settings
│   ├── config_gen   Claude Desktop / Cursor config snippet generation
│   ├── env          Governed shim enable/reload/disable and activation output
│   └── shell_init   Managed startup blocks for automatic terminal activation
├── config           ApexeConfig + apcore CoreConfig integration
├── corpus/          Managed Git snapshots, validated catalog and source acquisition
├── logging          Binary diagnostic file logging with daily rotation
├── errors           ApexeError + From<ApexeError> for ModuleError
├── governance/      Access control, audit, filesystem boundary
│   ├── acl          AclManager wrapping apcore::ACL
│   ├── audit        AuditManager, apexe's own JSONL execution/refusal record
│   └── path_guard   PathGuard: resolves every x-apexe-path argument and checks
│                    it against the system/credential baselines. Always-on, no
│                    flag; `apexe policy` reports it. CLI process controls
│                    are in module/executor.rs and module/io.rs
├── mcp/             MCP server integration
│   └── server       McpServerBuilder wrapping apcore_mcp::APCoreMCP
├── models/          ScannedCLITool, ScannedCommand, ScannedFlag, ScannedArg
├── module/          apcore Module trait implementation
│   ├── cli_module   CliModule (subprocess execution via Module trait)
│   ├── executor     Argument building, validation, env scrubbing,
│   │                 tokio::process (timeout + kill_on_drop)
│   ├── approval     ApprovalGate wrapping apcore's approval handlers
│   ├── breaker      HealthOnlyCircuitBreaker middleware
│   ├── failure_log  Payload-free failure/refusal logging middleware
│   └── registry     build_executor: load bindings, wire middleware/ACL/approval
├── output/          Binding file I/O
│   ├── yaml         YamlOutput wrapping apcore_toolkit::YAMLWriter
│   ├── loader       load_modules_from_dir (reads .binding.yaml)
│   └── skill        SkillOutput: writes a Claude Skill (SKILL.md) per module
├── openapi          HTTP operation import shared by MCP and A2A
├── runtime_path     Clean PATH resolution excluding governed shim directories
├── verification/    Source drift, isolated cases and digest-bound evidence
└── scanner/         Deterministic CLI scanner engine (tiers 1-3 + overlay)
    ├── orchestrator    ScanOrchestrator (top-level coordinator)
    ├── pipeline        ParserPipeline (priority-based parser selection)
    ├── parsers/        Man, BSD Usage, GNU, Click, Cobra, Clap format parsers
    ├── discovery       SubcommandDiscovery (recursive subcommand scanning)
    ├── cache           ScanCache (JSON filesystem caching)
    ├── resolver        ToolResolver (binary path + version + format detection)
    ├── exec            run_with_timeout: bounded subprocess probes
    ├── man_page        Man-page section/option/example extraction
    ├── variant         Tool variant detection (bsd/gnu/apple/busybox)
    ├── overlay_store   External corpus + user overlay loading and matching
    ├── value_placeholder  Shared placeholder → ValueType inference table
    ├── completion      Shell-completion-script subcommand discovery
    └── protocol        ParsedHelp / CliParser: the shared parser contract
```

## apcore Ecosystem Integration

| Crate | Version | Usage |
|-------|---------|-------|
| `apcore` | 0.32.1 | Module trait, Registry, ACL, ModuleError, ErrorCode, Context, Config |
| `apcore-toolkit` | 0.14.0 | ScannedModule, YAMLWriter, Verifier, ModuleAnnotations, `deduplicate_ids` |
| `apcore-mcp` | 0.23.0 | APCoreMCP server with structured results (stdio, streamable-http, SSE, Explorer UI) |
| `apcore-a2a` | 0.8.3 | A2A agent server with token/JWT authentication (`build_app_with_auth`) |
| `apcore-cli` | 0.12.4 | `--man` page generation (`build_program_man_page`) |

## v0.1.0 Features

### Scanner Engine (preserved from v0.1.x)
Three-tier deterministic scanner with plugin system, plus a fourth tier that
applies a human-reviewed overlay on top of what the first three observed:

1. **Tier 1 -- `--help` parser** (6 built-in parsers: Man, BSD Usage, GNU, Click, Cobra, Clap)
2. **Tier 2 -- Man page parser** (DESCRIPTION / OPTIONS / EXAMPLES extraction)
3. **Tier 3 -- Shell completion parser** (zsh/bash subcommand discovery)
4. **Tier 4 -- Curated overlay** (applied only when one matches the detected variant; see below)

Additional: ParserPipeline, SubcommandDiscovery, ScanCache, ToolResolver, plugin system.

### Adapter Layer (v0.1.0 new)
- `CliToolConverter`: flattens subcommand trees → `Vec<ScannedModule>`
- `schema::build_input_schema/output_schema`: JSON Schema from flags/args
- `annotations::infer`: readonly/destructive/idempotent inference from command names

### Module Executor (v0.1.0 new)
- `CliModule`: implements apcore `Module` trait for CLI subprocess execution
- Async execution via `tokio::process::Command` with `tokio::time::timeout` and `kill_on_drop(true)`
- No shell is ever invoked — argv is built as a `Vec<String>` and passed straight to `execve`, so shell metacharacters (`;|&$\`'"`) are inert data, not something the argument path needs to block. The actual guard rejects a value that would be parsed as an *option* by the wrapped binary (a leading `-`, unless the schema declares `--` support) and rejects control characters.
- Argument validation happens inline in `executor::build_argv` (there is no separate `preflight` step)

### Output Layer (v0.1.0 new, replaces v0.1.x binding generator)
- `YamlOutput`: wraps apcore-toolkit `YAMLWriter` with verification
- `load_modules_from_dir`: reads `.binding.yaml` files back as `Vec<ScannedModule>`

### MCP Server (v0.1.0 new, replaces v0.1.x self-built server)
- `McpServerBuilder`: bindings_dir → Registry → Executor → APCoreMCP
- Transports: stdio, streamable-http (was "http"), SSE
- MCP tool discovery and invocation via the published apcore-mcp 0.23.0 SDK.
  It negotiates `2025-06-18` and retains `2025-03-26` and `2024-11-05`. Compatible clients
  receive object `outputSchema` and redacted `structuredContent`, with JSON text
  retained for compatibility. HTTP selects the format per protocol-version header.
- Explorer UI (HTTP transports)
- Transport authentication is a first-class CLI surface (`--auth token|jwt|none`, `--auth-token`, `--jwt-secret`), required by default on the HTTP-family transports; a non-loopback bind without an explicit acknowledgement (`--allow-unauthenticated-bind`) refuses to start. See `docs/user-manual.md` §9.

### Governance (v0.1.0 rewritten, extended through v0.7.0)
- `AclManager`: wraps `apcore::ACL`, generates default rules from annotations; fails closed when a configured ACL is missing/malformed. **Opt-in** — a server started without `--acl` enforces no access control
- `AuditManager`: apexe's own append-only JSONL trail (log `0o600`); records executions, refusals (`event`, `trace_id`, `caller_id`, `error_code`) and ACL allow/deny decisions. No input values, hashed or otherwise — see user-manual §9.2
- `PathGuard` (v0.7.0): resolves every argument the schema types `x-apexe-path` and checks it against two compiled-in baselines — system paths bind writers, credential paths bind readers and writers alike. **Always-on, no flag.** `config.yaml` extends it with `additional_denied_paths` and relaxes it with `allowed_paths`; `apexe policy` reports the installed boundary
- `ApprovalGate`: weighs actual escalating inputs and audits refusals. Local execution installs it automatically. MCP interactive approval is opt-in via `--enable-approval`; approval stores can be supplied through the MCP/A2A library builders
- CLI process controls: always-on in `module/executor.rs` and `module/io.rs` — env scrubbing, direct argv, captured output cap, timeout + `kill_on_drop`, plus option-injection, control-character and `x-apexe-exec` guards. Native streams are inherited; there is no normal-execution SandboxManager

## Tool Overlays & Variant Detection (v0.4.0, extended in v0.5.0)

- **Variant detection**: every scan probes the binary (`<binary> --version`) and classifies it as `bsd`, `gnu`, `apple`, `busybox` or `unknown`, surfaced as `ScannedCLITool.variant`. Same command name, different implementation (macOS `/bin/ls` vs. Homebrew GNU `ls`) now yields different, correct results instead of one overwriting the other in cache.
- **Curated overlays**: a reviewed description of one `(command, variant, version_range)`, matched by `probe` > `platform` + `binary_globs` > `platform` alone. `mode: authoritative` replaces the scan surface; `mode: merge` overrides matching flags on top of the scan. **apexe ships none**: the corpus is the [cli-permissions](https://github.com/aiperceivable/cli-permissions) repository, read through `overlay_dirs`, `~/.apexe/overlays/*.{json,yaml}`, `apexe scan --overlay <PATH>`, or one of the packaged locations searched by default. Format defined by [`tool-overlay.schema.json`](https://github.com/aiperceivable/cli-permissions/blob/main/schemas/tool-overlay.schema.json), which lives with the corpus. See [`docs/overlays.md`](overlays.md) for the authoring/verification procedure.
- **Provenance requirement**: an overlay at `confidence: verified` must record how it was checked (platform, version, source document, date); the schema rejects a `verified` overlay without it.
- **Per-flag confidence/sources**: every `ScannedFlag` records source evidence capped at `high`; runtime `verified` module confidence is earned by explicit cases with matching digests. `apexe verify --record` persists case evidence and invalidates stale binding, binary and case-file fingerprints. Dry-run and private-CLI cases require OS isolation; see [verification](verification.md).
- **Man page `EXAMPLES` extraction** (v0.5.0): `ScannedCLITool.examples` / `CommandContract.examples` carry hand-written invocations pulled from a tool's man page.
- **`open_world` inference** (v0.5.0, breaking): risk annotation is inferred from the executable and networked subcommand names instead of being hardcoded, so `Risk::OpenWorld` is now actually emitted (e.g. `curl`, `git push`/`pull`/`clone`).

## Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `apcore` | Core module system, ACL, errors |
| `apcore-toolkit` | Scanner types, YAML writer, verifiers |
| `apcore-mcp` | MCP protocol server |
| `apcore-a2a` | A2A protocol server |
| `apcore-cli` | `--man` page generation |
| `clap` (derive mode) | CLI argument parsing |
| `serde` + `serde_json` + `serde_yaml` | Serialization |
| `tokio` | Async runtime |
| `tracing` + `tracing-subscriber` | Structured logging |
| `thiserror` | Typed error definitions |
| `regex` | Help-text parsing and format detection (all parsers) |
| `chrono` | RFC 3339 timestamps for the audit trail |
| `uuid` | UUID v4 for trace IDs |
| `shell-words` | Shell argument splitting |

## Initialization

`apexe init` bootstraps reviewed command coverage using the shared scanner and
ACL writer. Source selection, pinning and refresh use `--overlays`, `--ref` and
`--refresh`; the old `overlays` command is removed. Initialization preserves
existing policy and custom/edited bindings, and reports fallback coverage.

## Scope and remaining work

- **Broader native command coverage** — Requires matching reviewed grammar and
  explicit stdin/bundling capabilities. Scanner coverage alone does not make a
  command eligible for native argv or PATH shims.
- **Session daemon** — Deferred F8 stage; current shims invoke the apexe process
  for each command. See [F8](features/v2-f8-local-execution.md) for measurements.
- **Normal execution sandboxing** — OS filesystem, network and syscall
  containment remain outside normal exec/serve. Explicit verification cases have
  their own macOS/Linux isolation backends. See [verification](verification.md)
  and the [threat model](threat-model.md).
- **Additional HTTP proxy formats** — Multipart, per-input header/cookie
  parameters and non-JSON responses are outside the current OpenAPI surface.
  See [OpenAPI import](openapi.md) for supported method/parameter combinations.
- **Native Windows scanning** — Not supported; Windows help and PowerShell
  reflection need their own discovery path.
- **`apexe evo`** — Deferred; depends on apevo product maturity.
