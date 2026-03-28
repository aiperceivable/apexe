# Basic Example: Scan Git, Serve via MCP

This example scans `git`, generates binding files, and starts an MCP server that Claude Desktop or Cursor can use.

## Prerequisites

- `apexe` installed (`cargo install --path ../..`)
- `git` on your `$PATH`

## Run

```bash
./run.sh
```

## What it does

1. Scans `git` with depth 2 (discovers `git commit`, `git push`, `git status`, etc.)
2. Writes binding files to `./output/modules/`
3. Writes ACL rules to `./output/acl.yaml`
4. Shows the scan results and generated modules
5. Prints Claude Desktop and Cursor integration configs
6. Starts the MCP server on stdio (Ctrl+C to stop)

## Generated files

```
output/
  modules/
    *.binding.yaml     # One file per scanned module
  acl.yaml             # Access control rules (readonly=allow, destructive=deny)
```

## Claude Desktop integration

Copy the output of step 5 into your Claude Desktop config:
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Linux: `~/.config/claude/claude_desktop_config.json`

Then restart Claude Desktop. Git commands will appear as MCP tools.
