<div align="center">
  <img src="./apexe-logo.svg" alt="apexe logo" width="200"/>
</div>

# apexe

Outside-In CLI-to-Agent Bridge — automatically wraps existing CLI tools into governed [apcore](https://github.com/aiperceivable) modules, served via MCP/A2A.

## What is apexe?

`apexe` scans any CLI tool on your system (e.g., `git`, `docker`, `ffmpeg`), extracts its command structure, flags, and arguments, then generates MCP-compatible bindings so AI agents can invoke those tools in a governed way.

**Key capabilities:**

- **Scan** — Three-tier deterministic parser (help text → man pages → shell completions) with 4 built-in parsers (GNU, Click, Cobra, Clap) and a plugin system
- **Bind** — Generates `.binding.yaml` files with JSON Schema input definitions and module IDs (`git commit` → `cli.git.commit`)
- **Serve** — Self-built MCP JSON-RPC 2.0 server over stdio (Claude Desktop / Cursor) or HTTP
- **Govern** — Automatic annotation of destructive vs. readonly commands, default-deny ACL generation, and append-only audit logging

## Installation

Requires Rust 1.70+ and Cargo.

```bash
# Install from crates.io
cargo install apexe

# Or install from source (for development)
cargo install --path .
```

## Quick Start

```bash
# 1. Scan a CLI tool
apexe scan git

# 2. See what was scanned
apexe list

# 3. Serve via MCP (stdio — for Claude Desktop / Cursor)
apexe serve --transport stdio

# 4. Or serve via HTTP
apexe serve --transport http --port 8000
```

## Usage

### `apexe scan <tool> [<tool>...]`

Scan CLI tools and generate binding files.

```bash
apexe scan git docker ffmpeg        # Scan multiple tools
apexe scan git --depth 3            # Recurse 3 levels into subcommands (default: 2, max: 5)
apexe scan git --no-cache           # Force re-scan, bypass cache
apexe scan git --format json        # Output as JSON (also: yaml, table)
apexe scan git --output-dir ./out   # Custom output directory
```

### `apexe serve`

Start MCP server for scanned tools.

```bash
apexe serve                                    # stdio transport (default)
apexe serve --transport http --port 8000       # HTTP transport
apexe serve --show-config claude-desktop       # Print Claude Desktop integration snippet
apexe serve --show-config cursor               # Print Cursor integration snippet
apexe serve --name my-tools                    # Custom server name
```

### `apexe list`

List previously scanned tools and their modules.

```bash
apexe list                  # Table format
apexe list --format json    # JSON format
```

### `apexe config`

Show or initialize configuration.

```bash
apexe config --show     # Print current config (YAML)
apexe config --init     # Create default ~/.apexe/config.yaml
```

### Configuration

Config is resolved in three tiers (highest priority wins):

1. CLI flags
2. Environment variables
3. `~/.apexe/config.yaml`

Default directories:

| Path | Purpose |
|------|---------|
| `~/.apexe/config.yaml` | Configuration file |
| `~/.apexe/modules/` | Generated binding files |
| `~/.apexe/cache/` | Scan cache |
| `~/.apexe/acl.yaml` | Generated ACL rules |
| `~/.apexe/audit.jsonl` | Audit trail |

## How It Works

```
CLI Tool Binary
      │
      ▼
┌─────────────────┐
│  Scanner Engine  │  ← --help parser → man page → shell completions
└────────┬────────┘
         │  ScannedCLITool
         ▼
┌─────────────────┐
│ Binding Generator│  ← module IDs, JSON Schema, YAML output
└────────┬────────┘
         │  .binding.yaml
         ├──────────────────┐
         ▼                  ▼
┌────────────────┐  ┌──────────────┐
│   Governance   │  │ MCP Server   │
│ ACL + Audit    │  │ stdio / HTTP │
└────────────────┘  └──────────────┘
```

**Scanner tiers** (tried in order):
1. `--help` text parsing — GNU, Click/argparse, Cobra, Clap patterns
2. Man page parsing — extracts DESCRIPTION from `man -P cat <tool>`
3. Shell completion parsing — zsh/bash completion scripts for subcommand discovery

**Governance** automatically classifies commands:
- **Readonly** (`list`, `show`, `status`, ...) → auto-allow
- **Destructive** (`delete`, `rm`, `force-push`, ...) → deny (requires explicit allow)
- Flag boosting: `--force` / `--hard` → requires_approval; `--dry-run` → idempotent

## Developer Guide

### Build & Test

```bash
# Build
cargo build

# Run all tests (~393 tests)
cargo test

# Run tests including integration tests that require real CLI tools
cargo test -- --include-ignored

# Lint
cargo clippy --all-targets -- -D warnings

# Format
cargo fmt --all -- --check
```

### Adding a Custom Parser

Implement the `CliParser` trait in `src/scanner/protocol.rs`:

```rust
pub trait CliParser: Send + Sync {
    fn name(&self) -> &str;
    fn can_parse(&self, help_text: &str) -> bool;
    fn parse(&self, help_text: &str) -> Result<ParsedHelp>;
    fn priority(&self) -> u32; // lower = tried first
}
```

Register your parser in the `ParserPipeline`.

### Logging

Uses `tracing` with env filter. Set log level via:

```bash
RUST_LOG=debug apexe scan git
# or
apexe --log-level debug scan git
```

## Documentation

| Document | Description |
|----------|-------------|
| [User Manual](docs/user-manual.md) | Installation, commands, configuration, and troubleshooting guide |
| [Technical Design](docs/tech-design.md) | Architecture, design decisions, and protocol details |
| [Feature Manifest](docs/FEATURE_MANIFEST.md) | Feature decomposition, status, and scope summary |
| [Feature Specs](docs/features/overview.md) | Detailed specifications for each feature (F1–F5) |

## License

Apache-2.0
