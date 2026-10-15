# apexe Documentation

**Governed execution for CLI tools and HTTP APIs.** apexe runs CLI commands
locally and exposes CLI and OpenAPI operations through MCP and A2A, with shared
schema validation, configured access rules and audit records.

Start here according to the job you are trying to complete.

## I want to use apexe

These guides are for operators, developers and teams running CLI tools locally
or exposing CLI and HTTP operations to AI agents. They lead with the outcome, the commands to run, and the
boundaries you must review.

| Goal | Guide |
| --- | --- |
| Use Claude Code, Codex or pi with native exec and optional env | [Agent Skill](agent-skill.md) |
| Initialize CLI coverage, review its ACL, and choose local execution or MCP/A2A | [Quick Start](quickstart.md) |
| Execute CLI commands with module JSON, reviewed native argv or PATH shims | [Local execution example](../examples/local_execution/README.md) |
| Check source drift, explicit dry-runs and earned case evidence | [Verification guide](verification.md) and [runnable example](../examples/verification/README.md) |
| Import an HTTP API and call it through MCP/A2A | [OpenAPI guide](openapi.md) and [runnable example](../examples/openapi_import/README.md) |
| Find every command, configuration key, transport, and troubleshooting step | [User Manual](user-manual.md) |
| Understand what apexe protects and what still needs a sandbox or host policy | [Threat Model](threat-model.md) |
| Follow a runnable terminal walkthrough with ACL enforcement | [Basic example](../examples/basic/README.md) |
| Use verified POSIX command facts from cli-permissions | [cli-permissions example](../examples/cli_permissions/README.md) |
| Inspect the always-on filesystem boundary | [Path guard example](../examples/path_guard/README.md) |
| Read release upgrades and migrations | [Changelog](../CHANGELOG.md) |

The normal user path is:

```text
CLI agent: init → review bindings/ACL → install Agent Skill → native exec
Optional CLI integration: env shims or MCP/A2A protocol serving
API: review OpenAPI source and ACL → serve or a2a with --openapi --acl
```

Local exec/shims load the default ACL when present. Servers require `--acl`
for access control. Native argv/shims require reviewed grammar; HTTP import is
a server feature.

`apexe` is not a sandbox. Read the threat model and run exposed tools inside
the OS, container, VM, account, and network boundary appropriate to your use
case.

## I want to embed, extend, or maintain apexe

These documents are for Rust developers and project maintainers. They describe
implementation choices, public library APIs, parser and overlay authoring, and
historical design records. They are not required to configure or operate the
`apexe` command-line application.

| Goal | Guide |
| --- | --- |
| Find the library API, repository layout, and contributor references | [Developer Guide](developer-guide.md) |
| Embed apexe in a Rust application | [Programmatic example](../examples/programmatic/README.md) |
| Exercise ACL rules with an explicit apcore identity | [ACL library example](../examples/acl_demo/README.md) |
| Author or maintain a verified CLI overlay | [Authoring Tool Overlays](overlays.md) |
| Consume the overlay corpus from another implementation | [Overlay Consumers](overlay-consumers.md) |
| Inspect the current module map | [Feature Manifest](FEATURE_MANIFEST.md) |
| Read architecture records and feature specifications | [Developer Guide](developer-guide.md#architecture-and-feature-records) |
