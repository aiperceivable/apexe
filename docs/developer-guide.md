# apexe Developer Guide

This guide is for people embedding apexe as a Rust library, extending the
scanner, maintaining curated overlays, or changing the project itself. If you
want to execute CLI commands locally or expose CLI and HTTP operations, start
with the [Quick Start](quickstart.md) and [OpenAPI guide](openapi.md) instead.

## Development entry points

| Task | Starting point |
| --- | --- |
| Maintain or install the portable agent workflow | [Agent Skill guide](agent-skill.md) and [installer](../scripts/install_agent_skill.py) |
| Embed apexe and export generated tool definitions | [Programmatic example](../examples/programmatic/README.md) |
| Test ACL behaviour with an explicit caller identity | [ACL library example](../examples/acl_demo/README.md) |
| Import HTTP operations through the shared executor | [OpenAPI guide](openapi.md) and [runnable example](../examples/openapi_import/README.md) |
| Understand CLI verification and evidence scope | [Verification guide](verification.md) |
| Understand the repository’s current modules and dependencies | [Feature Manifest](FEATURE_MANIFEST.md) |
| Add or change a scanner parser | [README extension guidance](../README.md#adding-a-custom-parser) |
| Author a verified CLI overlay | [Authoring Tool Overlays](overlays.md) |
| Consume overlays outside apexe | [Overlay Consumers](overlay-consumers.md) |

## Scope boundary for library examples

The library API can construct an apcore `Context` carrying an explicit
identity, so the ACL example supplies roles directly to `Executor` calls.
The CLI token mode shares one principal; JWT can establish distinct subjects
and role sets. A network deployment needs transport credentials and ACL
identity/role conditions in addition to the library example. See
[authenticated caller identities](user-manual.md#authenticated-caller-identities).

## Architecture and feature records

The following files are design and implementation references. They preserve
historical decisions and may use crate-level terminology rather than operator
language:

- [apcore integration technical design](apcore-integration/tech-design.md)
- [Feature manifest](FEATURE_MANIFEST.md)
- [Feature index](features/v2-overview.md)
- [Feature specifications](features/)

For current runtime behaviour, prefer the [User Manual](user-manual.md),
[Threat Model](threat-model.md), and [Changelog](../CHANGELOG.md) over an older
feature specification.

The runnable local execution and OpenAPI example smoke tests require Python 3
on Unix. They use its standard library, isolate HOME, and terminate their child
servers. The macOS native example test is opt-in and requires the reviewed
cli-permissions corpus; see `examples/local_execution/README.md`.

Run `make check` for formatting, lint, character validation, the locked build,
the full Rust suite, Rust example tests, validator regression tests, and all
runnable examples. The pre-commit hook uses this same command. Runtime tests
require Python 3.9+, Git, bash and system CLI tools. macOS uses `sandbox-exec`;
Linux needs `bwrap` with working unprivileged user namespaces. The full example
gate also requires `jq`, `curl` and a reviewed cli-permissions checkout:

```bash
make check CORPUS=/path/to/cli-permissions/overlays
```

`CORPUS` defaults to `APEXE_TEST_CORPUS`, then
`../cli-permissions/overlays`. Missing corpus or required isolation fails the
full gate. Runtime checks use temporary HOME/work directories and terminate
owned process groups, including on timeout. They leave JSON receipts under
`target/runtime-validation/`; `RUNTIME_REPORT_DIR` can override this location.

The Rust integration test `cli_runtime_validation` exports the compiled Clap
command tree, option aliases, positional arguments and possible enum values.
It runs `scripts/validate_cli.py` and requires successful behavioral scenarios
for every declared parameter, spelling, subcommand and enum value. Help-only,
invalid-input and failed cases cannot satisfy that coverage. To rerun it:

```bash
cargo test --all-features --locked --test cli_runtime_validation
python3 scripts/validate_cli.py --manifest target/runtime-validation/cli-interface.json
make test-examples CORPUS=/path/to/cli-permissions/overlays
make test-scripts
```

When adding CLI arguments, aliases or enum values, add real scenarios in the
corresponding `validate_*` function in `scripts/validate_cli.py`. New example
entrypoints and Cargo example targets must be registered and exercised in
`scripts/validate_examples.py`. The inventory gate fails if a new entrypoint
has no scenario; include a semantic output assertion rather than only checking
that a process exited successfully.

CI runs the full gates on Linux and macOS, checks out the reviewed corpus,
enforces isolation, runs opt-in tests, and uploads JSON receipts even on failure.
Linux requires actual sh/bash/zsh/fish/PowerShell activation; macOS requires
sh/bash/zsh/fish. Local missing optional shell executables are explicitly
reported as not run. `APEXE_RUNTIME_REQUIRED_SHELLS` can require a comma-separated
set locally. Both platforms run the native synthetic CLI fixture; the real
cat/head shim pipeline is mandatory on macOS because the reviewed streaming
contracts in the current corpus are BSD-only. No Windows runtime coverage is
claimed.

Documentation publication rebases
repository links and publishes the example guides in a separate build tree:

```bash
python3 scripts/prepare_docs.py --output /tmp/apexe-docs
mkdocs build --strict --config-file /tmp/apexe-docs/mkdocs.yml
```

The documentation build requires `mkdocs-material`. It leaves the source docs
unchanged and checks links and heading anchors before publication.

## CLI interface compatibility

Keep command and option names stable: long options use hyphen-separated words,
and the same concept keeps the same name across commands. Scope matters:
`scan --output-dir` writes bindings, while `--bindings-dir` reads them. Introduce
aliases and a documented migration period for future renames.

Treat successful `--format json` output as an API, including empty and filtered
results. Keep explanatory messages out of JSON stdout. Native execution instead
preserves the child's streams and status; structured automation uses module JSON
or the protocol adapters. Every visible argument needs a help description, and
onboarding help should follow initialization, discovery, preview and execution.

Protocol capability claims must match the negotiated version and actual response
fields. Structured-output support belongs to the apcore-mcp SDK, not copied sources
or dependency patches in apexe. The SDK implementation exposes object output
schemas, validates after redaction and retains legacy text. Stdio/SSE use
session negotiation; stateless HTTP uses the protocol header per request.
The normal Cargo dependency uses published apcore-mcp 0.23.0 with apcore 0.32.1,
including local-reference sensitive-field redaction. Run integration tests against
the registry dependencies recorded in Cargo.lock. Client-side annotations are
hints; access control remains an executor decision. See the
[current MCP result contract](user-manual.md#13-error-handling-and-ai-guidance).

Keep generated output schemas aligned with execution: the module envelope is an
object, but optional `json_output` may hold any parsed JSON value. An object-only
constraint rejects valid array/scalar/null output under SDK validation. A2A
assembly restores apexe's configured logging instance after the SDK constructs
its router, so automatic SDK logging cannot override `--no-logging` or
`--no-log-arguments`.

The CLI also suppresses MCP SDK router diagnostics under either logging switch.
That router independently records input values at DEBUG and executor errors at
ERROR, so disabling only the execution middleware does not remove all call
payloads. Apply this filter before formatting or writing diagnostics and keep it
effective even when `RUST_LOG` requests verbose SDK output. Safe server lifecycle
diagnostics and apexe's payload-free failure records keep their existing policy.

CLI JSON failures use `CliErrorEnvelope` with `schema_version: 1`. Preserve
domain codes, details and recovery fields instead of classifying messages.
`--input-file` and `--input` converge before execution and must stay exclusive
with native argv. The file/stdin size limit protects the input reader and never
changes the wrapped tool's stdin contract.
