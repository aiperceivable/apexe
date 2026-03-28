# apexe Examples

> Hands-on examples for every apexe use case — from 30-second quick start to full Rust library integration.

## Overview

| Example | Type | What You'll Learn | Run |
|---------|------|-------------------|-----|
| [basic](basic/) | Shell script | Scan a tool, inspect results, start MCP server | `./examples/basic/run.sh` |
| [programmatic](programmatic.rs) | Rust code | Use apexe as a library: scan → convert → export → serve | `cargo run --example programmatic` |

## Prerequisites

```bash
# From the repo root
cargo install --path .
apexe --version
# apexe 0.1.0
```

---

## Example 1: basic — Full CLI Walkthrough

### What it does

A shell script that walks through the complete apexe workflow:

```
scan git → write bindings → write ACL → list modules → print configs → start server
```

### Run it

```bash
cd examples/basic
./run.sh
```

### Expected output

```
=== Step 1: Scan git ===
Tool: git (2.39.5)
  Binary: /usr/bin/git
  Scan tier: 2
  Subcommands: 12
  Global flags: 3

=== Step 2: List generated modules ===
MODULE ID                                DESCRIPTION
──────────────────────────────────────── ────────────────────────────────────────
cli.git.add                              Add file contents to the index
cli.git.commit                           Record changes to the repository
cli.git.push                             Update remote refs
cli.git.status                           Show the working tree status
...

12 module(s) found.

=== Step 3: Inspect ACL ===
rules:
  - callers: ["*"]
    targets: ["cli.git.status", "cli.git.log", "cli.git.diff"]
    effect: allow
    description: Auto-allow readonly CLI commands
  - callers: ["*"]
    targets: ["cli.git.rm"]
    effect: deny
    description: Block destructive CLI commands by default
    conditions:
      require_approval: true
default_effect: deny
```

### Tip: Force re-scan for better results

If git shows 0 subcommands, the cache has a stale result. Force re-scan:

```bash
apexe scan git --no-cache --depth 3
```

### Generated files

```
output/
  modules/
    cli.git.binding.yaml     # JSON Schema for every git subcommand
~/.apexe/
  acl.yaml                   # Access control rules
```

---

## Example 2: programmatic — Rust Library API

### What it does

Shows how to embed apexe in your own Rust application:

1. Scan CLI tools programmatically
2. Convert scan results to apcore `ScannedModule`
3. Write binding YAML files
4. Export OpenAI-compatible tool definitions
5. Build an MCP server (without starting it)

### Run it

```bash
cargo run --example programmatic
```

### Expected output

```
=== Scanning 'echo' (a simple tool for demonstration) ===
Scanned: echo (tier 1, 0 subcommands, 0 global flags)

=== Converting to ScannedModules ===
Module: cli.echo — Execute echo
  readonly=false, destructive=false, requires_approval=false
  display alias: echo

=== Writing binding files ===
Written: /Users/you/.apexe/modules/cli.echo.binding.yaml

=== Exporting OpenAI-compatible tool definitions ===
1 tool(s) exported:
[
  {
    "function": {
      "description": "Execute echo",
      "name": "cli-echo",
      "parameters": { "type": "object", "properties": {} }
    },
    "type": "function"
  }
]

=== Building MCP server (not starting) ===
MCP server built successfully. Call server.serve() to start.
```

### Key code snippets

**Scan a tool:**
```rust
use apexe::config::ApexeConfig;
use apexe::scanner::ScanOrchestrator;

let config = ApexeConfig::default();
let orchestrator = ScanOrchestrator::new(config);
let tools = orchestrator.scan(&["git".to_string()], false, 2)?;
```

**Convert to ScannedModules:**
```rust
use apexe::adapter::CliToolConverter;

let converter = CliToolConverter::new();
let modules = converter.convert_all(&tools);
// Each module has: module_id, input_schema, output_schema, annotations, display metadata
```

**Write binding YAML:**
```rust
use apexe::output::YamlOutput;

let yaml = YamlOutput::new(); // with verification
let results = yaml.write(&modules, output_dir, false)?;
```

**Export OpenAI function calling format:**
```rust
use apexe::mcp::McpServerBuilder;

let tools = McpServerBuilder::new()
    .modules_dir("/path/to/modules")
    .export_openai_tools()?;
// Returns Vec<serde_json::Value> in OpenAI function_tools format
```

**Build and start MCP server:**
```rust
let server = McpServerBuilder::new()
    .name("my-tools")
    .transport("http")            // or "stdio", "sse"
    .port(8000)
    .explorer(true)               // browser-based tool explorer UI
    .modules_dir("/path/to/modules")
    .enable_logging(true)         // structured logging middleware
    .enable_approval(true)        // approval dialog for destructive commands
    .tags(vec!["readonly".into()]) // only expose readonly tools
    .build()?;

server.serve()?; // blocking — starts the server
```

---

## Common Scenarios

### Scan multiple tools at once

```bash
apexe scan git curl grep find lsof --depth 3
apexe list
# Shows all modules from all tools
```

### Start HTTP server with Explorer UI

```bash
apexe serve --transport http --port 8000 --explorer
# Open http://127.0.0.1:8000/explorer in your browser
# Browse tools, view schemas, test invocations
```

### Claude Desktop integration

```bash
# 1. Scan tools
apexe scan git curl grep

# 2. Get integration config
apexe serve --show-config claude-desktop
# Output:
# {
#   "mcpServers": {
#     "apexe": {
#       "command": "apexe",
#       "args": ["serve", "--transport", "stdio"]
#     }
#   }
# }

# 3. Copy to config file
# macOS: ~/Library/Application Support/Claude/claude_desktop_config.json
# Linux: ~/.config/claude/claude_desktop_config.json

# 4. Restart Claude Desktop — tools appear in MCP tool list
```

### Cursor integration

```bash
apexe serve --show-config cursor
# Add the JSON to Cursor's MCP settings
```

### Custom output directory

```bash
apexe scan git --output-dir ./my-project/tools
apexe serve --modules-dir ./my-project/tools
```

### View generated JSON Schema

```bash
apexe scan git --format json | jq '.subcommands[0].flags'
# Shows parsed flags with types, defaults, descriptions
```

### Check what annotations were inferred

```bash
apexe scan git --format yaml | grep -A5 "annotations"
# readonly: true/false, destructive: true/false, requires_approval: true/false
```

---

## Troubleshooting

### `Using cached scan result` with 0 subcommands

Cache has a stale entry. Force re-scan:

```bash
apexe scan git --no-cache --depth 3
```

Or clear the entire cache:

```bash
rm -rf ~/.apexe/cache/
```

### `Tool not found on PATH`

The tool must be installed and accessible:

```bash
which git    # should print a path
apexe scan git
```

### MCP server does nothing in stdio mode

Stdio mode communicates via stdin/stdout — it's meant to be launched by AI agents, not run interactively. Use `--show-config` to get the agent integration snippet, or use HTTP mode for manual testing:

```bash
apexe serve --transport http --port 8000 --explorer
```

### Permission denied in ACL

The default ACL denies destructive and unknown commands. Edit `~/.apexe/acl.yaml`:

```yaml
rules:
  - callers: ["*"]
    targets: ["cli.git.push"]
    effect: allow
    description: "Allow git push"
default_effect: deny
```

---

## Adding Your Own Example

### Rust example

Create a `.rs` file in `examples/` — Cargo auto-discovers it:

```bash
# examples/my_example.rs
cargo run --example my_example
```

Key imports:

```rust
use apexe::adapter::CliToolConverter;
use apexe::config::ApexeConfig;
use apexe::governance::{AclManager, AuditManager};
use apexe::mcp::McpServerBuilder;
use apexe::module::CliModule;
use apexe::output::{YamlOutput, load_modules_from_dir};
use apexe::scanner::ScanOrchestrator;
```

### Shell script example

Create a directory under `examples/` with a `run.sh` and `README.md`:

```
examples/my-scenario/
  README.md
  run.sh        # chmod +x
```
