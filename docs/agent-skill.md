# Agent Skill: native CLI execution

For a terminal-capable Claude Code, Codex or pi agent, install the shared
[apexe Agent Skill](../skills/apexe/SKILL.md) and use native
`apexe exec -- <command> <arguments>` by default. Each call enters apexe
explicitly, without starting an MCP server or activating a shell environment.
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

Codex and pi use the same Agent Skills location, so a single installed copy
serves both. These paths follow the official [Claude Code skill documentation](https://code.claude.com/docs/en/skills),
[Codex skill documentation](https://learn.chatgpt.com/docs/build-skills#where-codex-loads-local-skills)
and [pi skill documentation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/skills.md).
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
selection for apexe tasks.

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

The shell handles pipes, expansions and redirections. A shell-side `> file`
is outside apexe's typed-path checks. Skill guidance cannot enforce that an
agent with unrestricted terminal access always enters apexe.

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

## Workflow skill and generated module skills

This shared `apexe` Skill teaches discovery, native execution, refusal handling,
JSON alternatives and optional shell integration. `apexe scan --skills-dir`
generates separate per-module skills with command-specific schemas and guidance
under `.claude/skills/<module_id>/`. Those outputs complement this workflow
skill; they do not install it or change native eligibility.

For clients that need protocol tool discovery or HTTP API import, see
[MCP/A2A integration](user-manual.md#12-integrating-with-ai-agents) and
[OpenAPI import](openapi.md). Normal exec/env are not OS sandboxes; see the
[threat model](threat-model.md).
