# apexe Developer Guide

This guide is for people embedding apexe as a Rust library, extending the
scanner, maintaining curated overlays, or changing the project itself. If you
want to scan and serve CLI tools, start with the [Quick Start](quickstart.md)
instead.

## Development entry points

| Task | Starting point |
| --- | --- |
| Embed apexe and export generated tool definitions | [Programmatic example](../examples/programmatic/README.md) |
| Test ACL behaviour with an explicit caller identity | [ACL library example](../examples/acl_demo/README.md) |
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

Run `make check` for the formatting, lint, character, build and full-test gates.
The pre-commit hook uses this same command. Documentation publication rebases
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
The integration awaits a published SDK release and a normal dependency update.
Client-side
annotations are hints; access control remains an executor decision. See the
[current MCP result contract](user-manual.md#13-error-handling-and-ai-guidance).

CLI JSON failures use `CliErrorEnvelope` with `schema_version: 1`. Preserve
domain codes, details and recovery fields instead of classifying messages.
`--input-file` and `--input` converge before execution and must stay exclusive
with native argv. The file/stdin size limit protects the input reader and never
changes the wrapped tool's stdin contract.
