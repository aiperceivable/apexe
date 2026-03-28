# Changelog

All notable changes to apexe are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). Versioning follows [Semantic Versioning](https://semver.org/).

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
