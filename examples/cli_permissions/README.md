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

## Managed corpus installation

With apexe's Git corpus manager, no environment variable is needed:

```bash
apexe overlays install https://github.com/aiperceivable/cli-permissions
apexe overlays list
apexe scan ls grep sort tail --no-cache
# Refresh an installed corpus, then rescan the affected bindings.
apexe overlays update cli-permissions
apexe scan ls grep sort tail --no-cache
# Refresh enabled native shims after rescanning.
apexe env --reload
# Unregister the corpus when it is no longer needed.
apexe overlays remove cli-permissions
```

The optional root `manifest.json` supplies corpus metadata and its overlay
directory. Use `install --ref <REF>` to pin a reviewed branch, tag or commit;
`update` preserves that reference unless explicitly overridden. Git and the CLI
binaries must be installed separately. Review the generated ACL before execution.
The scripted walkthrough below uses an explicit checkout for reproducibility.

## Local overlay development

From the apexe checkout, use a sibling working directory directly while editing
overlays:

```bash
APEXE_OVERLAY_DIRS=../cli-permissions/overlays apexe scan ls --no-cache
```

This includes uncommitted edits and requires no corpus installation or update.
The variable applies only to this scan. Repeat the scan after changing an
overlay, then use `apexe env --reload` if enabled shims need refreshing.
`apexe overlays install ../cli-permissions` serves a different purpose: it
installs a managed snapshot of the local Git repository's committed content.

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
