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

## Important: this is not shell pipeline support

The following is **not** a supported apexe invocation:

```text
ls | grep report
```

apexe executes one scanned binary per module by direct argv and supplies no
stdin stream. The `|` character is ordinary argument data, never a shell
operator. An agent can call two tools in sequence today, but that is not a
byte-stream pipeline. A real pipeline mode must be implemented as a
declarative no-shell composition that enforces ACL, path guard, timeouts,
output caps, and audit logging independently for every stage.
