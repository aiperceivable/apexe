# cli-permissions Example: Verified CLI Facts in a Governed Server

[`cli-permissions`](https://github.com/aiperceivable/cli-permissions) is a
separate corpus of reviewed command-variant facts. It is not an ACL policy or a
sandbox. apexe reads its overlays while scanning, then uses the resulting
bindings and its own reviewed ACL when it serves tools to an agent.

This example scans `ls`, `grep`, `sort`, and `tail` with the corpus enabled.
It makes variant-specific flags, mutual exclusions, behavioural annotations,
and facts such as `tail -f` being potentially long-running available to the
generated bindings. The command binaries still run directly through apexe's
normal execution path when an MCP or A2A client calls them.

## Initialize from managed rules

```bash
apexe init
apexe list --verbose
# Refresh rules and unchanged bindings owned by initialization.
apexe init --refresh
# Refresh an active native shim installation after binding changes.
apexe env --reload
```

Use `init --overlays <SOURCE>` to override the configured source for that
invocation, and `init --ref <REF>` to pin a reviewed branch, tag or commit. The
JSON report (`init --format json`) includes corpus metadata and resolved commits.
Git and CLI binaries must be installed separately. Review the generated ACL
before execution. Custom scans and edited bindings are preserved during init;
use a targeted `scan` when you deliberately want to regenerate them. The
scripted walkthrough below remains a custom scan with an explicit checkout for
reproducibility.

## Local overlay development

From the apexe checkout, use a sibling working directory directly while editing
overlays:

```bash
APEXE_OVERLAY_DIRS=../cli-permissions/overlays apexe scan ls --no-cache
```

This includes uncommitted edits and requires no corpus installation or update.
The variable applies only to this scan. Repeat the scan after changing an
overlay, then use `apexe env --reload` if enabled shims need refreshing.
`apexe init --overlays ../cli-permissions` initializes from a managed snapshot
of the local Git repository's committed content.

## Prerequisites

Install apexe and clone a reviewed revision of the corpus. The revision below
is the one this example was checked against; use a newer revision only after
reviewing its changes.

```bash
cargo install --path .
git clone https://github.com/aiperceivable/cli-permissions.git ../cli-permissions
git -C ../cli-permissions checkout 6686d7aaca2f21dbeeec67c5b7dcda3d7e2bbf54
```

`ls`, `grep`, `sort`, and `tail` must be available on `PATH`.

## Run

```bash
CLI_PERMISSIONS_DIR=../cli-permissions ./examples/cli_permissions/run.sh
```

The script sets `APEXE_OVERLAY_DIRS` only for its own commands, so it does not
change your standing configuration. It scans into `./output/cli-permissions/`,
prints the generated ACL decision for each module, and prints a Claude Desktop
or Cursor-compatible MCP configuration that includes `--acl`.

Before serving a client, inspect `~/.apexe/acl.yaml`. The corpus makes the
binding more precise; it does not grant permission. Keep the generated
default-deny policy and add only the module-level allowances your environment
needs.

## Execution surfaces and pipelines

MCP/A2A captured execution supplies no stdin stream. In a module argument, `|`
is ordinary data and cannot start a shell pipeline. Native PATH shims support
shell pipelines when every stage has a matching authoritative overlay declaring
reviewed native capabilities. The current corpus pilot covers BSD `cat`, `head`
and `ls` on the reviewed macOS build; it does not enable every command above.
See [the native example](../local_execution/README.md) and
[the shell command reference](../../docs/user-manual.md#48-apexe-env).
