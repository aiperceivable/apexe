---
name: apexe
description: Route external CLI commands through native apexe exec in an apexe-configured environment. Resolve project policy before task commands, preserve the selected ACL, inspect each preflight in a separate response, and stop all tools on refusal, including task tracking. Compatible with Claude Code, Codex, pi and OpenCode; env is optional.
license: Apache-2.0
---

# apexe

Route external CLI execution through `apexe exec -- <command> <arguments>`
using your host's existing terminal tool. Apply this route to ordinary tasks
in an apexe-configured environment even when the user does not mention apexe.
No MCP server or shell activation is needed. Requires apexe 0.9 or later,
installed bindings and matching reviewed grammar for native mode.

For every preflight, submit exactly one tool request in that assistant response:
the preflight itself. Put task tracking or other housekeeping in an earlier,
separate response, never alongside a potentially refused call. For a pipeline,
preview one stage at a time. Wait for its result, read the returned checks, and briefly
state that stage's admission or refusal before deciding the next call in a new
assistant response. Never emit `cat` and `head` preflights together. Two separate
tool calls in one response are a batch, even if the host executes them in order;
host serialization does not give the model a chance to inspect the first result.
Do not use a batch tool, parallel dispatch or a loop that queues later stages.
On refusal, stop with no remaining stages or other tools requested or queued.

## Stop all tools after refusal

On the first governance or required host-permission refusal, stop ALL tool use
for that task and return a text report only. This includes `todowrite`,
`update_plan`, task-status changes, file readers/editors, discovery commands,
cleanup and bookkeeping tools, even if their purpose is to mark work refused
or cancel unfinished steps. Describe completed and unfinished work in the
report without a tool call. Do not request a later pipeline stage or use another
execution surface to finish the refused operation.

Check that no other tool is queued with a preflight before submitting it.
Stopping CLI execution alone is insufficient if a tracking tool runs afterward.
Do not repair, retry or investigate the refusal through tools in the refused
task; the operator can authorize a subsequent task to resolve the prerequisite
or review policy. Missing native grammar alone and ordinary child nonzero
statuses are distinct from governance refusal, as described below.

## Resolve project policy before task commands

Loading this Skill is not sufficient to select the task's policy. Before any
task CLI call, including a readonly probe or `--dry-run`, retrieve the complete
applicable project instructions (`CLAUDE.md` for Claude Code; `AGENTS.md` for
Codex, pi or OpenCode) using the host reader or the builtin loading method below.
If the host already supplied their full contents, use those contents. A file
name, instruction summary or Skill description does not establish configuration.
In Claude Code, use `Read` for the applicable `CLAUDE.md` when its full contents
are not available; do not list the directory first to discover policy afterward.
Stop if required instructions are missing, truncated or refused by the host.

Resolve the working directory, bindings and ACL from those instructions and
the operator's task-specific configuration. Keep a configuration record for
the task: the selected paths, whether each uses an explicit override or an
instructed default, and the instruction that selected it. Resolve conflicting
selections before execution. Use defaults only when the applicable instructions
select them; a successful call under a default policy does not validate a
different operator-selected policy.

When an ACL path is selected, every task `apexe exec` must contain
`--acl <selected-path>` before `--`, starting with the first preflight. Carry
explicit binding overrides the same way. Check the actual command you are
about to submit against this configuration record. Include the selected ACL
in each pipeline stage, not just one stage or the preflight. Do not run a
default-policy trial before using the selected policy, and do not infer policy
selection from `acl_decision: allowed`: that field reports a decision, not
which ACL file was loaded. Read the selected ACL through the host reader when
needed to understand its rules; reading it later cannot authorize earlier calls.

The unqualified examples below apply only when the task selects the default
configuration. For a task selecting `/workspace/reviewed-acl.yaml`, the separate
preflight and execution calls instead use:

```bash
apexe exec --acl /workspace/reviewed-acl.yaml --dry-run -- ls -la
# Inspect the result before a separate execution call with the same ACL.
apexe exec --acl /workspace/reviewed-acl.yaml -- ls -la
```

Use the actual operator-provided path, never the example path. If you discover
that a submitted call omitted the selected ACL, stop and report the routing
error and any operation that already ran. Repeating it with the correct ACL
does not repair the earlier execution or make the task compliant.

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

### Route before the first terminal call

Instruction loading is part of the routing boundary. Prefer the host's Skill
loader or file reader. If only a terminal is available, read the discovered
Skill file with shell builtins and input redirection, without spawning a
reader program. For example, after setting `skill_path` to the host-discovered
installed entry point, retrieve the complete contents, continuing with another
read if the host truncates them. The name or frontmatter alone is insufficient:

```bash
skill_text=
while IFS= read -r line || test -n "$line"; do
  skill_text="$skill_text$line
"
done < "$skill_path"
printf '%s' "$skill_text"
unset skill_text
```

Emit the retrieved text once rather than writing each line separately, so a
streaming terminal host can capture the complete instructions in one result.
Do not bootstrap with bare `sed`, `cat`, `head` or an interpreter. These are
external CLI calls even when they only read instructions. Do not switch readers
after a host permission refusal. Project instructions must provide this loading
rule before the Skill body is available; a rule inside an unread Skill cannot
govern its own loading. This builtin read is for retrieving the selected
instructions, not for reading task data around an apexe refusal.

Initial inspection, orientation, readonly probes and quick checks are already
CLI execution. Do not run a raw command to prepare for a later governed call.
Loading this Skill does not itself govern subsequent commands: rewrite the
execution route before submitting each terminal call.

For example, `pwd && ls -la` is not an admissible preliminary inspection. `pwd`
can remain a shell builtin, but `ls` must enter apexe even in that first call:

```bash
pwd && apexe exec --dry-run -- ls -la
# Inspect preflight before the separate execution call.
apexe exec -- ls -la
```

Check every external program in a compound command, conditional branch or
pipeline before sending it to the terminal. Readonly status does not exempt
`ls`, `cat`, `head` or another external program from this route. If it cannot
be admitted, report the coverage or governance failure instead of first trying
the original program. The direct apexe administration calls below are the
discovery route; bare task commands are not a substitute for discovery.

Keep shell builtins such as `cd`, `pwd`, `test` and `printf` in the host shell. Keep
host-provided file readers, editors and other non-CLI tools on their existing
interfaces; this Skill routes external CLI commands, not every agent tool.
Do not select another tool to evade an apexe refusal. Run apexe's own discovery,
preflight and configuration commands directly without recursive wrapping.
Do not replace an unsupported command or pipeline with `sh -c`, `bash -c`,
an interpreter script or an absolute binary path to bypass admission.

## Discover the environment

Use the configured working directory, binding directory and ACL for the task.
Carry explicit binding-directory and ACL overrides into every preflight and
execution, including retries; do not silently revert to a different default.
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
Host permission to read an ACL and apexe's decision under that ACL are separate.
If a host reader refuses access, stop and report that prerequisite; do not
remove `--acl`, switch to the default policy or use a raw CLI to read around
the host's refusal. Operators should provide canonical paths and the required
host file permissions for the selected configuration.

## Execute native argv by default

For a newly selected invocation or changed arguments, preview the same command
you intend to execute in a preflight-only terminal call. Wait for the result and
inspect its checks before submitting execution in a separate tool call:

```bash
# First tool call: preflight only.
apexe exec --dry-run -- ls -la
# Read the returned checks. Separate tool call after admission:
apexe exec -- ls -la
```

Never submit preflight and execution together, even with `&&`, `;`, a newline,
or a conditional: a shell exit status does not replace reading the returned
checks. Do not submit another execution speculatively alongside a preflight.
Every preflight must be the sole tool request in its assistant response,
including non-CLI tracking tools. For a pipeline, preflight its stages in order.
Read the returned `valid`, `mode`, `acl_decision`, approval,
argument and path checks; report the current stage's decision briefly. Only
after admission may a new assistant response request the next stage. Separate
terminal calls in the same response, or inside a host batch, still violate this
rule. Do not request a later stage before the previous result is available.
On the first governance refusal, stop all tools and return only the text report
as specified above; do not preflight or execute remaining stages.

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

First assistant response: submit only the first stage's preflight.

```bash
apexe exec --dry-run -- cat input.txt
```

Read the result and state whether cat was admitted. If valid and allowed in
native mode, with matching paths and no unresolved approval, a new assistant
response may submit only the next preflight. If refused, stop here.

```bash
apexe exec --dry-run -- head -n 5
```

Read head's result and state its decision. Only after both stages are admitted
may another assistant response submit execution:

```bash
set -o pipefail && apexe exec -- cat input.txt | apexe exec -- head -n 5
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

After a governance refusal, report its reason, any operation already completed,
the unfinished work and the operator action needed, without further tools.
Do not try spelling variants, another execution surface or tracking updates.
A grammar-only gap may use the admitted JSON alternative above; it is not
permission to bypass policy, schema or path refusal. `--as` is a dry-run
assertion, not authenticated impersonation or execution approval.

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
