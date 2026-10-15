# apexe 0.9.0 Examples

Explore governed CLI execution, contract verification and HTTP API import.
Choose an example based on whether you are operating the `apexe` command or
developing against its Rust library.

## Use apexe from the terminal

| Example | What you will do | Run |
| --- | --- | --- |
| [basic](basic/) | Scan real CLI tools, review the generated ACL, print client configuration, and start a governed Explorer server | `cd examples/basic && ./run.sh` |
| [cli_permissions](cli_permissions/) | Scan commands using the verified external cli-permissions corpus, review the resulting ACL, and print a governed MCP configuration | `CLI_PERMISSIONS_DIR=/path/to/cli-permissions ./examples/cli_permissions/run.sh` |
| [local_execution](local_execution/) | Run captured governance and optionally install, exercise and remove native PATH shims in a temporary home | `python3 examples/local_execution/demo.py` |
| [verification](verification/) | Verify isolated dry-run/fixture cases and invalidate stale confidence in a disposable corpus | `python3 examples/verification/demo.py` |
| [openapi_import](openapi_import/) | Import a disposable authenticated HTTP API through MCP/A2A and prove ACL and credential separation | `python3 examples/openapi_import/demo.py` |
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
can populate remote identities and roles through JWT authentication. The library
example supplies them directly; a network deployment also needs JWT credential
configuration and transport ACL conditions. See the
[Developer Guide](../docs/developer-guide.md) for the full boundary.

## Prerequisites

Install apexe from the repository root, then run the user or developer example
that matches your goal:

```bash
cargo install --path .
apexe --version
```

The basic example requires `ls`, `jq`, and `curl` on `PATH`. The local execution
and OpenAPI examples require Python 3; their own READMEs show how to pass the
repository-built executable without installing it. The exact set of
generated modules can vary by operating system and installed CLI version.

## Automated validation

`make check CORPUS=/path/to/cli-permissions/overlays` builds and tests the Rust
examples and executes every example using isolated homes and local services.
For a focused rerun after building, use `make test-examples` with the same
`CORPUS` setting. Results are saved to `target/runtime-validation/examples.json`;
failures return nonzero. Missing required isolation or corpus is a failure.
The CI matrix enforces the full walkthroughs on Linux and macOS, including the
BSD native pipeline on macOS. See the [Developer Guide](../docs/developer-guide.md)
for prerequisites, platform scope and instructions for adding a new example.
