# apexe Documentation

Start here according to the job you are trying to complete.

## I want to use apexe

These guides are for an operator, developer, or team introducing existing CLI
tools to an AI agent. They lead with the outcome, the commands to run, and the
boundaries you must review.

| Goal | Guide |
| --- | --- |
| Scan a CLI, review its ACL, and connect an MCP client | [Quick Start](quickstart.md) |
| Find every command, configuration key, transport, and troubleshooting step | [User Manual](user-manual.md) |
| Understand what apexe protects and what still needs a sandbox or host policy | [Threat Model](threat-model.md) |
| Follow a runnable terminal walkthrough with ACL enforcement | [Basic example](../examples/basic/README.md) |
| Use verified POSIX command facts from cli-permissions | [cli-permissions example](../examples/cli_permissions/README.md) |
| Inspect the always-on filesystem boundary | [Path guard example](../examples/path_guard/README.md) |
| Read release upgrades and migrations | [Changelog](../CHANGELOG.md) |

The normal user path is:

```text
scan a small CLI → review bindings and ACL → serve with --acl → connect an agent
```

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
