#!/usr/bin/env bash
# apexe basic example: scan tools, inspect results, start MCP server.
set -euo pipefail

OUTPUT_DIR="./output"
MODULES_DIR="$OUTPUT_DIR/modules"

echo "=== Step 1: Scan tools ==="
echo "  ls   — simple tool, runs with {} immediately"
echo "  jq   — 22 flags with full schema (best Explorer demo)"
echo "  curl — 12 flags (GNU format)"
echo
apexe scan ls jq curl --no-cache --output-dir "$MODULES_DIR" --format table
echo

echo "=== Step 2: List generated modules ==="
apexe list --modules-dir "$MODULES_DIR"
echo

echo "=== Step 3: Inspect binding file (check the schema properties) ==="
FIRST_BINDING=$(ls "$MODULES_DIR"/*.binding.yaml 2>/dev/null | head -1)
if [ -n "$FIRST_BINDING" ]; then
    echo "Binding file: $FIRST_BINDING"
    head -50 "$FIRST_BINDING"
    echo "..."
fi
echo

echo "=== Step 4: Integration configs ==="
echo "--- Claude Desktop ---"
apexe serve --show-config claude-desktop --modules-dir "$MODULES_DIR"
echo
echo "--- Cursor ---"
apexe serve --show-config cursor --modules-dir "$MODULES_DIR"
echo

echo "=== Step 5: Start HTTP server with Explorer UI ==="
echo "Open http://127.0.0.1:8000/explorer in your browser."
echo "Click 'cli.curl' → fill in JSON input → click Call."
echo ""
echo "Try in Explorer:"
echo '  cli.ls   → input: {}                        → file listing (instant result!)'
echo '  cli.jq   → input: {"compact_output": true}  → 22 flags with form fields'
echo '  cli.curl → input: {"verbose": true}          → 12 flags with form fields'
echo ""
echo "Press Ctrl+C to stop."
echo
apexe serve --transport http --port 8000 --explorer --modules-dir "$MODULES_DIR"
