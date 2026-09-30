#!/usr/bin/env bash
set -euo pipefail

CLI_PERMISSIONS_DIR="${CLI_PERMISSIONS_DIR:-}"
OUTPUT_DIR="./output/cli-permissions"
BINDINGS_DIR="$OUTPUT_DIR/modules"
ACL_PATH="${HOME}/.apexe/acl.yaml"

if [ -z "$CLI_PERMISSIONS_DIR" ]; then
    echo "Set CLI_PERMISSIONS_DIR to a cli-permissions checkout." >&2
    echo "Example: CLI_PERMISSIONS_DIR=../cli-permissions $0" >&2
    exit 2
fi

OVERLAY_DIR="$CLI_PERMISSIONS_DIR/overlays"
if [ ! -d "$OVERLAY_DIR" ]; then
    echo "No overlays directory found at: $OVERLAY_DIR" >&2
    exit 2
fi

echo "=== Scan commands with verified cli-permissions overlays ==="
echo "Corpus: $OVERLAY_DIR"
APEXE_OVERLAY_DIRS="$OVERLAY_DIR" \
    apexe scan ls grep sort tail --no-cache --output-dir "$BINDINGS_DIR"
echo

echo "=== Review generated bindings and ACL decisions ==="
apexe list --bindings-dir "$BINDINGS_DIR" --verbose --acl "$ACL_PATH"
echo

echo "=== Print MCP configurations that enforce the reviewed ACL ==="
echo "--- Claude Desktop ---"
apexe serve --show-config claude-desktop --bindings-dir "$BINDINGS_DIR" --acl "$ACL_PATH"
echo
echo "--- Cursor ---"
apexe serve --show-config cursor --bindings-dir "$BINDINGS_DIR" --acl "$ACL_PATH"
echo

echo "To serve these bindings after reviewing the ACL:"
echo "  apexe serve --bindings-dir $BINDINGS_DIR --acl $ACL_PATH"
echo "A shell pipeline such as 'ls | grep report' is intentionally unsupported."
