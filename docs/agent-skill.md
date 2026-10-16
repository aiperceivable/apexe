# Connect a terminal agent with the apexe Skill

Use the same [apexe Skill](../skills/apexe/SKILL.md) with Claude Code, Codex,
pi or OpenCode. In a project configured for apexe, ordinary CLI requests use
`apexe exec -- <command> <arguments>` through the agent's existing terminal
tool. No MCP server or shell activation is required.

Follow the four steps below, then use [troubleshooting](#troubleshooting) if
the observed commands differ. Contributors testing a Skill change should use
the separate [maintainer verification guide](agent-skill-verification.md).

## Quick integration

### 1. Prepare CLI coverage and policy

Install apexe 0.9 or later and the command binaries in the environment where
the agent's terminal runs. See [installation](quickstart.md#1-install).
An operator initializes coverage once:

```bash
apexe --version
apexe init --format json
apexe list --verbose --format json
```

Review the generated bindings and `~/.apexe/acl.yaml` before execution. If this
environment is already prepared, inspect the existing configuration rather
than initializing it again. An absent ACL leaves access control inactive.

The init report's `native_ready` array identifies reviewed native coverage.
The current BSD pilot covers cat/head/ls; scanning GNU variants or other tools
does not establish native eligibility. If the intended commands are not
native-ready, resolve coverage before expecting the native workflow below.
See [initialization](quickstart.md#2-initialize) and [exec](user-manual.md#47-apexe-exec).

### 2. Install the same skill in your agent

Use Python 3.9+ from an apexe source checkout. If you installed a prebuilt
binary and have no checkout, obtain one first:

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
```

From the checkout, install for user-level availability across projects:

```bash
python3 scripts/install_agent_skill.py --scope user
```

The installer and Skill are repository files; the installed Skill itself
needs no checkout at runtime.

Alternatively, install only in the project where your agent works:

```bash
python3 scripts/install_agent_skill.py --project-dir /path/to/your-project
```

Replace the example project path. The default installer writes both directories:

| Agent | User-level file | Project-level file | Explicit selection |
| --- | --- | --- | --- |
| Claude Code | `~/.claude/skills/apexe/SKILL.md` | `.claude/skills/apexe/SKILL.md` | `/apexe` |
| Codex | `~/.agents/skills/apexe/SKILL.md` | `.agents/skills/apexe/SKILL.md` | `$apexe` |
| pi | `~/.agents/skills/apexe/SKILL.md` | `.agents/skills/apexe/SKILL.md` | `/skill:apexe` |
| OpenCode | `~/.agents/skills/apexe/SKILL.md` | `.agents/skills/apexe/SKILL.md` | Ask it to load the `apexe` Skill |

Codex and pi share one copy. OpenCode also discovers Claude-compatible copies;
keep every discovered `apexe` copy identical. See the official
[Claude Code](https://code.claude.com/docs/en/skills),
[Codex](https://learn.chatgpt.com/docs/build-skills#where-codex-loads-local-skills),
[pi](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/skills.md)
and [OpenCode](https://opencode.ai/docs/skills/) Skill references.

Use `--agent claude`, `--agent codex` or `--agent pi` to select a destination;
there is no `--agent opencode` option, and no extra OpenCode copy is needed.
`--dry-run` prints the planned destinations. Identical reinstalls leave files
unchanged. To replace a different version after reviewing it, add `--force`;
other files in the directory are preserved and symlinked destinations are
refused. The installer does not install the binary, initialize coverage or
change policy.

### 3. Declare the project's execution route

Add the following to `CLAUDE.md` for Claude Code and `AGENTS.md`
for Codex, pi and OpenCode, near the beginning of the file. If you use both
instruction files, put the rule in both. This template selects the reviewed
default configuration; use it only after completing step 1.

```markdown
This project uses apexe-governed external CLI execution, with the project
working directory and reviewed default bindings and ACL under ~/.apexe.
Before the first task CLI call, read this entire instruction file and the
complete installed apexe Skill. Use the host's Skill loader/file reader;
if only a terminal is available, use shell builtins and input redirection.
Do not load instructions with bare sed, cat, head or an interpreter. Stop
if required instructions are missing, truncated or refused by the host.

Follow the Skill for ordinary CLI tasks, including initial directory listing
and readonly probes. Use native `apexe exec -- <command> ...` by default.
Record the selected working directory, bindings and ACL before any task call.
Carry any explicitly selected ACL (--acl before --) and binding override into
EVERY preflight, execution, pipeline stage and retry; never try defaults first.
Submit preflight alone and inspect its actual returned checks before execution
in a separate tool call. For pipelines, submit exactly one stage's preflight
per assistant response, read and state its decision, then choose the next stage
in a NEW response. Do not batch or queue dependent stages, even if the host
runs tool calls serially. Do not join preflight and execution with && or ;.
Stop on the first refusal, with no later task tools queued or requested.
If a call omitted a selected ACL, stop and report the error and any operation
already completed; a corrected retry does not repair that earlier call.
Do not bypass refusal with raw binaries, another tool, unmanaged execution
or policy changes. Keep shell builtins and host file tools on their existing
interfaces. Use pipefail or collect all stage statuses for pipeline execution.
```

For a custom configuration, replace the opening configuration sentence with
actual canonical bindings and ACL paths. State that the selected `--acl` must
appear before `--` in every task exec, including the first preflight. For
example, the shape is `apexe exec --acl /actual/reviewed-acl.yaml --dry-run -- ls -la`;
replace that example path with your real path. Provide the host access to the
selected files. See [ACL reading](#host-acl-reading-and-apexe-policy-checks).

Installing the Skill makes it available; project or task instructions establish
where the routing rule applies. Automatic selection depends on the host and
model. A Skill guides behavior through the host terminal tool; mandatory
interception requires enforcement by that host.

### 4. Verify with an ordinary task

Start a new session in the configured project, keeping project instruction
sources enabled. Explicitly select the Skill once using the table above if it
has not loaded. Then ask, without spelling out an apexe command:

> List all entries in the current directory, including hidden entries, with detailed metadata.

Inspect the actual terminal tool calls. They should first preview the listing:

```bash
apexe exec --dry-run -- ls -la
```

The agent must read admission (`valid: true`, `mode: native`, ACL allowed,
matching arguments/paths and no unresolved approval) before a separate call:

```bash
apexe exec -- ls -la
```

Both calls must carry your explicit ACL/binding overrides when selected.
Check the output and exit status, and check that no preliminary bare `ls` ran.

With an existing readable `input.txt`, also ask:

> Run a command-line pipeline that reads input.txt and shows only its first two lines. Report the output and success or failure.

Expect cat's preflight, its inspected decision, then head's preflight in a
new response, its decision, then a pipeline whose **two** external stages
use exec. The agent must check all stage statuses. On a refusal it must stop
immediately. A successful final answer alone does not prove correct routing.
Use the [maintainer procedure](agent-skill-verification.md#3-run-the-three-scenarios)
for a controlled denial test without changing your normal ACL.

## Troubleshooting

| Symptom | Check and remedy |
| --- | --- |
| The Skill is missing | Check the installed file against the table, start a new session, and try explicit selection. Confirm the full Skill loaded rather than just its catalog name. |
| The host loads an old Skill | Check the actual loader's directory and full body. Remove stale copies or update all discovered copies to the same reviewed content, then restart. |
| The first command is bare `ls` or `sed` | Confirm the project instructions loaded before task tools. Add the step 3 rule and retain project configuration sources. Installing a Skill alone does not enforce routing. Stop the task and inspect what already ran. |
| Preview and execution share one shell call | Keep them in separate tool calls with model inspection between them. Shell short-circuiting cannot perform that inspection. |
| cat/head previews are submitted together | Require one stage per assistant response. Two serial tool calls in one response are still a batch; stop and correct the routing instructions. |
| The command has no binding or native grammar | Inspect `native_ready` and the concrete preflight. Install matching reviewed rules or use advertised module JSON only for a grammar gap when its normal checks permit the same operation. Do not use raw fallback. |
| A required ACL is absent or omitted | Restore the operator-selected policy/configuration. Carry its path into every call. If a call already omitted it, stop and report the error rather than counting a retry as success. |
| apexe returns `ACL_DENIED`, a path/schema refusal or unresolved approval | Stop without requesting later stages or using another tool to finish the denied operation. Report the reason for operator review. Do not widen policy to make the task pass. |
| The model cannot start | Check the host's provider, credentials and account-supported model. No tool calls means agent routing has not been tested. |
| A later tool call loses shim activation | Independent tool calls may start new shells. Keep explicit exec, or activate and verify shims in the same shell that executes the task. |

### Host ACL reading and apexe policy checks

A host reader's permission refusal is different from apexe's `ACL_DENIED`.
Resolve the selected ACL to its canonical path and grant the host only the
needed file/directory access. On macOS, `/tmp` may resolve to `/private/tmp`;
inconsistent spellings can trigger external-directory restrictions. OpenCode's
[permissions reference](https://opencode.ai/docs/permissions/) covers host access.

If the host refuses the instructions or ACL, stop and report the prerequisite.
Do not switch readers, remove `--acl` or try a default policy. That attempt
has not established a denial by apexe. An allowed preview also does not identify
which ACL file was loaded; inspect the submitted command's policy selection.

For Claude Code, retain project configuration sources. If you restrict them
using `--setting-sources`, include `project`, for example
`--setting-sources user,project`. Loading a Skill alone does not prove the
applicable `CLAUDE.md` was read. Model/provider setup is part of your agent
host configuration; sourcing a personal provider script is not an apexe step.

## Optional enhancement: env

Explicit exec works without shims. Use `env` when an authorized shell/script
workflow needs ordinary command names, and verify activation and every intended
shim route in the same shell before using those names. Enable/reload and the
deactivation helper change installation state; `env --check` inspects it.
Builtins, aliases, functions, absolute paths and uncovered tools can bypass
shims. See the [env reference](user-manual.md#48-apexe-env).

`scan --skills-dir` generates command-specific module Skills; it complements
this shared routing Skill. Use [MCP/A2A](user-manual.md#12-integrating-with-ai-agents)
for protocol clients and [OpenAPI import](openapi.md) for HTTP APIs. Normal
exec/env provide process controls and typed-path checks, not an OS sandbox;
see the [threat model](threat-model.md).
