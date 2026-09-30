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
| Add or change a scanner parser | [README extension guidance](../README.md#adding-a-parser) |
| Author a verified CLI overlay | [Authoring Tool Overlays](overlays.md) |
| Consume overlays outside apexe | [Overlay Consumers](overlay-consumers.md) |

## Scope boundary for library examples

The library API can construct an apcore `Context` carrying an explicit
identity, so the ACL example demonstrates role-based policy with `Executor`
calls. The `apexe serve` and `apexe a2a` commands do not currently populate a
distinct identity or role set for every remote caller. Do not present the
library example as ready-made network RBAC for those commands; see the
[user-manual limitation](user-manual.md#911-wrap-a-cli-and-enforce-its-acl).

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
