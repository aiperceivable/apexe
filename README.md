<div align="center">
  <img src="https://raw.githubusercontent.com/aiperceivable/apexe/main/apexe-logo.svg" alt="apexe logo" width="200"/>
</div>

# apexe

Outside-In CLI-to-Agent Bridge — automatically wraps existing CLI tools into governed [apcore](https://github.com/aiperceivable) modules, served via MCP.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

## What is apexe?

`apexe` scans any CLI tool on your system (e.g., `git`, `curl`, `ls`, `jq`), deterministically extracts its command structure, flags, and arguments — then exposes them as governed MCP tools that AI agents can invoke safely.

**No LLM required for scanning. No changes to the CLI tools. Zero-config governance.**

### Key capabilities

- **Scan** — Three-tier deterministic engine (--help → man pages → shell completions) with 6 built-in parsers (Man, BSD Usage, GNU, Click, Cobra, Clap)
- **Schema** — Generates JSON Schema with type mapping, format hints (`path`, `uri`), defaults, enums, and required fields
- **Serve** — MCP server via [apcore-mcp](https://github.com/aiperceivable/apcore-mcp-rust) (stdio / streamable-http / SSE) with an Explorer UI, plus an A2A agent server via [apcore-a2a](https://github.com/aiperceivable/apcore-a2a-rust). Servers bind localhost by default; transport authentication is available via the apcore library API
- **Govern** — Behavioral annotations (readonly/destructive/idempotent), flag boosting (`--force` → requires_approval), **fail-closed** default-deny ACL, a JSONL audit trail of executions **and** ACL allow/deny decisions (input hashed, log `0o600`), `Module::preview()` dry-run for destructive commands
- **Isolate** — every wrapped subprocess runs with the environment **scrubbed** to a base allowlist (secrets don't leak to tools), no shell (direct argv + injection filtering), output capped at 64 MiB, and a hard timeout that actually kills the process (`kill_on_drop`); circuit breaker + retry middleware on by default, optional `/metrics` + `/usage`
- **AI Guidance** — Every error includes `ai_guidance` to help agents self-correct; non-zero exit codes return stderr context

### Built on the apcore ecosystem

| Crate | Role |
|-------|------|
| [apcore](https://github.com/aiperceivable/apcore-rust) 0.26 | Module trait, Registry, ACL, ModuleError, Context |
| [apcore-toolkit](https://github.com/aiperceivable/apcore-toolkit-rust) 0.10 | ScannedModule, YAMLWriter, DisplayResolver |
| [apcore-mcp](https://github.com/aiperceivable/apcore-mcp-rust) 0.17 | MCP server with middleware, auth, Explorer UI |
| [apcore-a2a](https://github.com/aiperceivable/apcore-a2a-rust) 0.4 | A2A agent server sharing the same governed `Executor` |
| [apcore-cli](https://github.com/aiperceivable/apcore-cli-rust) 0.10 | AuditLogger |

---

## Installation

### Prebuilt binary

No Rust toolchain required. Download the archive for your platform from the
[Releases page](https://github.com/aiperceivable/apexe/releases), verify the
checksum, and put the `apexe` binary on your `PATH`:

```bash
tar -xzf apexe-<target>.tar.gz
shasum -a 256 -c apexe-<target>.tar.gz.sha256
sudo mv apexe-<target>/apexe /usr/local/bin/
apexe --version
```

Available targets: `x86_64-apple-darwin`, `aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

### From source

Requires **Rust 1.75+** and Cargo.

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

### Platform support

apexe scans Unix CLIs. What you get depends on the host:

| Host | Scanning | Curated overlays |
|---|---|---|
| **macOS** | Full — all four tiers | 21 commands (`bsd`, `apple`) |
| **Linux** | Full — install `man-db` for tier 2 | 21 commands (`gnu`) |
| **FreeBSD** | Full | `bsd` overlays apply |
| **OpenBSD / NetBSD / DragonFly** | Full | none — falls back to scanning |
| **Other Unix** (Solaris, illumos, AIX) | Tiers 1–2 | none |
| **Windows** | **Not supported** | none |

The GNU overlays carry no platform condition — they are selected by probing the
binary — so they apply anywhere GNU tools are installed, including WSL and a
macOS box with Homebrew coreutils. The BSD overlays declare `macos` and
`freebsd`, which is why the other BSDs fall back to scanning: their tools are
separately evolved code, not the same source, so claiming otherwise would be a
guess.

**On Windows apexe runs but produces little.** Tier 2 shells out to `man` and
tier 3 to `zsh`; both are absent, so both return nothing rather than failing
loudly. Tier 1 still runs `--help`, but the parsers target Unix conventions
while Windows help is `/?` output with `/X` switches. Expect near-empty results,
not an error message. Native Windows support means a PowerShell reflection path
and a `/?` parser, and is not implemented.

---

## Quick Start

```bash
# Scan git — extracts commands, flags, types, annotations
apexe scan git

# See what was generated
apexe list

# Start MCP server (Claude Desktop / Cursor)
apexe serve

# Or HTTP with browser-based tool explorer
apexe serve --transport http --port 8000 --explorer
```

### Claude Desktop integration

```bash
apexe serve --show-config claude-desktop
# Copy output to ~/Library/Application Support/Claude/claude_desktop_config.json
# Restart Claude Desktop — git commands appear as MCP tools
```

### Cursor integration

```bash
apexe serve --show-config cursor
# Add to Cursor's MCP settings
```

---

## Commands

### `apexe scan <TOOLS>...`

Scan CLI tools and generate binding files + ACL rules.

```bash
apexe scan ls jq curl                   # Scan multiple tools
apexe scan git --depth 3               # 3 levels of subcommands (default: 2, max: 5)
apexe scan git --no-cache              # Force re-scan
apexe scan git --format json           # Output as JSON (also: yaml, table)
apexe scan git --output-dir ./out      # Custom output directory
apexe scan git --skills-dir ./out      # Also write .claude/skills/<id>/SKILL.md per module
apexe scan ls --overlay ./ls.json      # Apply a curated overlay instead of trusting the scan
```

#### Tool variants and overlays

A command name does not identify a program: macOS `/bin/ls` is BSD, Linux
`/usr/bin/ls` is GNU coreutils, Alpine is BusyBox, and Homebrew coreutils puts
GNU `ls` on a macOS box. Every scan probes the binary (`<binary> --version`) and
records the result as `variant`, then looks for a **curated overlay** matching
`(command, variant, version_range)`.

The vocabulary is `bsd`, `gnu`, `apple`, `busybox`, `unknown`. `gnu` is the
whole GNU family, not just coreutils — `diffutils`, `tar`, `sed` and `bash` are
separate projects, and the specific package is recorded in `provenance.package`
and pinned, where it matters, in `match.probe.output_contains`. `apple` covers
Apple's own ports, whose banner names Apple rather than BSD (macOS `sort`
reports `2.3-Apple (197)`, `git` reports `(Apple Git-155)`).

`match.platform`, by contrast, is an **open string** taken verbatim from Rust's
`std::env::consts::OS` — `macos`, `linux`, `freebsd`, `openbsd`, `netbsd`,
`dragonfly`, `solaris` and anything else a host reports. Operating systems are
not an enumerable set, so naming a new one needs no apexe change. There is
deliberately no Linux distribution dimension: GNU coreutils `ls` has the same
83 flags on Debian, Ubuntu and Fedora across three different releases, while
BusyBox `ls` has 21 — divergence tracks the implementation, which `variant`
already captures.

The classification order is load bearing: BSD is tested **before** GNU, because
macOS `grep` reports `grep (BSD grep, GNU compatible) 2.6.0-FreeBSD` and is a
BSD tool advertising GNU compatibility rather than a GNU tool. Apple is tested
only after `<arch>-apple-<os>` target triples are stripped, because
`curl 8.7.1 (x86_64-apple-darwin25.0)` is not an Apple port. See
[Authoring Tool Overlays](docs/overlays.md).

An overlay is a reviewed description of one variant of one command. In
`authoritative` mode it replaces the scan's flags, positional args and
subcommands entirely; in `merge` mode it overrides matching flags and adds
missing ones. Overlays are the only source that can state `conflicts_with`,
because no help or man page format expresses mutual exclusion machine-readably.

Overlays are loaded from, in increasing precedence: the built-ins compiled into
apexe (`overlays/`), `~/.apexe/overlays/*.{json,yaml}`, and `--overlay <PATH>`.
The format is defined by [`schemas/tool-overlay.schema.json`](schemas/tool-overlay.schema.json).

Every emitted flag carries `sources` and a derived `confidence`:
`verified` (overlay) > `high` (completion spec) > `medium` (two independent
heuristic sources agree) > `low` (a single unconfirmed source).

An overlay flag may also declare `long_running: true` — "this option may make
the command never terminate on its own", which is why `tail -f` hangs an agent
until the harness timeout. It reaches the emitted contract as the JSON Schema
extension keyword `x-apexe-long-running`, so an executor can bound the timeout
or refuse instead of blocking. The claim is possibility, not certainty: BSD
`tail -f` returns immediately when its input is a pipe.

**Writing one.** An overlay claiming `confidence: verified` must carry a
`provenance` block recording how it was checked — which build was consulted,
which document was read (`man-page` / `help` / `vendor-docs`), and when. The
schema rejects a `verified` overlay without it, because a `verified` +
`authoritative` overlay replaces the entire scan result with an assertion
nobody can re-check.

Read the flag list off the tool itself, never from memory:

```bash
man -P cat ls | col -b                          # BSD / macOS (strips overstrike)
docker run --rm debian:stable-slim ls --help    # GNU coreutils
docker run --rm alpine ls --help                # BusyBox
```

There is deliberately **no `apexe overlay verify` command**. A tool can compare
flag names, but not descriptions, `conflicts_with`, types or enum values — and
those are the reason overlays exist. A green check covering only the mechanical
half would read as "this overlay is correct".

See **[Authoring Tool Overlays](docs/overlays.md)** for the full procedure,
including how to cross-check a draft against a reference installation and the
traps already hit in practice (of the 38 flags `ls` shares between its BSD and
GNU variants, **zero** have identical descriptions — never copy one across).

### `apexe serve`

Start MCP server for scanned tools.

```bash
apexe serve                                         # stdio (default)
apexe serve --transport http --port 8000             # HTTP
apexe serve --transport http --port 8000 --explorer  # HTTP + browser UI
apexe serve --transport sse --port 8000              # Server-Sent Events
apexe serve --show-config claude-desktop             # Print integration config
apexe serve --name my-tools                          # Custom server name
apexe serve --transport http --metrics                # + /metrics (Prometheus) and /usage
apexe serve --no-circuit-breaker --no-retry           # Disable resilience middleware
```

### `apexe a2a`

Start an A2A agent server for scanned tools. Shares governance (ACL, logging, audit) with `apexe serve` via the same `Executor`.

```bash
apexe a2a                                       # http://127.0.0.1:8000 (default)
apexe a2a --url http://0.0.0.0:9000 --explorer  # Custom bind address + browser UI
apexe a2a --acl ~/.apexe/acl.yaml               # Governed by an ACL policy
apexe a2a --cors-origin https://example.com     # Allow a browser origin
```

> A2A has no interactive elicitation transport, so there is **no `--enable-approval`
> flag** on `apexe a2a` (it's available on `apexe serve`, and on A2A only via the
> library `ApprovalStore` API). See [`docs/user-manual.md`](docs/user-manual.md).

### `apexe list`

List registered modules.

```bash
apexe list                  # Table format
apexe list --format json    # JSON format
```

### `apexe config`

Show or initialize configuration.

```bash
apexe config --show     # Print resolved config (YAML)
apexe config --init     # Create ~/.apexe/config.yaml
```

---

## How It Works

```
CLI Tool Binary
      |
      v
+--------------------+
|   Scanner Engine   |  <-- Tier 1: --help (GNU/Click/Cobra/Clap)
|                    |  <-- Tier 2: man pages (DESCRIPTION + OPTIONS)
|                    |  <-- Tier 3: shell completions (subcommand discovery)
+---------+----------+
          |  ScannedCLITool
          v
+--------------------+
|   Adapter Layer    |  <-- module IDs, JSON Schema, annotations, display metadata
+---------+----------+
          |  ScannedModule (apcore-toolkit)
          |
    +-----+-----+
    |           |
    v           v
+--------+  +------------------+
| Output |  |   MCP Server     |
| .yaml  |  | apcore-mcp       |
| ACL    |  | stdio/http/sse   |
| Audit  |  | middleware+auth   |
+--------+  +------------------+
```

### Behavioral annotations

| Signal | Inference |
|--------|-----------|
| Command `list`, `show`, `status`, `get` | `readonly: true`, `cacheable: true` |
| Command `delete`, `rm`, `kill`, `destroy` | `destructive: true`, `requires_approval: true` |
| Flag `--force`, `-f`, `--hard` | Escalates to `requires_approval: true` |
| Flag `--dry-run`, `--check`, `--simulate` | `idempotent: true` |

### Schema generation

| CLI Type | JSON Schema |
|----------|-------------|
| `--message "hello"` | `"type": "string"` |
| `--count 5` | `"type": "integer"` |
| `--config /path` | `"type": "string", "format": "path"` |
| `--url https://...` | `"type": "string", "format": "uri"` |
| `--format json\|yaml` | `"type": "string", "enum": ["json","yaml"]` |
| `--include a --include b` | `"type": "array", "items": {"type":"string"}` |

---

## Configuration

Resolved in 4 tiers (highest wins): **CLI flags > env vars > config file > defaults**

```bash
apexe config --init    # Creates ~/.apexe/config.yaml
```

| Env Variable | Default | Description |
|-------------|---------|-------------|
| `APEXE_MODULES_DIR` | `~/.apexe/modules` | Binding file storage |
| `APEXE_CACHE_DIR` | `~/.apexe/cache` | Scan cache |
| `APEXE_LOG_LEVEL` | `info` | Log level |
| `APEXE_TIMEOUT` | `30` | CLI subprocess timeout (seconds) |
| `APEXE_SCAN_DEPTH` | `2` | Subcommand recursion depth |

---

## File Locations

| Path | Purpose |
|------|---------|
| `~/.apexe/config.yaml` | Configuration |
| `~/.apexe/modules/*.binding.yaml` | Generated tool bindings |
| `~/.apexe/cache/` | Scan result cache |
| `~/.apexe/acl.yaml` | Access control rules |
| `~/.apexe/audit.jsonl` | Audit trail |

---

## Examples

See [examples/README.md](examples/README.md) for full details.

| Example | Description | Run |
|---------|-------------|-----|
| [basic](examples/basic/) | Shell script: scan → list → serve | `./examples/basic/run.sh` |
| [programmatic](examples/programmatic.rs) | Rust library: scan → convert → export OpenAI tools → build MCP server | `cargo run --example programmatic` |
| [acl_demo](examples/acl_demo/) | Rust library: role-based ACL rules on `CliModule` calls via `Executor` | `cargo run --example acl_demo` |

---

## Developer Guide

### Build & Test

```bash
cargo build                                             # Build
cargo test --all-features                               # Run tests (~338)
cargo test -- --include-ignored                         # Include integration tests
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo fmt --all -- --check                              # Format check
cargo run --example programmatic                        # Run example
```

### Adding a Custom Parser

Implement the `CliParser` trait in `src/scanner/protocol.rs`:

```rust
pub trait CliParser: Send + Sync {
    fn name(&self) -> &str;
    fn can_parse(&self, help_text: &str) -> bool;
    fn parse(&self, help_text: &str, tool_name: &str) -> anyhow::Result<ParsedHelp>;
    fn priority(&self) -> u32; // lower = tried first
}
```

### Logging

```bash
RUST_LOG=debug apexe scan git
apexe --log-level trace scan git
```

### Releasing

`apdev-rs release` handles tagging, the GitHub Release, and publishing to
crates.io; pushing the `rust/vX.Y.Z` tag it creates automatically triggers
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds
the prebuilt binaries described in [Installation](#installation) and attaches
them to that release. No separate step is needed for a normal release.

To backfill binaries onto a release that's missing them (e.g. one published
before this workflow existed), dispatch it manually against the existing tag:

```bash
gh workflow run release.yml -f tag=rust/vX.Y.Z
```

---

## Documentation

| Document | Description |
|----------|-------------|
| **[Quick Start](docs/quickstart.md)** | Get running in 30 seconds |
| **[User Manual](docs/user-manual.md)** | Full reference — commands, config, scanning, schema generation, annotations, governance, MCP server, AI integration, error handling |
| **[Authoring Tool Overlays](docs/overlays.md)** | How to write and verify a curated overlay — required reading before adding one |
| **[Examples](examples/README.md)** | Shell script walkthrough + Rust library API usage |
| **[Changelog](CHANGELOG.md)** | Release history and migration notes |

### Architecture & Design

| Document | Description |
|----------|-------------|
| [Technical Design](docs/apcore-integration/tech-design.md) | v0.1.0 architecture with apcore ecosystem integration |
| [Feature Manifest](docs/FEATURE_MANIFEST.md) | Module map, crate dependencies, project status |
| [Feature Specs](docs/features/v2-overview.md) | Detailed specifications for features F1-F7 |

### Feature Specs

| Spec | Description |
|------|-------------|
| [F1: Scanner Adapter](docs/features/v2-f1-scanner-adapter.md) | ScannedCLITool → ScannedModule conversion |
| [F2: Module Executor](docs/features/v2-f2-module-executor.md) | apcore Module trait for CLI subprocess execution |
| [F3: Binding Output](docs/features/v2-f3-binding-output.md) | apcore-toolkit YAMLWriter integration |
| [F4: MCP Server](docs/features/v2-f4-mcp-server.md) | apcore-mcp server builder |
| [F5: Governance](docs/features/v2-f5-governance.md) | ACL + AuditLogger + Sandbox wrappers |
| [F6: Error Migration](docs/features/v2-f6-error-migration.md) | ApexeError → ModuleError conversion |
| [F7: Config Integration](docs/features/v2-f7-config-integration.md) | apcore Config integration |

---

## License

Apache-2.0
