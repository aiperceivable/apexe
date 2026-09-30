#!/usr/bin/env bash
# apexe basic example: scan tools, inspect results, start MCP server.
set -euo pipefail

OUTPUT_DIR="./output"
BINDINGS_DIR="$OUTPUT_DIR/modules"
ACL_PATH="${HOME}/.apexe/acl.yaml"

echo "=== Step 1: Scan tools ==="
echo "  ls   — short options and an operand, from a curated overlay"
echo "  jq   — boolean flags, parsed from GNU-format help"
echo "  curl — the widest surface here; flag count depends on your build"
echo
apexe scan ls jq curl --no-cache --output-dir "$BINDINGS_DIR" --format table
echo

echo "=== Step 2: List generated modules ==="
apexe list --bindings-dir "$BINDINGS_DIR"
echo

echo "=== Step 3: Inspect binding file (check the schema properties) ==="
FIRST_BINDING=$(ls "$BINDINGS_DIR"/*.binding.yaml 2>/dev/null | head -1)
if [ -n "$FIRST_BINDING" ]; then
    echo "Binding file: $FIRST_BINDING"
    head -50 "$FIRST_BINDING"
    echo "..."
fi
echo

echo "=== Step 4: Review the generated ACL ==="
echo "ACL path: $ACL_PATH"
if [ -f "$ACL_PATH" ]; then
    sed -n '1,160p' "$ACL_PATH"
else
    echo "ACL was not found. Run 'apexe scan' again and inspect your configuration."
fi
echo

echo "=== Step 5: Integration configs with ACL enforcement ==="
echo "--- Claude Desktop ---"
apexe serve --show-config claude-desktop --bindings-dir "$BINDINGS_DIR" --acl "$ACL_PATH"
echo
echo "--- Cursor ---"
apexe serve --show-config cursor --bindings-dir "$BINDINGS_DIR" --acl "$ACL_PATH"
echo

echo "=== Step 6: Start HTTP server with Explorer UI and ACL enforcement ==="
echo "Open http://127.0.0.1:8000/explorer in your browser."
echo "Click 'cli.curl' → fill in JSON input → click Call."
echo ""
echo "Explorer shows only required fields. Replace null with a real value before"
echo "calling a required string input such as curl's URL."
echo ""
echo "Try in Explorer:"
echo '  cli.ls   → {}                                          → listing of the current directory'
echo '  cli.ls   → {"l": true, "file": ["/etc/hosts"]}          → short option + operand'
echo '  cli.curl → {"url": "https://httpbin.org/get", "silent": true}'
echo '              → denied by the generated ACL because curl is open-world'
echo "Review $ACL_PATH before adding a narrow allow rule for a real use case."
echo ""
echo "Press Ctrl+C to stop."
echo
apexe serve --transport http --port 8000 --explorer --bindings-dir "$BINDINGS_DIR" --acl "$ACL_PATH"
