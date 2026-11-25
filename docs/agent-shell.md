# Connect terminal agents with the apexe shell

`apexe-sh` is the stable shell entry point installed by `apexe env --enable`.
Hosts that allow a custom shell path can use it directly. Hosts that do not can
use PATH shims, a pre-tool hook, or an MCP-only deployment.

This guide covers Claude Code, Antigravity, OpenCode, Codex and pi. It records
the tested versions; host behavior changes between releases, so re-run the
verification steps after upgrading.

## How it works

```text
agent -> apexe-sh -c "<command>"
           | execve(real shell, PATH with shim directory first)
           v
      /bin/zsh -c "<command>"
           | external command resolved to an installed shim
           v
      command shim -> shared executor -> configured ACL, approval, audit
```

`apexe-sh` does not parse the command string and does not stay in the process
tree. It prepares the environment and replaces itself with the real shell.
Shell builtins, functions, pipelines and redirections keep their normal
semantics.

Only commands with an installed shim are governed. Absolute paths, builtins,
functions, `sudo`, interpreter-internal execution and commands without a shim
are not. This is not a sandbox.

## One-time setup

After a binary-only install, run `apexe setup` to initialize bindings, create
a `strict` starter ACL on fresh installations, bind the policy to the shims,
and offer to configure detected hosts. Review the summary and ACL. For Codex,
follow the login-shell step below.
These commands prepare and inspect the components individually.

```bash
apexe init
apexe env --enable
apexe sh --check
```

`apexe sh --check` reports the real shell, the entry point, the manifest, ACL
state, verified and scanned routes, and host configuration snippets. For one
host, `apexe agent opencode` (or `pi`, `codex`, `claude`, `antigravity`) prints
the supported configuration, and `apexe agent --check <host>` checks launcher
and host readiness without writing files. The entry point is normally:

```text
~/.apexe/bin/apexe-sh
```

Use its absolute path in host configuration.

## OpenCode

Tested with OpenCode 1.19.5.

Set `shell` in the project `opencode.json` or in
`~/.config/opencode/opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "shell": "/Users/you/.apexe/bin/apexe-sh"
}
```

Restart OpenCode after changing the file. Its shell tool invokes
`apexe-sh -c "<command>"`.

Verify inside OpenCode by asking it to run:

```text
command -v ls
apexe exec -- ls -la
```

`command -v ls` should print `~/.apexe/bin/ls`, and `~/.apexe/audit.jsonl`
should contain an execution record.

## pi

Tested with pi-coding-agent 1.1.0.

Set `shellPath` in `~/.pi/agent/settings.json`:

```json
{
  "shellPath": "/Users/you/.apexe/bin/apexe-sh"
}
```

Restart pi. Its bash tool invokes `apexe-sh -c "<command>"` and exports
`APEXE_SHELL_DEPTH=1` to nested shells.

If pi's default shell should be bash rather than the inherited `$SHELL`, set:

```bash
export APEXE_REAL_SHELL=/bin/bash
```

Verified behavior includes shim resolution for `ls`/`cat`/`head`, ACL allow and
execution audit records, fail-closed syntax refusals, `exit 7` passthrough and
nested-shell governance.

## Codex

Tested with Codex CLI 0.145.0.

Codex does not accept `apexe-sh` as its shell. It starts commands with its real
login shell (for example `/bin/zsh -lc`), so the supported integration is PATH
activation:

```bash
apexe env --reload --shell zsh
# Restart Codex.
```

The managed block is written to `.zshrc` and `.zprofile`. The `.zprofile` block
runs after macOS `path_helper` resets `PATH`, so the shim directory is restored
for non-interactive login shells. For bash, use `--shell bash`; for sh, use the
login profile.

Do not set `SHELL=apexe-sh` for Codex. Verify inside Codex:

```text
command -v ls
```

It should print `~/.apexe/bin/ls`.

## Claude Code

Tested with Claude Code 2.1.318.

Claude Code rejects `apexe-sh` as `SHELL` or `CLAUDE_CODE_SHELL` and falls back
to `/bin/zsh`. Use one of the two supported paths.

### PreToolUse hook (recommended)

Add a `PreToolUse` hook matching `Bash` in `.claude/settings.json` (project) or
`~/.claude/settings.json` (user):

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/Users/you/.apexe/bin/apexe hook claude",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

Use the real `apexe` executable path, not `apexe-sh`: `apexe-sh` forwards its
arguments to the shell, while `apexe hook` is a normal subcommand.

The hook rewrites simple external commands to
`<apexe> sh -c '<command>'` through `updatedInput` and returns
`permissionDecision: "allow"`. Claude Code still evaluates its own deny/ask
rules against the rewritten command. Commands already routed through
`apexe`/`apexe-sh` are left unchanged, so the hook does not recurse.

Stateful builtins (`cd`, `export`, `set`, `alias`, `echo`, …), pipelines,
redirections, command substitution, compound commands and `sudo` are left
unchanged. They continue to rely on PATH shims. A missing shim manifest makes
the hook deny the call with a remedy instead of pretending it is governed.

### PATH shims

Start Claude Code from a terminal whose `PATH` already contains the shim
directory:

```bash
export PATH="$HOME/.apexe/bin:$PATH"
claude
```

Claude Code's shell snapshot copies the launch environment's `PATH`; editing
`.zshrc` alone is not enough. This governs external commands that resolve
through PATH shims, but does not cover absolute paths or compound commands the
same way the hook does.

## Antigravity

Antigravity (Desktop, IDE and `agy` CLI) supports lifecycle `PreToolUse` hooks
and Stdio MCP servers. It does not require installing `gemini` CLI.

### PreToolUse hook (recommended)

Add an `apexe` hook entry matching `run_command` in `.agents/hooks.json`
(project) or `~/.gemini/config/hooks.json` (user):

```json
{
  "apexe": {
    "PreToolUse": [
      {
        "matcher": "run_command",
        "hooks": [
          {
            "type": "command",
            "command": "/Users/you/.apexe/bin/apexe hook antigravity",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

Use the real `apexe` executable path, not `apexe-sh`. The hook reads
Antigravity's camelCase `PreToolUse` JSON payload (`toolCall.name ==
"run_command"`, `toolCall.args.CommandLine`) and rewrites simple external
commands to `<apexe> sh -c '<command>'` through `overwrite.CommandLine` with
`decision: "allow"`. Commands already routed through `apexe`/`apexe-sh`,
stateful builtins, pipelines, redirections, command substitution, compound
commands and `sudo` exit `0` with empty stdout so Antigravity leaves them
unchanged and PATH shims govern eligible external commands (returning `{}`
without a `decision` is treated as a denial by Antigravity). A missing shim
manifest makes the hook return `decision: "deny"` with a remedy.

Verified on Antigravity Desktop (macOS arm64, 2026-11-25): simple `run_command`
calls (`ls README.md`) were rewritten through `overwrite.CommandLine`, resolved
to `~/.apexe/bin/ls` and recorded both ACL `allow` and `execution` entries in
`~/.apexe/audit.jsonl`; compound commands and already-routed `apexe` commands
passed through unchanged; a missing shim manifest denied `run_command` with the
hook's remedy text. Note that Antigravity's Terminal Sandbox blocks
`run_command` from reading or writing `~/.apexe` outside the workspace (`os
error 1`), so disable Terminal Sandbox for the project or allow access to
`~/.apexe` when using the local hook integration.

`apexe setup` and `apexe agent --check antigravity` detect Antigravity through
`agy`/`antigravity` on `PATH` or an existing `~/.gemini/antigravity*`
directory (covering Antigravity Desktop and IDE installations that do not put a
CLI binary on `PATH`).

## Automatic host configuration

`apexe agent <host> --write` merges the supported configuration into the host's
JSON file without replacing unrelated keys:

```bash
apexe agent opencode --write
apexe agent claude --write
apexe agent antigravity --write
apexe agent pi --write
apexe agent codex --write
```

- OpenCode writes `shell` to the project `opencode.json`; use
  `--scope user` for `~/.config/opencode/opencode.json`.
- pi writes `shellPath` to `~/.pi/agent/settings.json`.
- Claude Code writes a `PreToolUse` hook to `.claude/settings.json`; use
  `--scope user` for `~/.claude/settings.json`.
- Antigravity writes a `PreToolUse` hook to `.agents/hooks.json`; use
  `--scope user` for `~/.gemini/config/hooks.json`.
- Codex has no apexe-specific shell file; `--write` reloads startup files for
  the resolved real shell (`APEXE_REAL_SHELL`, then `SHELL`, then the launcher
  fallback), preserving the selected shim directory. The check reads that
  shell's noninteractive login startup file rather than accepting a block from
  an unrelated shell or an interactive-only `.bashrc`/`.zshrc`.
  Restart Codex afterwards. If Codex explicitly selects a different shell,
  configure that shell with `apexe env --reload --shell <shell>` instead.

The writer refuses invalid JSON and OpenCode refuses when `opencode.jsonc`
already exists. OpenCode, pi and Claude hook installation also refuse an
incomplete launcher installation: a missing entry point breaks shell-path
hosts, and a missing manifest makes the Claude hook deny Bash calls. Invalid
Claude hook container types are refused without changing the file. Shell
configuration writes and `apexe agent --check` also require a loadable ACL
recorded in the shim manifest. For an older installation with an implicit
default ACL, run `apexe env --reload` to bind it. Use the installed `--shim-dir`
if you chose a custom directory. Review the host file after writing.

### Repair stale integration without bypassing policy

Run the checks from the terminal that will launch the agent:

```bash
apexe sh --check
apexe env --reload
apexe agent <host> --write
apexe agent --check <host> --format json
```

Reload preserves the recorded bindings, ACL and filters when the manifest is
valid. If the manifest is missing or corrupt, restore with `apexe env --enable`
and the original `--shim-dir`, `--bindings-dir`, `--acl` and policy options;
do not substitute a default policy for a previously selected one.
Use the same `--scope user` or project scope as the original installation,
then restart the agent. OpenCode/pi checks compare the selected launcher path
with the active installation; an unrelated or removed `apexe-sh` is not ready.
An explicit OpenCode project `shell` overrides the user setting, even when it
is invalid or points elsewhere. A project file with no `shell` inherits the
user selection; unreadable or malformed project JSON cannot establish readiness.
pi's supported leading `~/` is expanded before comparison, including when
checking for host settings that depend on shims being uninstalled.
Claude `--write` updates recognized apexe hook commands to the current binary
without adding another hook, including quoted paths and `--persistent`. Other
hooks and permission rules are preserved. A stale hook is reported even when
PATH already contains shims, because PATH cannot repair a hook executable.

Do not repair an installation by filling `apexe-sh` with a hand-written shell
wrapper. Do not clear an ACL or ignore a governance refusal. Plain `apexe sh`
continues to run a real shell without an installation; that is not evidence of
governed execution, and the hook retains its fail-closed prerequisite check.
Existing MCP-only `tools.bash: false` or `permissions.deny: ["Bash"]` settings
are not removed by shell configuration; explicitly review them if restoring
ordinary shell access is intended.

## Experimental persistent session

The Claude Code hook normally rewrites only simple external commands, because a
one-shot `apexe sh -c` does not preserve shell state. `--persistent` keeps a
governed shell alive across calls:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/Users/you/.apexe/bin/apexe hook claude --persistent",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

The first call starts a detached session over a Unix socket (mode `0600`). The
session preserves `cwd`, shell variables, functions and aliases, and accepts
compound commands, pipelines and redirections. It is scoped to the host session
id and project directory, exits after 30 minutes idle by default
(`APEXE_SESSION_IDLE_SECONDS`), and can be stopped with:

```bash
APEXE_SESSION_STOP=1 apexe sh -c stop
```

Limitations: commands that read stdin are not supported; output is text; one
request is served at a time; the daemon runs with the user's privileges. Treat
this as experimental until it has run across your host versions.

## MCP-only deployment

MCP-only configures access to apexe tools and requests that the host disable
its shell tool. Verify the effective permission in the running host after
reload before relying on it as a boundary. It is not suitable for agents that
must do arbitrary coding.

```bash
apexe agent opencode --mcp-only --write
apexe agent claude --mcp-only --write
apexe agent antigravity --mcp-only --write --scope user
apexe agent codex --mcp-only
```

- OpenCode: merges `mcp.servers.apexe` and a V2 `shell` deny rule. Automatic edits
  require OpenCode 2.3.0. apexe checks discovered permission sources and
  previews the proposed rule through the host's effective agent diagnostics.
  A conflict or an inconclusive preview stops publication. In user scope,
  this preview covers the current project context. For other versions or
  conflicts, omit `--write` for a snippet and review permissions manually.
- Claude Code: project scope merges `mcpServers.apexe` in `.mcp.json` and adds
  `Bash` to `permissions.deny` in `.claude/settings.json`. Both files' shapes
  are checked before either write. If publishing the second file fails, the
  diagnostic reports the completed write; correct the error and retry the same
  command. User scope updates `~/.claude/settings.json` and prints the
  `claude mcp add --scope user` command you must run to register the server.
  In project scope, approve the server if Claude Code prompts. The acceptance
  fixture verified MCP invocation and Bash refusal after restart in both
  scopes with Claude Code 2.1.318 on macOS in non-interactive `dontAsk` mode.
- Antigravity: user scope merges `mcpServers.apexe` into
  `~/.gemini/config/mcp_config.json`. Antigravity keeps its terminal tool
  (`run_command`), so restrict it in Antigravity Settings (Permission Grants /
  Command Denylist) if you require strict MCP-only access.
- Codex: prints a `[mcp_servers.apexe]` TOML block. Codex keeps its shell, so
  this is not strict MCP-only; combine it with a sandbox.
- pi: no stable MCP-only profile in the tested version.

The generator validates and includes the selected ACL via `--acl`. It uses a
policy recorded in the local shim manifest when present and otherwise uses
the default ACL, including on installations without shims. When a manifest
exists, the generator also uses its binding directory via `--bindings-dir`.
A manifest whose binding pattern differs from the active apcore configuration
stops generation. MCP applies its own tool filters and the selected ACL.
A missing or malformed policy stops generation before host files are changed.
The generator also stops if that policy requires human approval. Configure an
elicitation-capable MCP client with `apexe serve --enable-approval` to use
that policy.
Regenerate older MCP configurations to include the selected ACL and bindings.

## Verify any host

For a shell or hook deployment, check the host configuration and selected ACL
before starting a fresh agent session.

```bash
apexe agent --check <host> --format json
```

Confirm that the host is configured and the report lists the intended ACL and
shim route. For PATH-based integration, confirm in the actual agent terminal
that `command -v ls` points to the installed shim. The Claude Code hook can
route a simple command even when its original shell resolves the native binary.

Ask the agent for a normal detailed directory listing, including hidden entries.
In the terminal tool request, check for `ls -la` with ordinary command syntax.
Use an equivalent CLI operation if this installation covers a different tool.

Check the apexe audit log (default `~/.apexe/audit.jsonl`) for a new
`"event":"execution"` record with the expected module ID. Confirm the
corresponding ACL decision as well. Scanned routes carry `"tier":"scanned"`.
To test denial, use a harmless command with an installed shim and an explicit
deny in a disposable test policy. Check for an ACL deny record and confirm
that the child command did not run.

## Boundaries and troubleshooting

- `apexe env --enable` must have run; without the manifest the launcher still
  starts a shell, but no command is governed.
- `apexe sh --check` fails when the entry point or manifest is missing and
  warns when no ACL is loaded.
- `apexe env --disable` warns when OpenCode/pi still selects `apexe-sh`, or a
  Claude/Antigravity hook remains and would refuse shell tool calls without its
  manifest. Re-enable the installation, or explicitly remove the corresponding
  host setting/hook. Uninstall does not silently edit another application's
  configuration.
- Login shells may reset `PATH`; keep the managed startup block after
  `path_helper` or use the hook.
- A host upgrade can change config discovery or shell behavior. Re-run the
  verification steps and record the new version.
- On Windows, `apexe sh` is not available; use MCP-only or PATH integration per
  host.
