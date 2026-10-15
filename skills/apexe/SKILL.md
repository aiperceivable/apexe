---
name: apexe
description: Route external CLI commands through native apexe exec by default in an apexe-configured environment. Use for ordinary terminal tasks such as listing files, reading text or running tools when the operator or project requires apexe governance, and for explicit apexe requests. Compatible with Claude Code, Codex and pi; env is optional.
license: Apache-2.0
---

# apexe

Route external CLI execution through `apexe exec -- <command> <arguments>`
using your host's existing terminal tool. Apply this route to ordinary tasks
in an apexe-configured environment even when the user does not mention apexe.
No MCP server or shell activation is needed. Requires apexe 0.9 or later,
installed bindings and matching reviewed grammar for native mode.

## Select the execution route

An environment is in scope when the operator or project instructions declare
apexe-governed CLI execution and identify the intended configuration, or the
user explicitly requests it. Installing this Skill or finding apexe on PATH
alone does not enroll unrelated projects. If the task claims a configured
environment but prerequisites are missing, report the gap; do not silently
fall back to raw execution.

Before running an external program in scope, route it through apexe. For
example, a request to list all directory entries becomes `apexe exec -- ls -la`,
not bare `ls -la`. Preserve the working directory, arguments and task intent.
Prefer native argv; use governed module JSON only for a grammar gap as described
below. Do not wrap an already governed call again.

Keep shell builtins such as `cd`, `test` and `printf` in the host shell. Keep
host-provided file readers, editors and other non-CLI tools on their existing
interfaces; this Skill routes external CLI commands, not every agent tool.
Do not select another tool to evade an apexe refusal. Run apexe's own discovery,
preflight and configuration commands directly without recursive wrapping.
Do not replace an unsupported command or pipeline with `sh -c`, `bash -c`,
an interpreter script or an absolute binary path to bypass admission.

## Discover the environment

Use the configured working directory, binding directory and ACL for the task.
When they are not already known, inspect them once:

```bash
apexe --version
apexe config --show
apexe list --verbose --format json
```

Verbose listings contain a `modules` array with `id`, `input_schema`, annotations
and ACL context. Listing a module does not establish native eligibility or
authorize a concrete invocation. An existing initialization report's
`native_ready` array is useful; otherwise check the intended native call with
`--dry-run`. Current reviewed native coverage includes BSD cat/head/ls; do not
assume GNU variants or other scanned tools are eligible.

If setup is missing, report the missing binary, bindings or policy. When setup
is part of the authorized task, use `apexe init --format json` and review the
resulting bindings and ACL before execution. Initialization writes local state
and may acquire rules from Git; do not repeat it just to inspect eligibility.
Use `scan` for an authorized custom-tool setup. Neither init nor scan proves
complete native grammar or automatically activates shims.

Local execution loads the default ACL when present. If no ACL is loaded,
access control is inactive: do not present such a call as policy-authorized.
Use the operator-provided `--acl` path when needed; if policy is required and
unavailable, report the prerequisite instead of creating permissive rules.

## Execute native argv by default

For a newly selected invocation or changed arguments, preview the same command
you intend to execute, then inspect its checks:

```bash
apexe exec --dry-run -- ls -la
apexe exec -- ls -la
```

Preflight reports `valid`, `mode`, `module_id`, `acl_decision`,
`approval_required`, `checks`, resolved `binary_path`, `argv` and typed path
checks where applicable. Continue only when the invocation is valid, its mode
is `native`, the intended policy allows it, its paths and arguments match the
task, and it does not require an unresolved approval. Treat
`approval_required: true` conservatively: inspect the annotations and explain
the requirement before continuing. For flag-derived annotations it can be a
static ceiling; the execution gate checks the arguments actually sent and may
stand down for a benign call. Do not change annotations to clear the warning.
A successful preflight neither obtains human approval nor reserves permission:
execution checks the call again. Calls that actually need interactive approval
fail closed locally because exec has no connected approval client.

Put all apexe options before `--`; every later token belongs to the child:

```bash
apexe --timeout 10 exec --acl /path/to/reviewed-acl.yaml -- ls -la
```

Pass a literal argument vector when the host supports one. If the host accepts
a shell command string, quote data for that shell and avoid inserting tool
output as executable syntax. Native output passes through and the process
status is the child's status; Unix signals return `128 + signal`. stdout is
not a JSON envelope. A nonzero status can be an ordinary CLI outcome; inspect
stderr and the command's meaning before retrying, especially after possible
side effects.

The shell owns pipes, redirections, expansions and compound commands. Wrap each
external program in an in-scope pipeline independently:

```bash
apexe exec -- cat input.txt | apexe exec -- head -n 5
```

Use the shell's `pipefail` support or collect individual stage statuses when
checking pipeline success. A successful final stage can hide an earlier refusal.

stdin passes through only when the reviewed binding admits streaming input.
A redirection such as `> output.txt` is performed by the shell outside apexe's
typed-path checks. Do not claim an entire pipeline or shell expression was
governed merely because one stage used exec.

## Handle missing grammar and refusals

Distinguish unsupported native grammar from policy, approval, schema or path
refusal. Inspect preflight output and available error guidance. Never respond
to a governance refusal by invoking the original binary directly, using an
absolute path, changing PATH, enabling `--unmanaged allow`, widening the ACL,
or weakening annotations or path rules.

If the task still maps to an advertised module and native grammar alone is
unavailable, use module JSON as a compatible alternative only when its normal
schema, policy, approval and path checks permit the same operation. Read the
advertised property names and types; do not translate flags by guessing:

```bash
# Use the property names and shapes from this machine's binding.
apexe exec cli.ls --input '{"file":["."]}' --dry-run
apexe exec cli.ls --input '{"file":["."]}'
```

Module JSON returns captured execution results, including `exit_code`,
`stdout` and `stderr`, and uses null child stdin. `--input-file request.json`
or `--input-file -` supplies JSON input; it does not stream data to the child.
Those input sources and native argv are mutually exclusive.
`--error-format json` provides a versioned apexe failure envelope on stderr;
keep `--log-stderr` off when parsing it. A completed child's nonzero exit can
still return a normal result instead of an apexe failure envelope. In native
mode, child stderr remains passthrough and is not necessarily JSON.

After a refusal, correct only a demonstrably invalid request within the task's
scope. If the intended operation remains denied, report the reason and the
operator action needed. Do not try spelling variants or another execution
surface to evade the decision. `--as` is a dry-run assertion, not authenticated
impersonation or execution approval.

## Optional shell integration

Use `env` for an authorized existing-script or shell workflow that benefits
from governed native command names. Explicit exec remains the default and
works even without shims. Enabling writes shims and managed shell startup
blocks; it cannot change the parent shell.

```bash
# One-time setup when shell integration is requested.
apexe env --enable
```

After successful setup, activate in the shell that executes commands
(bash/zsh defaults):

```bash
source ~/.apexe/bin/apexe-activate.sh &&
apexe env --check &&
test "$(command -v cat)" = "$HOME/.apexe/bin/cat" &&
test "$(command -v head)" = "$HOME/.apexe/bin/head" &&
cat input.txt | head -n 5
```

Use the helper paths printed by apexe for a custom shim directory. POSIX sh
uses `.` instead of `source`; consult `apexe env --help` for other shells.
The guarded example also checks that both names resolve to the expected shims;
adapt those paths for a custom directory. Only rely on bare names after checking
activation, installed routes and policy. Stop on any failed check so the shell
cannot continue with a raw binary after a failed activation.
If your host starts a new shell for each tool call, source the helper and run
the command in that same call, or keep using explicit exec. A sourced helper in
one process cannot activate later independent processes.

Use `apexe env --reload` after authorized binding changes. It preserves stored
installation settings and enables an absent installation; it is not a read-only
inspection. `apexe env --check` is the inspection command. Deactivation via the
sourced `apexe-deactivate.sh` helper uninstalls owned shims and managed startup
blocks and restores that shell's saved environment; it is not just a temporary
PATH toggle, so do not automatically deactivate an operator's existing setup.

Shims cover eligible external commands only. Builtins, aliases, functions,
absolute paths and uncovered tools can bypass them. Skills guide behavior;
the agent host must enforce any requirement that commands cannot bypass apexe.
Normal exec and shims provide process controls and typed-path checks, not an
OS sandbox. Imported HTTP APIs require MCP/A2A OpenAPI import and are outside
local exec; use those protocols only when the task calls for them.
