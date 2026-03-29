# apexe Quick Start

> Turn any CLI tool into an AI-callable MCP service in 30 seconds.

## Install

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

## 30 seconds: Scan, Serve

```bash
apexe scan git
apexe serve
```

## Claude Desktop Integration

```bash
apexe serve --show-config claude-desktop
```

Copy the output into `~/Library/Application Support/Claude/claude_desktop_config.json`, then restart Claude Desktop.

## Cursor Integration

```bash
apexe serve --show-config cursor
```

Add the output to Cursor's MCP settings.

## HTTP Mode with Explorer UI

```bash
apexe serve --transport http --port 8000 --explorer
```

Open `http://127.0.0.1:8000` in a browser to explore available tools.

## What happened?

1. `apexe scan git` ran git's `--help`, parsed man pages, and checked shell completions.
2. Generated `~/.apexe/modules/git.binding.yaml` (JSON Schema for every subcommand).
3. Generated `~/.apexe/acl.yaml` (readonly commands allowed, destructive commands denied).
4. `apexe serve` started an MCP server on stdio, exposing all scanned tools.

## Scan more tools

```bash
apexe scan ls curl jq
```

## See what you have

```bash
apexe list
apexe list --format json
```

## Next steps

```bash
# Deep scan with 3 levels of subcommands
apexe scan git --depth 3

# HTTP server for remote agents
apexe serve --transport http --port 8000

# SSE transport
apexe serve --transport sse --port 8000

# Initialize a config file for customization
apexe config --init

# View resolved configuration
apexe config --show
```

See [User Manual](user-manual.md) for full documentation.
