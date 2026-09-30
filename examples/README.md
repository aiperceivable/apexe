# apexe 0.8.0 Examples

Choose an example based on whether you are operating the `apexe` command or
developing against its Rust library.

## Use apexe from the terminal

| Example | What you will do | Run |
| --- | --- | --- |
| [basic](basic/) | Scan real CLI tools, review the generated ACL, print client configuration, and start a governed Explorer server | `cd examples/basic && ./run.sh` |
| [cli_permissions](cli_permissions/) | Scan commands using the verified external cli-permissions corpus, review the resulting ACL, and print a governed MCP configuration | `CLI_PERMISSIONS_DIR=/path/to/cli-permissions ./examples/cli_permissions/run.sh` |
| [path_guard](path_guard/) | Inspect the always-on filesystem boundary and its read/write behaviour | `cargo run --example path_guard` |

Start with the [basic walkthrough](basic/README.md). It is the supported user
path: scan a small set of tools, review the generated policy, and serve with
`--acl`. Do not remove `--acl` merely to make a denied call succeed; review and
narrowly change the policy for the use case instead.

## Develop or embed apexe

| Example | What you will learn | Run |
| --- | --- | --- |
| [programmatic](programmatic/README.md) | Scan and convert a CLI from Rust, write bindings, export tool definitions, and build an MCP server | `cargo run --example programmatic` |
| [acl_demo](acl_demo/) | Apply role-based ACL rules to `CliModule` calls using an explicit apcore `Context` | `cargo run --example acl_demo` |

These are library examples. In particular, `acl_demo` supplies caller roles
directly to the Executor. The built-in `apexe serve` and `apexe a2a` commands
do not currently populate distinct roles for remote callers, so this is not a
copy-and-paste network RBAC deployment. See the
[Developer Guide](../docs/developer-guide.md) for the full boundary.

## Prerequisites

Install apexe from the repository root, then run the user or developer example
that matches your goal:

```bash
cargo install --path .
apexe --version
```

The basic example requires `ls`, `jq`, and `curl` on `PATH`. The exact set of
generated modules can vary by operating system and installed CLI version.
