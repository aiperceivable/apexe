#!/usr/bin/env bash
# apexe basic example: scan git, inspect results, start MCP server.
set -euo pipefail

OUTPUT_DIR="./output"
MODULES_DIR="$OUTPUT_DIR/modules"

echo "=== Step 1: Scan git ==="
apexe scan git --output-dir "$MODULES_DIR" --depth 2 --format table
echo

echo "=== Step 2: List generated modules ==="
apexe list --modules-dir "$MODULES_DIR"
echo

echo "=== Step 3: Inspect ACL ==="
if [ -f "$HOME/.apexe/acl.yaml" ]; then
    echo "ACL rules at ~/.apexe/acl.yaml:"
    cat "$HOME/.apexe/acl.yaml"
else
    echo "(ACL file not found — may have been written to default location)"
fi
echo

echo "=== Step 4: Inspect a binding file ==="
FIRST_BINDING=$(ls "$MODULES_DIR"/*.binding.yaml 2>/dev/null | head -1)
if [ -n "$FIRST_BINDING" ]; then
    echo "First binding file: $FIRST_BINDING"
    head -40 "$FIRST_BINDING"
    echo "..."
fi
echo

echo "=== Step 5: Integration configs ==="
echo "--- Claude Desktop ---"
apexe serve --show-config claude-desktop --modules-dir "$MODULES_DIR"
echo
echo "--- Cursor ---"
apexe serve --show-config cursor --modules-dir "$MODULES_DIR"
echo

echo "=== Step 6: Start MCP server (stdio) ==="
echo "Press Ctrl+C to stop."
echo "In production, this is launched by Claude Desktop / Cursor, not run manually."
echo
apexe serve --transport stdio --modules-dir "$MODULES_DIR"
