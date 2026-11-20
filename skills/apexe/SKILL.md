---
name: apexe
description: Route external CLI commands through explicit apexe exec when a project or operator has selected apexe and the agent lacks a configured shell or hook. Use the configured entry point directly when shell or MCP integration is already active.
license: Apache-2.0
---

# apexe

Use the execution route already selected for this project or task.

- **Configured terminal integration.** For commands covered by `apexe-sh`, PATH
  shims or the Claude Code `PreToolUse` hook, use the host terminal with ordinary
  command syntax. The integration routes the call to apexe. Treat an uncovered
  command as a coverage gap when the task requires governed execution.
- **Configured MCP integration.** Use the advertised tool and input schema
  within the configured MCP surface. A refusal ends the attempted operation
  through that surface and any alternative route.
- **Explicit exec fallback.** Use the steps below when project instructions or
  the operator select apexe-governed terminal execution and there is no
  configured shell or hook for the workflow. Mere discovery of apexe or this
  Skill does not enroll a project.

## Select the fallback configuration

Before the first task CLI call, obtain the applicable project instructions
(`CLAUDE.md` or `AGENTS.md`) and this Skill through the host's instruction
loader or file reader. Use complete instructions already supplied by the
host. For a terminal-only host, use shell builtins with input redirection to
read the discovered instruction paths. Stop if required material is missing,
truncated or refused. The first task call includes directory listings,
orientation and readonly probes.

Use the working directory, bindings and ACL selected by the project or
operator. For the configured default ACL, leave `--acl` out of the command
even when the project names the default file. Apexe loads that file
automatically. Use `--acl` only when the project or operator explicitly
selects a different policy. Preserve any explicit `--acl` or `--bindings-dir`
override through every preview, execution and pipeline stage. In native mode,
put those options before `--`. If a submitted call omitted a required
override, stop and report what already ran under the wrong configuration.

For discovery, run apexe itself directly. `apexe config --show` reports
configuration, and `apexe list --verbose --format json` reports module IDs,
schemas and ACL context. Supply the selected binding and ACL overrides to
`list` when applicable. Check `native_ready` in an existing init report or
preflight the invocation to establish native coverage. If the selected
policy is unavailable or the host refuses access to it, report that
prerequisite. A preview with `acl_decision: no_acl_loaded` cannot authorize work.

## Preview and execute

Route every external program in scope through apexe, including external
stages of compound commands. Shell builtins and native host file tools keep
their own interfaces. Preserve literal arguments and quote shell strings
for the host.

For a new invocation or changed arguments, submit the concrete preflight as
the sole tool request in its assistant response. Read the result before
requesting execution in a separate response.

```bash
apexe exec --dry-run -- ls -la
```

Execute the same arguments and configuration when the plan has `valid: true`,
`mode: native`, `acl_decision: allowed`, matching paths and arguments, and no
unresolved approval.

```bash
apexe exec -- ls -la
```

Preflight cannot grant approval. For flag-derived annotations,
`approval_required` may report a static ceiling. The gate checks the actual
arguments during execution. Native local execution can prompt on a
controlling terminal and refuses when one is required and unavailable.

For pipelines, preview one stage at a time in order. Briefly state each
stage's decision after reading its result. Request each later stage in a
new response. Keep preflights, execution and task tracking in separate
responses. For example, preview `apexe exec --dry-run -- cat input.txt` and
`apexe exec --dry-run -- head -n 2`. After both are admitted, use a shell with
`pipefail` or collect all stage statuses.

```bash
set -o pipefail && apexe exec -- cat input.txt | apexe exec -- head -n 2
```

The shell handles redirections outside apexe's typed-path checks. Native
execution passes through child streams and exit status. Streaming stdin
requires support in the reviewed binding.

## Respond to coverage gaps and refusals

For a native grammar gap alone, use the equivalent advertised module JSON
operation when its schema, ACL, path and approval checks permit it. Preview
with the same configuration and require `mode: module` and an allowed ACL
decision before execution. Module JSON captures the result and supplies null
stdin to the child. A child exit status may describe an ordinary command
outcome. Check stderr before deciding what it means.

On the first apexe governance refusal in fallback mode, or a host permission
refusal that prevents loading required instructions or policy, stop all tool
use for the task. Return a text report with the reason, completed work,
remaining work and the operator action needed. This includes stopping later
pipeline stages, retries, cleanup, plan updates and task tracking. Preserve
the same authorization boundary across raw CLI, other tools and execution
surfaces.
