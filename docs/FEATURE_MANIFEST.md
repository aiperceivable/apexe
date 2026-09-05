# apexe Feature Manifest

## Project Overview

**apexe** -- Outside-In CLI-to-Agent Bridge. Automatically wraps CLI tools into governed apcore modules, served via MCP/A2A.

**Version:** 0.7.0 — Full apcore ecosystem integration (MCP + A2A), curated tool overlays and variant-aware scanning, plus an always-on filesystem path guard and per-call approval escalation.

**Status:** F1–F7 implemented; F8 (local execution) is design, Stage 1 ready to build. Run `cargo test --all-features` for the current test count — two integration tests are ignored unless `git`/`docker` are on `PATH`.

> The section headings below tagged "(v0.1.0 …)" record when each piece first
> landed; the crate is now **0.7.0**. For the authoritative current state see
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

Call path — every surface shares one `build_executor`, so the controls below
apply identically to MCP, A2A and the library API:

```
MCP client                A2A client
     |                         |
     └───────────┬─────────────┘
                 v
        [apcore Executor]
                 |
    ┌────────────┼──────────────────────────────┐
    |            |                              |
[ACL]      [ApprovalGate]              [middleware chain]
 --acl     --enable-approval        CircuitBreaker / Retry
 opt-in    opt-in, per-call         Logging / FailureLog
    |            |                              |
    └────────────┴──────────────┬───────────────┘
                                v
                          [CliModule]
                                |
                                v
                      [PathGuard]  always-on, no flag
                                |
                                v
                 [executor::build_argv]  option-injection +
                                |        control-char + exec-param guards
                                v
              [execute_subprocess]  env-scrubbed, no shell,
                                |   output-capped, timeout + kill_on_drop
                                v
                          wrapped binary
                                |
                                v
                   audit.jsonl  execution / refusal / acl_decision
```

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
├── cli/             clap CLI entry point (scan/serve/a2a/list/config/policy)
│   └── config_gen   Claude Desktop / Cursor config snippet generation
├── config           ApexeConfig + apcore CoreConfig integration
├── errors           ApexeError + From<ApexeError> for ModuleError
├── governance/      Access control, audit, filesystem boundary
│   ├── acl          AclManager wrapping apcore::ACL
│   ├── audit        AuditManager, apexe's own JSONL execution/refusal record
│   └── path_guard   PathGuard: resolves every x-apexe-path argument and checks
│                    it against the system/credential baselines. Always-on, no
│                    flag; `apexe policy` reports it. (Subprocess isolation is
│                    likewise always-on, in module/executor.rs)
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
    ├── overlay_store   Built-in + user overlay loading and matching
    ├── value_placeholder  Shared placeholder → ValueType inference table
    ├── completion      Shell-completion-script subcommand discovery
    └── protocol        ParsedHelp / CliParser: the shared parser contract
```

## apcore Ecosystem Integration

| Crate | Version | Usage |
|-------|---------|-------|
| `apcore` | 0.28 | Module trait, Registry, ACL, ModuleError, ErrorCode, Context, Config |
| `apcore-toolkit` | 0.10 | ScannedModule, YAMLWriter, Verifier, ModuleAnnotations, `deduplicate_ids` |
| `apcore-mcp` | 0.19 | APCoreMCP server (stdio, streamable-http, SSE, Explorer UI) |
| `apcore-a2a` | 0.6 | A2A agent server (`async_serve` / `build_app`, `Authenticator`) |
| `apcore-cli` | 0.11 | `--man` page generation (`build_program_man_page`) |

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
- `McpServerBuilder`: modules_dir → Registry → Executor → APCoreMCP
- Transports: stdio, streamable-http (was "http"), SSE
- Full MCP protocol compliance via apcore-mcp
- Explorer UI (HTTP transports)
- Transport authentication is a first-class CLI surface (`--auth token|jwt|none`, `--auth-token`, `--jwt-secret`), required by default on the HTTP-family transports; a non-loopback bind without an explicit acknowledgement (`--allow-unauthenticated-bind`) refuses to start. See `docs/user-manual.md` §9.

### Governance (v0.1.0 rewritten, extended through v0.7.0)
- `AclManager`: wraps `apcore::ACL`, generates default rules from annotations; fails closed when a configured ACL is missing/malformed. **Opt-in** — a server started without `--acl` enforces no access control
- `AuditManager`: apexe's own append-only JSONL trail (log `0o600`); records executions, refusals (`event`, `trace_id`, `caller_id`, `error_code`) and ACL allow/deny decisions. No input values, hashed or otherwise — see user-manual §9.2
- `PathGuard` (v0.7.0): resolves every argument the schema types `x-apexe-path` and checks it against two compiled-in baselines — system paths bind writers, credential paths bind readers and writers alike. **Always-on, no flag.** `config.yaml` extends it with `additional_denied_paths` and relaxes it with `allowed_paths`; `apexe policy` reports the installed boundary
- `ApprovalGate`: wraps apcore-mcp's elicitation handler, audits every refusal, and stands down for a call that carries none of the escalating arguments its module was marked for. **Opt-in** via `--enable-approval`; MCP only
- Subprocess isolation: always-on in `module/executor.rs` — env scrubbing, no-shell argv, output cap, timeout + `kill_on_drop`, plus the option-injection, control-character and `x-apexe-exec` parameter guards (no separate SandboxManager)

## Tool Overlays & Variant Detection (v0.4.0, extended in v0.5.0)

- **Variant detection**: every scan probes the binary (`<binary> --version`) and classifies it as `bsd`, `gnu`, `apple`, `busybox` or `unknown`, surfaced as `ScannedCLITool.variant`. Same command name, different implementation (macOS `/bin/ls` vs. Homebrew GNU `ls`) now yields different, correct results instead of one overwriting the other in cache.
- **Curated overlays**: a reviewed description of one `(command, variant, version_range)`, matched by `probe` > `platform` + `binary_globs` > `platform` alone. `mode: authoritative` replaces the scan surface; `mode: merge` overrides matching flags on top of the scan. **apexe ships none**: the corpus is the [cli-permissions](https://github.com/aiperceivable/cli-permissions) repository, read through `overlay_dirs`, `~/.apexe/overlays/*.{json,yaml}`, `apexe scan --overlay <PATH>`, or one of the packaged locations searched by default. Format defined by [`tool-overlay.schema.json`](https://github.com/aiperceivable/cli-permissions/blob/main/schemas/tool-overlay.schema.json), which lives with the corpus. See [`docs/overlays.md`](overlays.md) for the authoring/verification procedure.
- **Provenance requirement**: an overlay at `confidence: verified` must record how it was checked (platform, version, source document, date); the schema rejects a `verified` overlay without it.
- **Per-flag confidence/sources**: every `ScannedFlag` records which tiers produced it plus a derived trust level (`verified` > `high` > `medium` > `low`).
- **Man page `EXAMPLES` extraction** (v0.5.0): `ScannedCLITool.examples` / `CommandContract.examples` carry hand-written invocations pulled from a tool's man page.
- **`open_world` inference** (v0.5.0, breaking): risk annotation is inferred from the executable and networked subcommand names instead of being hardcoded, so `Risk::OpenWorld` is now actually emitted (e.g. `curl`, `git push`/`pull`/`clone`).

## Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `apcore` | Core module system, ACL, errors |
| `apcore-toolkit` | Scanner types, YAML writer, verifiers |
| `apcore-mcp` | MCP protocol server |
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

## Open Items

1. **A2A protocol** -- Implemented in 0.3.0 (`apexe a2a`, `A2aServerBuilder`).
2. **CLI rewiring completion** -- `apexe scan` fully rewired; `apexe serve` uses McpServerBuilder.
3. **Tool overlays & variant detection** -- Implemented in 0.4.0; man page examples and `open_world` inference added in 0.5.0.
4. **F8 local execution** -- Design only; see [`features/v2-f8-local-execution.md`](features/v2-f8-local-execution.md). Stage 1 (`apexe run` with a preflight that reports every governance decision) is ready to build. Stage 2 (PATH shims) is blocked on two contracts the spec names: an `ExecutionIo` mode for passthrough I/O, and a `streaming_stdin_safe` allowlist. Four threat-model additions are reserved for it in [`threat-model.md`](threat-model.md) §5.10.
5. **Per-caller identity over the wire** -- `Identity.roles` is not populated from a JWT claim or header on either surface, so role-gated ACL conditions are unreachable over MCP/A2A. See user-manual §11.
6. **OS-level sandboxing** -- seccomp/landlock, namespaces and cgroup limits remain roadmap. v0.x relies on the governance stack plus process hygiene; see [`threat-model.md`](threat-model.md) §1.
7. **`apexe evo`** -- Deferred. Depends on apevo product maturity.
