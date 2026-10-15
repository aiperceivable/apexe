# Agent Skill: default CLI execution routing

For a terminal-capable Claude Code, Codex, pi or OpenCode agent, install the shared
[apexe Agent Skill](../skills/apexe/SKILL.md) to route external CLI commands
through native `apexe exec -- <command> <arguments>` by default in an
apexe-configured environment. Ordinary requests such as listing files or
reading text use this route without the user naming apexe each time. Each call
enters apexe explicitly, without starting an MCP server or activating a shell environment.
Use `env` as an optional enhancement for existing scripts and shell workflows.
MCP/A2A remain available for protocol clients and imported HTTP APIs.

## Install the same skill in your agent

The portable package contains one self-contained `SKILL.md` with standard
`name`, `description` and `license` frontmatter. It does not depend on
host-specific tool names, macros, permissions metadata or a source checkout
after installation. The apexe binary, CLI binaries, bindings and policy must
still be available in the agent's execution environment.

| Agent | Project location | User location | Explicit invocation |
|---|---|---|---|
| Claude Code | `.claude/skills/apexe/SKILL.md` | `~/.claude/skills/apexe/SKILL.md` | `/apexe` |
| Codex | `.agents/skills/apexe/SKILL.md` | `~/.agents/skills/apexe/SKILL.md` | `$apexe` |
| pi | `.agents/skills/apexe/SKILL.md` | `~/.agents/skills/apexe/SKILL.md` | `/skill:apexe` |
| OpenCode | `.agents/skills/apexe/SKILL.md` | `~/.agents/skills/apexe/SKILL.md` | Ask the agent to load the `apexe` Skill |

Codex and pi use the same Agent Skills location, so a single installed copy
serves both. These paths follow the official [Claude Code skill documentation](https://code.claude.com/docs/en/skills),
[Codex skill documentation](https://learn.chatgpt.com/docs/build-skills#where-codex-loads-local-skills)
and [pi skill documentation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/skills.md).
OpenCode discovers these Agent Skills paths too, as well as Claude-compatible
paths, and loads instructions through its native `skill` tool. See the
[OpenCode skill documentation](https://opencode.ai/docs/skills/). The installed
copies must have identical content if both locations contain `apexe`; a host
may select either copy. No additional OpenCode-specific installation is needed.
This integration targets terminal-capable agents; Claude Desktop's MCP setup is
a separate integration.

From an apexe source checkout, use Python 3.9+ to install it into the project where
you run your agent:

```bash
# Install for all three agents; Codex and pi share one copy.
python3 scripts/install_agent_skill.py --project-dir /path/to/your-project
# Or select one agent.
python3 scripts/install_agent_skill.py --agent claude --project-dir /path/to/your-project
# Or install across projects for your user.
python3 scripts/install_agent_skill.py --scope user
# Inspect destinations without creating files.
python3 scripts/install_agent_skill.py --scope user --dry-run
```

The default scope is the current project. Identical reinstallation leaves files
untouched. A different existing `SKILL.md` is refused unless `--force` is given;
replacement preserves other files in the skill directory. Symlinked skill
destinations are refused. The installer copies only the self-contained skill
entry point and does not install apexe, initialize bindings, edit ACLs, activate
shims or configure MCP. Installing a binary alone does not install this skill.

You can also copy `skills/apexe/SKILL.md` into one of the table's directories
manually; create the directory first and preserve any custom version. Start a
new agent session if the installed skill is not visible. Invoke the skill
explicitly when you want to select it; its description also supports automatic
selection for ordinary CLI tasks in an apexe-configured environment. Automatic
selection depends on the agent host and model; installation alone does not
establish a project-wide routing requirement.

## Declare the project's execution route

After preparing bindings and reviewing policy, declare apexe-governed CLI
execution in the instructions your host reads: `AGENTS.md` for Codex, pi or OpenCode,
and `CLAUDE.md` for Claude Code. For example:

```markdown
This project uses apexe-governed CLI execution. Load the installed apexe Skill
before the first terminal call. Route external CLI commands, including initial
inspection, readonly probes, file listing and text processing, through native
`apexe exec -- <command> ...`
by default, using the current working directory and the reviewed default
bindings and ACL under ~/.apexe. Preflight new or changed invocations.
Do not run a preliminary bare command such as `pwd && ls -la` before routing.
For missing native grammar, use an advertised module's JSON interface only
when it preserves the operation and its normal checks permit it. Report
missing coverage or governance refusals; do not fall back to raw binaries.
Keep shell builtins and host-provided file tools on their existing interfaces.
```

Adapt the configuration description for your deployment and provide any
explicit binding directory or ACL path. Do not put placeholder paths into
commands. A task-specific operator instruction can establish the same scope.
The Skill is available across projects when installed for a user; only projects
or tasks opting into apexe-governed execution adopt its default CLI route.
An explicit apexe request also selects the route. A missing binary, binding or
required ACL is a reported prerequisite, not permission for raw fallback.

This is agent execution guidance. Enforcing that every external command must
enter apexe requires integration or restrictions at the host's execution tool;
a Skill cannot intercept or constrain an unrestricted terminal by itself.

## Prepare CLI coverage and policy

An operator prepares the machine once, reviews the generated policy, and
provides the agent with the intended working directory and configuration:

```bash
apexe init --format json
apexe list --verbose --format json
# Review ~/.apexe/acl.yaml before relying on its access rules.
```

The init report includes `native_ready` module IDs. CLI scanner coverage does
not imply native eligibility: native argv needs matching authoritative rules
with explicit grammar and capabilities. The current reviewed BSD pilot covers
cat/head/ls. If the report is unavailable, preflight the intended command to
check its admission. The Skill does not rerun init on every task.

Local calls load `~/.apexe/acl.yaml` when present, or use an explicit `--acl`.
An absent ACL leaves access control inactive; a malformed selected policy
fails closed. Skill installation does not grant permissions or broaden policy.
See [initialization](quickstart.md#2-initialize) and the
[exec reference](user-manual.md#47-apexe-exec).

## Default agent workflow: native exec

Routing starts with the first terminal call, including orientation and readonly
probes. Loading the Skill is followed by rewriting the intended commands;
an initial bare `ls` is a routing violation even if a later listing uses apexe.
Shell builtins do not exempt adjacent external programs in a compound command.

For a request such as "list all current directory entries, including hidden
files," the agent selects native exec itself and checks the intended call:

```bash
apexe exec --dry-run -- ls -la
apexe exec -- ls -la
# Each external pipeline stage uses its own governed call.
apexe exec -- cat input.txt | apexe exec -- head -n 5
```

The Skill teaches the agent to inspect `valid`, `mode`, `acl_decision`,
`approval_required`, argument and path checks before executing a newly selected
invocation. Keep the same arguments and policy for preview and execution;
execution checks them again. Place apexe options before `--` and child options
after it. Native output and child status pass through instead of returning a
JSON envelope. stdin is inherited only when the reviewed grammar admits it.

`approval_required` can be a conservative static ceiling for flag-derived
annotations. The Skill inspects that warning; actual execution weighs the sent
arguments and can stand down when none escalate. Preflight does not obtain
human approval, and calls actually needing an interactive decision fail closed
locally. The Skill never clears the warning by editing policy or annotations.

### Host ACL reading and apexe policy checks

Host file permissions and apexe's ACL decisions are separate. Use canonical
paths for the working directory and any operator-selected ACL. For example,
macOS `/tmp` can resolve to `/private/tmp`; a host may classify a path under a
different spelling as outside the working directory. For an ACL stored outside
the workspace, an operator may need to allow the selected file through the
host's file-reading and external-directory permissions. OpenCode documents
these separately in its [permission reference](https://opencode.ai/docs/permissions/).

Provide only the needed host access. Do not disable all host permissions or
weaken apexe policy to make a test pass. If a host reader refuses the ACL, report
that prerequisite; the apexe denial test has not run. Preserve the selected
`--acl` on every preflight and execution instead of silently using a default.

The shell handles pipes, expansions and redirections. Route each external
pipeline stage independently; use `pipefail` or inspect all stage statuses so
an earlier refusal cannot be hidden by a later success. A shell-side `> file`
is outside apexe's typed-path checks. Builtins such as `cd`, `test` and `printf`
remain in the shell. Host-provided file readers and editors keep their existing
interfaces. Do not double-wrap apexe calls, recursively govern its own setup
commands, or use an interpreter or another tool to evade admission.

When native grammar alone is unavailable, the agent can use an advertised
module's JSON interface after reading its actual schema:

```bash
apexe list --verbose --format json
# Property names and shapes come from the advertised input_schema.
apexe exec cli.ls --input '{"file":["."]}' --dry-run
apexe exec cli.ls --input '{"file":["."]}'
```

Module JSON remains subject to schema, policy, approval and path checks. It
returns captured output and uses null child stdin. Switching representation
does not authorize an otherwise denied operation. The Skill requires reporting
unresolved governance refusals instead of using raw binaries, unmanaged
execution or policy edits to get around them. Local approval-required calls
cannot obtain interactive approval; an operator can choose a configured MCP
approval workflow separately when that is needed.

## Optional enhancement: env

For an explicitly requested shell/script setup with eligible bindings:

```bash
apexe env --enable
```

After successful setup, activate and execute in the same shell:

```bash
source ~/.apexe/bin/apexe-activate.sh &&
apexe env --check &&
test "$(command -v cat)" = "$HOME/.apexe/bin/cat" &&
test "$(command -v head)" = "$HOME/.apexe/bin/head" &&
cat input.txt | head -n 5
```

Enabling installs shims and managed startup blocks. Source the helper in the
same shell that runs commands; an agent's later tool call may start a new shell
and will not inherit an earlier shell's activation. Explicit exec avoids that
dependency. Use the printed helper path for a custom shim directory. POSIX sh
uses `.` instead of `source`; other shells have their own activation syntax.
The guarded example requires both names to resolve to their expected shims;
adapt those checks for custom directories. A failed activation, environment
check or route check stops before the pipeline instead of falling through to
ordinary binaries.

`--check` inspects installation and activation. `--reload` changes installation
state and can enable an absent installation. Sourcing the deactivation helper
uninstalls owned shims and startup blocks as well as restoring the shell's saved
environment, so the Skill does not automatically deactivate existing operator
setups. Builtins, aliases, functions, absolute paths and uncovered tools bypass
shims. See the [env reference](user-manual.md#48-apexe-env).

## Routing skill and generated module skills

This shared `apexe` Skill chooses the default external CLI execution route in
configured environments and teaches discovery, native execution, refusal
handling, JSON alternatives and optional shell integration. `apexe scan --skills-dir`
generates separate per-module skills with command-specific schemas and guidance
under `.claude/skills/<module_id>/`. Those outputs complement this routing
skill; they do not install it or change native eligibility.

For clients that need protocol tool discovery or HTTP API import, see
[MCP/A2A integration](user-manual.md#12-integrating-with-ai-agents) and
[OpenAPI import](openapi.md). Normal exec/env are not OS sandboxes; see the
[threat model](threat-model.md).
