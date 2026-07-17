# Changelog

All notable changes to apexe are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). Versioning follows [Semantic Versioning](https://semver.org/).

---

## [0.3.0] - 2026-07-17

### Added
- **`apexe a2a`** — New subcommand exposing scanned CLI modules as an A2A agent via `apcore-a2a`, sharing governance (ACL, logging, approval) with `apexe serve` through a common `build_executor()`.
- **Live module annotations** — `CliModule` now implements `Module::annotations()`, so apcore's approval gate reads `requires_approval`/`destructive` from the resolved module instance instead of silently defaulting.
- **Display/alias metadata now reaches the registry** — `ModuleDescriptor.metadata` and `.display` are populated from the scanned binding, activating the MCP/A2A tool-alias resolution (`metadata["display"]["mcp"|"a2a"]["alias"]`) that `DisplayResolver` already computed but was previously discarded at registration.
- **`examples/acl_demo`** — New example (aligned with `axum-apcore/examples/acl_demo`) demonstrating role-based ACL rules (`orders.delete` admin-only, `orders.list` public) enforced on `CliModule` calls via `Executor`.
- **Subprocess hardening** — `execute_subprocess` now caps captured stdout/stderr at 64 MiB (`stdout_truncated`/`stderr_truncated` surfaced in the result) and switched from `std::process::Command` + `spawn_blocking` to `tokio::process::Command` with `kill_on_drop(true)`, so a timed-out subprocess is actually killed instead of leaking as an orphan.
- **`Module::preview()`** — `CliModule` implements apcore's dry-run preview hook for destructive commands, surfacing the exact resolved command line (not a simulated before/after) via apcore-mcp's `__apcore_module_preview` meta-tool.
- **Resilience middleware** — `CircuitBreakerMiddleware` and `RetryMiddleware` are wired into `build_executor` (on by default; `apexe serve`/`apexe a2a --no-circuit-breaker`/`--no-retry` to disable). Retry only ever fires on a timeout for a module annotated `idempotent`, so it can't cause a destructive command to run twice.
- **`SkillOutput` + `apexe scan --skills-dir <DIR>`** — writes a Claude Skill (`SKILL.md`) per module via apcore-toolkit's `ModuleStyle::Skill` formatter, directly usable by Claude Code.
- **`apexe serve --metrics`** — opt-in `/metrics` (Prometheus) and `/usage` (JSON) observability endpoints via `APCoreMCPBuilder::observability`. HTTP/SSE transports only; a no-op warning on stdio.
- **Pluggable approval store** — `ExecutorOptions`/`McpServerBuilder`/`A2aServerBuilder` accept an optional `Arc<dyn ApprovalStore>`, switching a `requires_approval` module's approval flow from the blocking `ElicitationApprovalHandler` to the non-blocking `StorageBackedApprovalHandler`. Library-only (no CLI flag — `InMemoryApprovalStore` isn't shared across process invocations); bring your own persistent store to use it.
- **Governance audit trail (wired end to end)** — `CliModule::execute` records every execution (module_id, status, exit_code, duration; input **hashed**, never stored raw) and `ACL::set_audit_logger` records every allow/deny decision, both appended as JSONL to `config.audit_log`. Previously `AuditManager` and `config.audit_log` were dormant.
- **MCP & A2A serve-path integration tests** — `tests/mcp_integration.rs` / `tests/a2a_integration.rs` drive a real on-disk binding through the request path (`tools/call` executes, injection blocked, agent card exposes the scanned skill). Adds `A2aServerBuilder::agent_card()` (in-process, no port bind — the A2A analog of `McpServerBuilder::export_openai_tools`).

### Changed
- **Upgraded apcore ecosystem** — `apcore = "0.26"`, `apcore-cli = "0.10"`, `apcore-mcp = "0.17"`, `apcore-toolkit = "0.10"` (`default-features = false`), plus new `apcore-a2a = "0.4"`.
- **`ModuleDescriptor` construction rewritten** — matches the reshaped struct (`module_id`, `description`, `version`, `documentation`, `annotations: Option<...>`, etc.) shared by the new MCP and A2A builders via `crate::module::registry::build_executor`.
- **`output/loader.rs`** — `load_modules_from_dir` now delegates to apcore-toolkit's `BindingLoader` (strict mode) instead of a hand-rolled parser, picking up a 16 MiB per-file cap and a 10,000-file per-directory cap for free. Drops the undocumented bare-single-`ScannedModule`-without-a-`bindings`-key fallback, which `YamlOutput` never produced anyway.
- **Timeout retryability moved from `execute_subprocess` to `CliModule::execute`** — a killed subprocess may have partially applied a non-idempotent side effect, so only `CliModule` (which knows the module's `idempotent` annotation) decides whether a timeout is safe to retry.
- **Removed the non-functional `apexe a2a --enable-approval` flag** — A2A has no elicitation transport and no CLI approval-store, so the flag could only ever error at serve time. Approval on A2A remains a library-only feature (supply an `ApprovalStore`); `apexe serve --enable-approval` is unaffected.
- **Parser regexes compiled once (`LazyLock`)** — help-format parsers hoisted per-call `Regex::new` to module-level statics, removing thousands of recompiles on deep subcommand trees.
- **CRLF help normalized at the parser pipeline boundary** — `\r\n`/`\r` help text (e.g. a Windows tool's output) now parses identically to `\n` across all parsers.
- **Specs aligned to implementation** — the F5 `--sandbox`/`SandboxManager` and F4 `--require-auth` designs are removed from the feature specs; subprocess isolation is unconditional (see Security) and transport auth is delegated to the apcore library API with a localhost default.

### Removed
- **`SandboxManager` / `governance::sandbox`** — deleted. `apcore-cli::Sandbox` requires the host binary to re-exec itself with an `--internal-sandbox-runner` subcommand and rediscover modules from `APCORE_EXTENSIONS_ROOT`; apexe's runtime-scanned `CliModule`s don't fit that model. There is no `--sandbox` toggle: subprocess isolation (timeout + kill-on-drop, output caps, no-shell direct argv, **environment scrubbing**) is applied unconditionally in `execute_subprocess`.
- **`nom` dependency** — the unused nom-based `parse_flag_line` GNU parser (~130 LOC, never wired into the parse path) was deleted; GNU help extraction uses regex only.

### Security
- **Environment scrubbing for wrapped subprocesses** — an agent-invoked CLI no longer inherits apexe's full process environment. `execute_subprocess` clears the env and passes only a base allowlist (`PATH`/`HOME`/`USER`/`LOGNAME`/`SHELL`/`LANG`/`LC_*`/`TERM`/`TZ`/`TMPDIR`), so secrets in apexe's environment (API tokens, cloud credentials) cannot leak to a wrapped tool. File-based credentials under `$HOME` still work.
- **ACL fails closed** — `apexe serve`/`apexe a2a --acl <path>` with a missing or malformed file now refuses to start, instead of logging a warning and serving with **no** access control (which apcore treats as allow-all).
- **Audit log hardened to `0o600`** — the JSONL audit log (executions + ACL decisions) is created owner-only, so caller identities and denied targets are not world-readable on shared hosts.

### Fixed
- **`ErrorCode::AclDenied` → `ACLDenied`** rename tracked.
- **ACL audit logger now populated** — the served ACL is built with an audit logger wired to `AuditManager` (previously constructed with `None`), so allow/deny decisions are actually recorded.
- **`apexe scan`/`apexe list` surface failures** — `scan` exits non-zero when a binding/ACL/skill write fails (was warn-and-exit-0); `list` surfaces a corrupt binding's parse error instead of a misleading "No modules found".
- **Resolved `log_level` applied to logging** — `APEXE_LOG_LEVEL` / `config.yaml` `log_level` now feed the tracing subscriber (precedence `RUST_LOG > --log-level > config > "info"`); previously only `--log-level`/`RUST_LOG` took effect.
- **Adapter propagates examples & deduplicates module ids** — scanned `command.examples` now reach the generated module; colliding `module_id`s after subcommand flattening are disambiguated; the output schema advertises `json_output` only when the structured format is JSON.
- **Scanner robustness** — every scan subprocess is timeout-bounded (was only the top-level `--help`); the shared timeout helper drains stdout/stderr concurrently to avoid a deadlock on large output; a discovered subcommand's `help_format` is tagged from the matching parser (was hardcoded `Unknown`); `cache.invalidate()` matches the exact tool name (was an over-broad prefix); the cobra parser handles CRLF section boundaries.
- **Failed/killed executions are audited** — a timed-out or spawn-failed run records a `status=error` audit entry instead of leaving no trace.

---

## [0.2.0] - 2026-03-29

### Added
- **6 new `apexe serve` flags** — `--tags`, `--prefix`, `--acl`, `--enable-approval`, `--no-logging`, `--skip-validation` for full McpServerBuilder control from CLI.
- **Expanded help fallback** — Scanner tries `--help all` and `-h` when `--help` yields few flags. Fixes curl (0 → 12 flags on macOS).
- **GNU regex relaxation** — Flag regex now matches 1-space indent (`\s{1,}`) for curl-style help format.
- **`apexe --man`** — Generates complete roff man page via `apcore_cli::build_program_man_page()`, including commands, flags, exit codes, and docs URL.
- **Documentation URL** — `set_docs_url()` sets `https://github.com/aiperceivable/apexe` in help/man output.
- **Explorer `allow_execute`** — `serve_with_options()` enables tool execution from Explorer UI.
- **Env var test stability** — Global `ENV_LOCK` Mutex prevents parallel test race conditions on environment variables.

### Changed
- **Dependencies switched to crates.io** — `apcore = "0.14"`, `apcore-cli = "0.4"`, `apcore-mcp = "0.11"`, `apcore-toolkit = "0.4"`. Zero path dependencies.
- **ACL opt-in only** — `--acl <path>` required to enable access control. Without it, all tools are allowed (fixes Explorer `AclDenied` issue).
- **`require_auth` removed** — Was silently ineffective without JWT authenticator. Removed to prevent false security assumptions.
- **Config override validation** — CLI `scan_depth` (1-5) and `timeout` (>0) overrides now range-checked with warning on rejection.

### Fixed
- **Explorer UI empty response** — Fixed by using `serve_with_options()` with `ExplorerOptions { explorer: true, allow_execute: true }` instead of `serve()`.
- **clippy `result_large_err`** — Suppressed on `execute_subprocess` (Rust 1.94 stricter closure checking).
- **Flaky env var tests** — Eliminated race condition with `ENV_LOCK` mutex.

---

## [0.1.0] - 2026-03-28

First release with full apcore ecosystem integration.

### Added

**Scanning Engine**
- Three-tier deterministic CLI scanner: `--help` parsing (GNU, Click, Cobra, Clap), man page enrichment (DESCRIPTION + OPTIONS sections), shell completion subcommand discovery
- `ParserPipeline` with automatic format detection and priority routing
- `SubcommandDiscovery` with recursive scanning up to depth 5
- `ScanCache` with JSON filesystem caching and version-based invalidation
- `ToolResolver` with binary path resolution and version detection
- Plugin system via `CliParser` trait for custom parser registration
- Tier 3 completion-discovered subcommands merged back into scan results

**Adapter Layer** (ScannedCLITool → ScannedModule)
- `CliToolConverter`: recursive subcommand tree flattening, dot-separated module IDs (`cli.git.commit`)
- JSON Schema generation with full type mapping: String, Integer, Float, Boolean, Path (`format: path`), URL (`format: uri`), Enum (`enum: [...]`)
- Repeatable flags → array schemas, required flags → `required` array, default value coercion
- Behavioral annotation inference from command names (readonly/destructive patterns) and flag boosting (`--force` → requires_approval, `--dry-run` → idempotent)
- `DisplayResolver` integration: auto-generated MCP aliases (`cli.git.commit` → `git_commit`), per-surface display metadata

**Module Executor**
- `CliModule`: implements apcore `Module` trait for CLI subprocess execution
- Async execution via `tokio::task::spawn_blocking` with `tokio::time::timeout`
- Shell injection prevention: 15-character blacklist validated at construction time and runtime
- Context integration: `trace_id`, `identity`, `duration_ms` propagated through execution
- `ai_guidance` on non-zero exit codes with stderr context for AI self-correction

**MCP Server**
- `McpServerBuilder`: fluent API for module loading → Registry → Executor → APCoreMCP
- Transports: stdio (Claude Desktop/Cursor), streamable-http, SSE
- `LoggingMiddleware` enabled by default (structured logging with redaction)
- `ElicitationApprovalHandler` for interactive destructive command approval (opt-in)
- Tags and prefix filtering for tool access control
- Explorer UI support (HTTP transports)
- `export_openai_tools()`: OpenAI function calling format export
- Config snippet generation for Claude Desktop and Cursor

**Governance**
- `AclManager`: wraps `apcore::ACL`, auto-generates default-deny rules from annotations
- `AuditManager`: wraps `apcore_cli::AuditLogger`, JSONL append-only with SHA-256 input hashing
- `SandboxManager`: wraps `apcore_cli::Sandbox`, subprocess isolation with timeout enforcement

**Output**
- `YamlOutput`: wraps `apcore_toolkit::YAMLWriter` with optional verification
- `load_modules_from_dir()`: reads `.binding.yaml` files as `Vec<ScannedModule>`

**Configuration**
- 4-tier config resolution: CLI flags > env vars > config file > defaults
- 5 environment variables: `APEXE_MODULES_DIR`, `APEXE_CACHE_DIR`, `APEXE_LOG_LEVEL`, `APEXE_TIMEOUT`, `APEXE_SCAN_DEPTH`
- Optional `apcore::Config` integration via `~/.apexe/apcore.yaml`
- Range validation on all numeric config overrides

**Error Handling**
- `From<ApexeError> for ModuleError`: all 9 error variants mapped with `ai_guidance`
- `into_module_error_with_trace()` for trace_id attachment
- Rich error display in CLI with suggestion text

**CLI**
- `apexe scan <TOOLS>...` — scan with --depth, --no-cache, --format, --output-dir
- `apexe serve` — --transport, --host, --port, --explorer, --name, --show-config
- `apexe list` — --format, --modules-dir
- `apexe config` — --show, --init

**Documentation**
- Quick Start guide (`docs/quickstart.md`)
- Full User Manual with 15 chapters (`docs/user-manual.md`)
- Technical Design document (`docs/apcore-integration/tech-design.md`)
- 7 feature specifications (`docs/features/v2-f1..f7`)
- Feature Manifest with module map (`docs/FEATURE_MANIFEST.md`)

**Examples**
- `examples/basic/` — Shell script walkthrough: scan → list → serve
- `examples/programmatic.rs` — Rust library API: scan → convert → export → build server

### Dependencies

| Crate | Version | Role |
|-------|---------|------|
| apcore | 0.14 | Core types: Module, Registry, ACL, ModuleError, Config |
| apcore-toolkit | 0.4 | ScannedModule, YAMLWriter, DisplayResolver, Verifier |
| apcore-mcp | 0.11 | MCP server: APCoreMCP, transports, auth, Explorer |
| apcore-cli | 0.3 | AuditLogger, Sandbox |

### Stats

- 39 source files, ~8,850 lines of Rust
- 338 tests, 0 failures
- All quality gates pass: `cargo fmt`, `cargo clippy -D warnings`, `cargo test --all-features`
