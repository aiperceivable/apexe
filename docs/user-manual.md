# apexe User Manual

> **Audience:** This manual is for people operating `apexe` from the terminal:
> scanning an existing CLI, reviewing the generated policy, connecting an AI
> client, and troubleshooting a deployment. For Rust embedding, parser
> extensions, overlays, and architecture records, see the
> [Developer Guide](developer-guide.md).

| Field | Value |
|-------|-------|
| **Version** | 0.9.0 |
| **Date** | 2026-09-29 |
| **Platform** | macOS / Linux |

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Installation](#2-installation)
3. [Quick Start](#3-quick-start)
4. [Commands Reference](#4-commands-reference)
5. [Configuration](#5-configuration)
6. [How apexe reads a CLI](#6-scanning-engine)
7. [What agents receive](#7-schema-generation)
8. [How apexe classifies risk](#8-behavioral-annotations)
9. [Governance](#9-governance)
10. [MCP Server](#10-mcp-server)
11. [A2A Server](#11-a2a-server)
12. [Integrating with AI Agents](#12-integrating-with-ai-agents)
13. [Error Handling and AI Guidance](#13-error-handling-and-ai-guidance)
14. [File Locations](#14-file-locations)
15. [Global Flags, Logging and Debugging](#15-global-flags-logging-and-debugging)
16. [Troubleshooting](#16-troubleshooting)

---

## 1. Introduction

**apexe** turns any CLI tool on your system into a governed, schema-enforced service that AI agents can invoke safely via the MCP protocol. It works in three steps:

1. **Scan** — Deterministically extract commands, flags, and arguments from CLI tools (no LLM required).
2. **Govern** — Classify commands as readonly/destructive/open-world, generate ACL rules, enable audit logging.
3. **Serve** — Expose tools via MCP (stdio for Claude Desktop/Cursor, HTTP for remote agents).

apexe is built on the [apcore](https://github.com/aiperceivable/apcore-rust) ecosystem: apcore (core types), apcore-toolkit (output), apcore-mcp (MCP server), apcore-a2a (A2A agent server), apcore-cli (`--man` page generation). The audit trail is apexe's own — see §9.2.

---

## 2. Installation

### Prerequisites

- **Rust** 1.75 or later (uses async fn in traits)
- **Cargo** (included with Rust)
- macOS or Linux

### Install from source

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

---

## 3. Quick Start

See [Quick Start Guide](quickstart.md) for the fastest path to a working setup.

```bash
apexe init                         # prepare matching installed CLI tools
apexe list --verbose               # inspect modules and generated policy
apexe exec -- ls -la               # execute through reviewed native grammar
apexe serve --acl ~/.apexe/acl.yaml # optionally serve with the reviewed ACL
```

---

## 4. Commands Reference

### 4.1 `apexe scan`

Scans one or more CLI tools and generates `.binding.yaml` files + ACL rules.

```
apexe scan <TOOLS>... [OPTIONS]
```

| Argument / Option | Default | Description |
|-------------------|---------|-------------|
| `<TOOLS>...` | (required) | CLI tool names to scan (must be on `$PATH`) |
| `--output-dir <DIR>` | `~/.apexe/bindings/` | Directory to write binding files |
| `--depth <N>` | `2` | Subcommand recursion depth (1-5). `git remote add` = depth 2 |
| `--no-cache` | off | Force fresh tool scan; does not update the cached overlay corpus |
| `--offline` | off | Skip automatic corpus download; use installed/local overlays and built-in classification. Also settable with `APEXE_OFFLINE=true` |
| `--format <FMT>` | `table` | Output format: `json`, `yaml`, or `table` |
| `--skills-dir <DIR>` | - | Also write a Claude Skill (`SKILL.md`) per module under `<DIR>/.claude/skills/<module_id>/` |
| `--overlay <PATH>` | - | Load one explicit curated overlay file (JSON/YAML). See [§6.5 Tool Overlays](#65-tool-overlays) |
| `--verify` | off | Fail the command when a written binding does not verify. The YAML verifier runs either way; without this a failure is a warning and the scan still exits 0 |
| `--dry-run` | off | Report what would be written — bindings, ACL and skills — without creating or overwriting anything |

The first online scan automatically installs a validated snapshot of
[cli-permissions](https://github.com/aiperceivable/cli-permissions) under
`<config_dir>/corpora`, unless local `overlay_dirs` or an explicit `--overlay`
were configured. Subsequent scans reuse the recorded commit without contacting
the network. Matching uses the actual tool variant, platform, declared version
constraints and probes; macOS alone does not imply BSD tools.

Failed acquisition, including unavailable Git, produces a warning and falls back
to local overlays and built-in classification. With `--offline`, no automatic
installation is attempted; available snapshots are still used. `--dry-run` never
installs a corpus. Automatic Git operations use the shorter of 10 seconds and
the global timeout; use `apexe init` for normal setup under the configured timeout.
Use `apexe init --format json` to inspect sources and commits, and
`apexe init --refresh` to update rules and bindings owned by initialization.
Rescan custom tools explicitly to adopt those new facts.
`--no-cache` refreshes tool discovery rather than updating the corpus.

```bash
apexe scan git                         # basic scan
apexe scan ls jq curl                  # multiple tools
apexe scan git --depth 3               # deeper subcommand discovery
apexe scan git --no-cache              # force re-scan
apexe scan git --format json           # JSON output
apexe scan git --skills-dir ./out      # also write .claude/skills/cli.git.*/SKILL.md
apexe scan ls --overlay ~/.apexe/overlays/ls-gnu.json  # apply a curated overlay
```

### 4.2 `apexe serve`

Starts an MCP server exposing scanned tools to AI agents.

```
apexe serve [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--transport <TYPE>` | `stdio` | Transport: `stdio`, `http`, or `sse` (`sse` is deprecated upstream — see §10) |
| `--host <HOST>` | `127.0.0.1` | Host for HTTP/SSE transports |
| `--port <PORT>` | `8000` | Port for HTTP/SSE transports (1-65535) |
| `--explorer` | off | Enable the browser-based Tool Explorer UI. HTTP/SSE only — on stdio it warns and mounts nothing, since there is no HTTP surface to serve it on |
| `--bindings-dir <DIR>` | `~/.apexe/bindings/` | Directory containing binding files |
| `--name <NAME>` | `apexe` | MCP server name |
| `--show-config <TARGET>` | - | Print an integration snippet for `claude-desktop` or `cursor` and exit — see [§12](#12-integrating-with-ai-agents). Any other value is an error on stderr with a non-zero exit |
| `--prefix <PREFIX>` | - | Serve only modules whose id starts with `<PREFIX>` — excluded modules are not callable either |
| `--tags <TAGS>` | - | Serve only modules carrying every listed tag (comma-separated, AND) |
| `--acl <PATH>` | - | Path to ACL policy YAML file (the per-caller boundary) |
| `--auth <MODE>` | per-transport | `token` (default for HTTP/SSE), `jwt`, or `none`. Ignored for stdio |
| `--auth-token <VALUE>` | `APEXE_AUTH_TOKEN` | Bearer token for `--auth token`; one is generated and written to **stderr** at startup if unset |
| `--jwt-secret <VALUE>` | `APEXE_JWT_SECRET` | Signing secret for `--auth jwt` |
| `--allow-unauthenticated-bind` | off | Acknowledge `--auth none` on a non-loopback bind (otherwise refused) |
| `--allow-deprecated-sse` | off | Accepted and ignored; the defect it acknowledged was fixed in apcore-mcp 0.18. Hidden from `--help`, removal planned |
| `--enable-approval` | off | Prompt the connected MCP client for a human decision on every `requires_approval` module; a client that cannot be prompted is refused — see §9.6 |
| `--no-logging` | off | Disable structured logging middleware entirely |
| `--no-log-arguments` | off | Drop `inputs`/`output` from every log event, error records included; failures keep a payload-free record |
| `--no-circuit-breaker` | off | Disable CircuitBreakerMiddleware (on by default) |
| `--no-retry` | off | Disable RetryMiddleware (on by default; only ever retries idempotent timeouts) |
| `--metrics` | off | Enable `/metrics` (Prometheus) + `/usage` (JSON) — HTTP/SSE only |

```bash
apexe serve                                        # stdio (Claude Desktop/Cursor)
apexe serve --transport http --port 8000            # HTTP server
apexe serve --transport http --explorer             # HTTP + browser UI
apexe serve --show-config claude-desktop            # print integration config
apexe serve --transport http --metrics               # + /metrics and /usage
apexe serve --no-circuit-breaker --no-retry           # disable resilience middleware
```

### 4.3 `apexe a2a`

Starts an A2A agent server exposing scanned tools, sharing governance (ACL, logging, approval) with `apexe serve` via the same `Executor`.

```
apexe a2a [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--url <URL>` | `http://127.0.0.1:8000` | Base URL to bind the A2A server to |
| `--bindings-dir <DIR>` | `~/.apexe/bindings/` | Directory containing binding files |
| `--name <NAME>` | `apexe` | A2A agent name |
| `--explorer` | off | Enable browser-based Explorer UI |
| `--acl <PATH>` | - | Path to ACL policy YAML file |
| `--tags <T1,T2>` | - | Serve only modules carrying every listed tag. Excluded modules are neither advertised nor callable |
| `--prefix <PREFIX>` | - | Serve only modules whose ID starts with this prefix. Excluded modules are neither advertised nor callable |
| `--no-logging` | off | Disable structured logging middleware entirely |
| `--no-log-arguments` | off | Drop `inputs`/`output` from every log event, error records included; failures keep a payload-free record |
| `--no-circuit-breaker` | off | Disable CircuitBreakerMiddleware (on by default) |
| `--no-retry` | off | Disable RetryMiddleware (on by default; only ever retries idempotent timeouts) |
| `--execution-timeout <SECS>` | `300` | Per-task execution timeout in seconds |
| `--cors-origin <ORIGIN>` | - | Allowed CORS origin (repeatable) |
| `--auth <MODE>` | `token` | A2A transport authentication: `token`, `jwt`, `none` |
| `--auth-token <TOKEN>` | generated | Shared bearer credential, or `APEXE_AUTH_TOKEN` |
| `--jwt-secret <SECRET>` | absent | JWT signing secret, or `APEXE_JWT_SECRET` |
| `--allow-unauthenticated-bind` | off | Acknowledge `--auth none` on a non-loopback URL |

> **No `--enable-approval` on `apexe a2a`.** A2A has no interactive elicitation
> transport, so an approval prompt can never be resolved over it; the flag would
> only ever error. Approval on A2A is a library-only feature — construct
> `A2aServerBuilder` with an `ApprovalStore`. `apexe serve` keeps
> `--enable-approval`, which prompts the connected MCP client for a human
> decision — see §9.6.

> **A2A requires bearer authentication by default.** apcore-a2a provides an
> `Authenticator`, and apexe now wires token/JWT modes. `--auth token` generates
> a startup token when none is supplied. `--auth none` on a non-loopback bind
> requires `--allow-unauthenticated-bind`. The shared token authenticates as
> `apexe-token`; JWT preserves its subject and roles for ACL and audit.

```bash
apexe a2a                                       # http://127.0.0.1:8000
apexe a2a --url http://127.0.0.1:9000 --explorer  # custom port + browser UI
# A public unauthenticated bind requires both explicit opt-outs:
apexe a2a --url http://0.0.0.0:9000 --auth none --allow-unauthenticated-bind
apexe a2a --acl ~/.apexe/acl.yaml               # governed by an ACL policy
apexe a2a --prefix cli.git.                     # serve only the git modules
apexe a2a --tags readonly                       # serve only readonly modules
```

### 4.4 `apexe list`

Lists all registered modules from binding files.

```
apexe list [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--format <FMT>` | `table` | Output format: `table` or `json` |
| `--bindings-dir <DIR>` | `~/.apexe/bindings/` | Directory to read binding files from |
| `--verbose` | off | Print each module's behavioral annotations and, with `--acl`, the ACL decision an unauthenticated caller would get |
| `--acl <PATH>` | `<config_dir>/acl.yaml` if present | ACL policy file to evaluate against in `--verbose` output. Read-only report — does not enable enforcement anywhere |
| `--available-only` | off | Only list modules whose binary is reachable on this machine right now. `apexe serve`/`apexe a2a` apply this check unconditionally; here it is opt-in so the plain listing still shows everything ever scanned — see [Availability Filtering](#availability-filtering) |

### 4.5 `apexe config`

Shows or initializes apexe configuration.

```
apexe config [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--show` | Print resolved configuration as YAML |
| `--init` | Create default config at `~/.apexe/config.yaml` |

### 4.6 `apexe policy`

Prints the filesystem access boundary every wrapped tool is checked against, or
checks one path against it. This is the only way to inspect the path guard
(§9.7) without standing up a server and driving it with a client.

```
apexe policy [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--path <PATH>` | - | Check this one path instead of printing the whole policy. Resolved exactly as a wrapped tool's argument would be: joined to the working directory, symlinks followed, `..` folded afterwards |
| `--mode <MODE>` | `write` | Access mode `--path` is checked under: `write` is what every unannotated module gets, `read` is the narrower boundary a `readonly` module is held to |
| `--format <FMT>` | `table` | Output format: `table` or `json` |

With no `--path`, the summary is read off the **installed** guard rather than
re-derived from `config.yaml`, so it can never describe a policy other than the
one in force. It lists the working directory relative paths resolve against,
both compiled-in baselines, anything `additional_denied_paths` added, the
`allowed_paths` carve-outs, and the derived temp-directory exemption.

```bash
apexe policy                                   # the whole boundary
apexe policy --format json                     # same, machine-readable
apexe policy --path /etc/passwd                # would a writer be refused?
apexe policy --path /etc/passwd --mode read    # would a readonly module?
apexe policy --path ~/.ssh/id_rsa --mode read  # credentials bind readers too
```

`--path` calls the guard's own `check`, the same call a real argument goes
through, so its verdict cannot drift from what an invocation would get. The two
modes disagree exactly where §9.7 says they should — a system path is legible to
a reader and refused to a writer:

```
$ apexe policy --path /etc/passwd
DENY   /etc/passwd  (mode: write)
  the given --path resolves to '/private/etc/passwd', which is protected by '/private/etc'
  hint: '/etc/passwd' resolves to '/private/etc/passwd', inside or above the protected
        location '/private/etc'. This is a compiled-in boundary that no configuration
        removes. Choose a path outside the protected system and credential directories.

$ apexe policy --path /etc/passwd --mode read
ALLOW  /etc/passwd  (mode: read)
```

---

### 4.7 `apexe exec`

Execute a native command with passthrough output, or a scanned module with JSON
input and captured output. `apexe run` has been removed; there is no compatibility
alias. Native execution does not require activation or installing shims.

```bash
# Native argv: put apexe options before -- and child arguments after it.
apexe exec --dry-run -- ls -la
apexe exec -- ls -la
printf 'one\ntwo\n' | apexe exec -- cat | apexe exec -- head -n1

# Module JSON: use the property names in your scanned binding schema.
apexe exec cli.ls --input '{"file":["."]}'
apexe exec cli.ls --input '{"file":["."]}' --dry-run --as ci
```

| Option | Default | Description |
|--------|---------|-------------|
| `--input <JSON>` | `{}` | JSON object for module mode; conflicts with native argv |
| `--bindings-dir <DIR>` | configured bindings directory | Binding files to load |
| `--acl <PATH>` | `<config_dir>/acl.yaml` when present | Missing explicitly requested files and malformed policies fail closed |
| `--dry-run` | off | Report schema, ACL, approval, path-guard decisions and planned argv without execution |
| `--as <CALLER>` | `local` | Simulate a caller; accepted only with `--dry-run` |
| `--unmanaged <deny\|allow\|approval>` | `deny` | Native mode only: policy for commands with no registered CLI binding; loaded ACL still applies |

Native mode uses the same reviewed grammar, route selection and argv
reconstruction as PATH shims. A matching authoritative overlay, current scan
format and consistent capabilities are required. It checks the executable
resolved from the clean PATH against the binding; an absolute executable path
is accepted when it identifies the same binary. Installed shim directories are
excluded from lookup. Known commands with invalid arguments, unavailable or
ineligible bindings, ACL denials or removed installed routes never fall back to
unmanaged execution. Rescan when the executable or its grammar changes.

Native stdout and stderr pass through unchanged; module mode prints the captured
JSON result. Both modes return the exact child exit status (128 + signal on
Unix). Diagnostics go to the log files by default; `--log-stderr` mirrors them
to stderr. Failures remain visible on stderr. Pipes, redirections, wildcards and `&&` are
handled by the calling shell, not interpreted by `exec`. Use one `exec` call per
pipeline stage that needs governance. Child flags such as `--help`, `--man` or
`--timeout` belong after the separator.

Real execution uses the fixed `local` ACL caller. No ACL file means no access
control, with a warning. Timeout, environment scrubbing and audit remain active.
Dry-run prints `mode` (`module`, `native`, or `unmanaged`) and governance checks;
ACL-denied plans omit binary and argv details. Invalid governance checks produce
nonzero exit status. Command lookup and native grammar errors are reported on
stderr before a plan can be produced.

`approval_required` reports whether approval would be required. Local execution
has no MCP client to prompt, so calls requiring approval are refused; dry-run
can inspect them. `--as` is an asserted simulation identity. Module mode supplies
no stdin; reviewed native commands inherit stdin only when their overlay declares
`streaming_stdin_safe`. PTY sessions remain unsupported.

#### Explicit unmanaged execution

By default, commands absent from the bindings are refused. Opt in per call:

```bash
# Requires ACL permission for unmanaged.my-tool when an ACL is loaded.
apexe exec --unmanaged allow -- my-tool --version
apexe exec --unmanaged approval --dry-run -- my-tool --version
```

Unmanaged calls use module ID `unmanaged.<executable-basename>` and the same
executor, local caller, ACL, approval gate, timeout, environment scrubbing and
audit. `allow` does not override an ACL denial. Review and add a rule for that
module ID when your ACL defaults to deny. `approval` requires a human decision
and therefore refuses actual local execution with the current noninteractive
approval handler.

Unmanaged argv is passed literally without schema reconstruction. No argument
path typing is available, so schema-based path checks do not apply; dry-run
reports `path_guard_available: false`. stdin is null. This mode can run arbitrary
programs, including ones that launch other commands, and needs an external
sandbox when used by an agent. It does not certify an unknown command as safe.

These policies apply to calls through `apexe exec`. Activating PATH shims still
leaves uninstalled commands, builtins and shell functions outside apexe; use an
external execution boundary to require agents to enter through `exec`.

### 4.8 `apexe env`

The installed source helpers enable governed external-command shims, configure
automatic activation in future terminals, and activate the current terminal.
Use the installed apexe binary in bash or zsh; no repository checkout is required:

```bash
apexe scan cat head ls --no-cache
# Review ~/.apexe/acl.yaml, then install the shell helpers once.
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
printf 'one\ntwo\n' | cat | head -n1
ls -la
# Remove owned shims/startup blocks and restore the current terminal.
source ~/.apexe/bin/apexe-deactivate.sh
# Automatically re-enable and activate using the same installed helper.
source ~/.apexe/bin/apexe-activate.sh
```

POSIX sh uses `.` instead of `source`. Source the files in the terminal you want
to change; running them with `bash` or `sh` changes only that child process's
environment. The source helpers support sh, bash and zsh. For fish and PowerShell,
use the shell-specific setup below.

| Option | Default | Description |
|--------|---------|-------------|
| `--enable` | off | Create or update eligible shims, source helpers and managed shell startup blocks |
| `--reload` | off | Refresh an existing installation using its stored settings; automatically enable when absent |
| `--disable` | off | Remove owned shims and managed startup blocks; retain namespaced shell helpers; succeeds when already disabled |
| `--check` | off | Diagnose installed shims, current PATH resolution, bindings and ACL without executing commands |
| `--rc-file <PATH>` | detected startup file | Configure an explicit startup file with `--enable` or `--reload` |
| `--shim-dir <DIR>` | `<config_dir>/bin` | Directory for command links and their routing manifest |
| `--bindings-dir <DIR>` | configured directory | Binding snapshot to install from |
| `--acl <PATH>` | default ACL when present | Explicit policy path to store in the manifest |
| `--tags <TAGS>` | absent | Require every listed tag |
| `--prefix <PREFIX>` | absent | Restrict the eligible module IDs |
| `--shell <SHELL>` | detected for `--enable`; `sh` for printed activation | Choose `sh`, `bash`, `zsh`, `fish` or `powershell` |

`--enable` detects the shell from `SHELL` and integrates startup configuration
with shim installation. Startup blocks are validated before links are installed.
An unknown shell requires an explicit `--shell`. The startup block activates
only when the apexe executable and shim manifest exist. Once enabled, use native
commands directly; there is no per-command or per-terminal setup to repeat.
Use `--reload` when newly scanned eligible tools or subcommands change the shim
list or routing, or when changing the prefix/tag filter. For example, adding
reviewed `ls` to an installation previously covering `cat` and `head` needs
reload. Existing bindings are read during execution, so parameter descriptions
or schemas alone normally do not require rebuilding the shim list.

For an existing installation, reload retains its binding directory, binding
pattern, explicit ACL path, prefix and tags unless command-line options override
them. It preserves startup files by default and does not need shell detection.
An explicit `--shell` or `--rc-file` also updates startup configuration. When
there is no installed manifest, including after disable, reload automatically
performs enable with the current settings and flags. The source helpers also
activate the current terminal immediately; a direct `--enable` or `--reload`
call requires sourcing a helper or opening a new terminal.

| Shell | Default startup file |
|-------|----------------------|
| zsh | `$ZDOTDIR/.zshrc`, or `~/.zshrc` when `ZDOTDIR` is unset |
| bash | `~/.bashrc` and the first existing login file among `.bash_profile`, `.bash_login`, `.profile`; creates `.bash_profile` if none exists |
| fish | `$XDG_CONFIG_HOME/fish/config.fish`, or `~/.config/fish/config.fish` |
| sh / dash | `~/.profile`, loaded by login shells |
| PowerShell | Requires `--rc-file "$PROFILE"` in the target PowerShell host |

Use absolute `ZDOTDIR` and `XDG_CONFIG_HOME` paths. For custom shell startup
layouts or multiple installed shells, configure each explicitly:

```bash
apexe env --enable --shell zsh
apexe env --enable --shell bash --rc-file ~/.config/bash/interactive.rc
```

```powershell
apexe env --enable --shell powershell --rc-file "$PROFILE"
```

Enabling preserves unrelated text, existing file permissions and startup
file symlinks. It is idempotent and records ownership in
`<shim-dir>/.apexe-shell-init.json`. Disable removes only the recorded blocks,
preserving user edits outside them and leaving startup files in place. A managed
block changed by hand is refused; restore or remove that block before retrying.

New terminals calculate activation using their current PATH. An apexe subprocess
cannot modify the parent shell, so source a helper to change an existing terminal.
`--enable` and `--reload` install two namespaced files in `<shim-dir>`
(default `~/.apexe/bin`), even when apexe itself is installed in `~/.cargo/bin`:

```bash
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
source ~/.apexe/bin/apexe-deactivate.sh
```

`apexe-activate.sh` reloads the installed settings, automatically enabling when
absent, then activates the current terminal. `apexe-deactivate.sh` runs
`apexe env --disable` and, only after successful uninstall, restores the PATH
and APEXE_SHIM_DIR saved before the first activation. Repeated activation
preserves that original snapshot, including unset and empty values; no separate
`eval` or `rehash` is required. New-terminal startup and `eval "$(apexe env)"`
also save the environment so the same deactivation script can restore it.
Both namespaced scripts and their ownership record remain installed after
disable, including manual `apexe env --disable`, so they can be sourced again
without a checkout. For an older installation, run `apexe env --reload` once
to generate the current helper paths in `bin`; previously installed helpers
outside the shim directory are left untouched.

These scripts work like Python venv activation by changing the current shell.
apexe prepends command shims, while venv selects Python. The full namespaced
paths distinguish the two; apexe does not define or replace the venv's
`deactivate` function, change VIRTUAL_ENV or add a prompt prefix. Activate the
venv before apexe,
then deactivate apexe before the venv so the saved PATH is restored in order:

```bash
source .venv/bin/activate
source ~/.apexe/bin/apexe-activate.sh
# Use commands in the combined environment.
source ~/.apexe/bin/apexe-deactivate.sh
deactivate
```

The scripts use the installation's absolute executable and shim directory.
Their ownership record is `<shim-dir>/.apexe-source-scripts.json`;
modified files, symlinks or invalid records are refused before reload. Enable
and reload regenerate missing owned helpers. For compatibility, they also
create `<shim-dir>/activate` and `<shim-dir>/deactivate`, recorded in the shim
manifest; disable removes these compatibility files. `activate`, `deactivate`,
`apexe-activate.sh`, `apexe-deactivate.sh` and `.apexe-source-scripts.json` are
reserved names in the shim directory. Repository
`scripts/activate.sh` and `scripts/deactivate.sh` are optional development
conveniences, requiring apexe on PATH.

For custom shim directories, both scripts and their ownership record are
installed directly in that directory. Enable/reload prints their full paths:

```bash
apexe env --enable --shell zsh --shim-dir "$HOME/.apexe/custom-bin"
source "$HOME/.apexe/custom-bin/apexe-activate.sh"
apexe env --check --shim-dir "$HOME/.apexe/custom-bin"
source "$HOME/.apexe/custom-bin/apexe-deactivate.sh"
```

The helper selects its recorded shim directory even before APEXE_SHIM_DIR is
set. Pass installation options such as `--acl`, `--tags` and `--prefix` to
`apexe env --enable` or `--reload` before sourcing it. After disable removes the
manifest, re-enabling uses the current configuration and command-line defaults;
set nondefault bindings, ACL or filters again before activation if needed.

The deactivation helper changes the installation as well as the current shell:
it removes the recorded automatic startup blocks and owned shims. If uninstall
is refused because an owned file or block was modified, it returns failure and
keeps the current terminal environment active. Other open terminals retain their
existing environments; reopen them to load the cleaned startup configuration.

`--enable` and `--reload` write diagnostics to the log files without printing
activation scripts; use `--log-stderr` to mirror those diagnostics. A direct
`--disable` call cannot restore the parent shell; source
`~/.apexe/bin/apexe-deactivate.sh` (or the printed custom helper) to restore
its saved environment, even after uninstall.
With `--shim-dir`, use the same directory for enabling, reloading and disabling.
Enable, reload, disable and check are mutually exclusive operations.

`--rc-file` requires `--enable` or `--reload`.

Check the environment from the terminal where the shims should be active:

```bash
apexe env --check
apexe env --check --shim-dir /path/to/custom/shims
```

The check selects `--shim-dir`, then `APEXE_SHIM_DIR`, then `<config_dir>/bin`.
It reports `[PASS]`, `[FAIL]`, `[WARN]` and `[SKIP]` findings and a summary.
Exit status is 0 when there are no failures and 1 when any check fails;
warnings alone do not fail the check. It inspects the current process PATH
without removing shims, including each command's lookup priority and duplicate
directory entries, and validates owned links, executable permissions, compatibility
helpers and persistent namespaced scripts with their ownership record. Legacy
installations without source scripts receive a warning
to reload; recorded scripts that are missing or modified fail the check.
The installed executable must match the apexe execning the check; when multiple
installations exist, run the recorded executable or reload using the intended one.
The manifest must contain valid absolute paths and unambiguous command routes.
Bindings are loaded using the manifest's directory, pattern and filters, then
checked for runtime registration, native executable availability, matching
routes and current overlay capabilities. Retained refusal tombstones are
reported as failed bindings because those routes cannot execute.

ACL checks use the manifest's explicit policy, or the current default
`<config_dir>/acl.yaml` when no explicit path was installed. Missing or malformed
ACLs fail the check, including an installation running without access control;
target naming errors that would prevent runtime startup also fail. Unmatched
target patterns produce warnings. Independent checks continue after a failure;
checks requiring an unreadable manifest are skipped.

`--check` does not execute wrapped commands, create configuration directories,
write audit records, or change links and shell startup files. Installation
options such as `--acl`, `--bindings-dir`, `--tags` and `--prefix` cannot be
combined with it: it checks the installed settings. Shell aliases, functions
and command hashes are outside the process's visibility; use `type -a` in the
target shell to inspect those. Successful ACL loading does not prove that a
particular invocation will be allowed.

An eligible binding needs a matching authoritative overlay and an explicit
`cross_flag_ordering: false` assertion. Short bundles such as `-la` and attached
short values such as `-n20` need `short_flag_bundling: true`. Inherited stdin
needs `streaming_stdin_safe: true`; otherwise stdin is null. These are reviewed
overlay facts, never scanner guesses. Re-scan after updating an overlay: old
bindings cannot acquire capabilities just by reinstalling links. Overlay format
1.1 introduces these fields; format 1.0 remains supported for scanning.

The locally verified BSD pilot covers `cat`, `head` and `ls` in the companion
corpus. Other entries remain scan-only until their native grammar is reviewed.
Unknown flags, unsupported grammar and mutually exclusive options are refused.
The parser builds typed JSON, and the executor validates it and rebuilds the
actual argv; no unresolved tokens are forwarded. Subcommands route through the
original `command_path`, and a selected subcommand's refusal is final.

stdout and stderr are inherited; there is no JSON envelope or output buffering.
A child exit code reaches the shell unchanged; a POSIX signal returns `128 +
signal`. POSIX children restore default SIGPIPE, so an upstream pipeline stage
exits quietly when its reader closes. The timeout still bounds execution;
the captured-output byte cap does not apply to passthrough. ACL, approval,
path checks, environment scrubbing and audit stay active. A call requiring
approval is refused because this local surface has no MCP client to prompt.
The shim uses the current `default_timeout` / `APEXE_TIMEOUT` configuration.

Activation is idempotent. Scanners and wrapped children remove managed shim
directories from PATH. Reinstalling updates owned links and refuses name
collisions or unowned files. Removed commands remain refusal tombstones until
an explicit uninstall; a missing module never falls through to a native binary.
Installation uses symbolic links on Unix and hard links on Windows.

Shims cover external commands only. Builtins, aliases, functions and absolute
paths can bypass them; a local human gets an audit trail and mistake guard,
while enforcement requires an externally controlled PATH and filesystem.
Per-call latency measurements and their limits are in the
[F8 delivery record](features/v2-f8-local-execution.md#10-delivery-record).

The [runnable local execution example](../examples/local_execution/README.md)
uses a temporary HOME and working directory. From the repository root:

```bash
cargo build --all-features
python3 examples/local_execution/demo.py --apexe target/debug/apexe
# On macOS, with the reviewed companion corpus available:
python3 examples/local_execution/demo.py --apexe target/debug/apexe \
  --corpus ../cli-permissions/overlays --shims
```

The default demo checks captured execution, preflight, ACL denial and audit.
The `--shims` variant also checks a native pipeline and owned uninstall.

### 4.9 `apexe verify`

Re-scan source help/man/completion contracts without replacing stored bindings.
The baseline is captured before overlays are applied, so an authoritative overlay
cannot hide a source upgrade. Changes are classified as `breaking`, `additive`,
`cosmetic` or `review`; unparsed source-text changes require review. Breaking,
review and unverifiable checks exit nonzero. `--strict` also fails on additive
or cosmetic changes. A missing baseline requires a rescan, never an invented pass.

```bash
apexe scan cat head ls --no-cache
apexe verify --report ./verification.json
apexe verify --cases ./readonly-cases.json --report ./verification.json
```

| Option | Description |
|---|---|
| `--bindings-dir <DIR>` | Binding baseline to compare |
| `--depth <N>` | Fresh subcommand scan depth (1-5, default 2) |
| `--strict` | Fail on every detected change |
| `--cases <PATH>` | Explicit JSON behavioral cases; none execute by default |
| `--overlay <PATH>` | Reapply an explicit overlay used by the original scan |
| `--report <PATH>` | Save JSON with binding, binary and case digests; destination must be outside the binding baseline directory |

```json
{"allowlist":["cli.cat"],"cases":[{"module_id":"cli.cat",
 "input":{"file":["$FIXTURE/input.txt"]},
 "fixtures":{"input.txt":"hello\n"},"expected_stdout":"hello\n"}]}
```

Behavioral admission is conjunctive: explicitly allowlisted, readonly/idempotent,
non-destructive, no approval requirement and no network boundary. The default readonly case mode
uses an independently reviewed binary allowlist is system cat/head/ls; other modules are
recorded as `unverifiable`. String inputs are restricted to fixture paths, selected flags must have literal
option spellings, and system cat/head/ls do not accept appended JSON-output
flags. Every path operand must reference `$WORKDIR` or
`$FIXTURE/name`; real paths and long-running options are refused. Each case uses
an isolated temporary HOME/work directory and a governed `apexe exec` child,
with a scrubbed environment, a temporary default-deny ACL allowing only the
selected readonly module, and bounded timeout. Expected stdout and exit zero
must match. A failed case is `failed`, an inadmissible case `unverifiable`.

`passed` is earned for the specific case and hashes in the report. The corpus's
legacy `confidence: verified` remains an author assertion backed by provenance;
apexe caps source-derived flag confidence at `high`. No case requested means
behavioral `unverifiable`, without failing the drift-only command.

| Additional option | Description |
|---|---|
| `--allow-dry-run` | Requires `--cases`; authorize explicitly reviewed, digest-pinned dry-run cases only |
| `--isolation <MODE>` | `auto` (default), `required` or `none`; dry-run and isolated modes always require a working backend |
| `--record` | Requires `--cases`; write digest-bound evidence for passed cases and unchanged source contracts |

```bash
apexe verify --cases ./readonly-cases.json --record --report ./verification.json
apexe verify --cases ./dry-run-cases.json --allow-dry-run --isolation required --record
python3 examples/verification/demo.py --apexe target/debug/apexe
```

`readonly` is the default case mode. `dry_run` additionally requires an exact
`binary_digest`, a declared boolean `dry_run_parameter` set to true, one of the
literal switches `--dry-run`, `--check`, `--noop`, `--plan` or `--whatif`, and
working OS isolation. Its temporary binding authorizes that scoped invocation;
normal binding annotations and approvals are preserved. Fixture inputs are
read-only. `isolated` admits explicitly digest-pinned readonly/idempotent,
non-destructive, non-networked modules under mandatory isolation, including
literal subcommands matching `command_path`. `expected_stderr` defaults to
empty; optional `expected_files` checks temporary fixture effects.

macOS uses system `sandbox-exec`. Linux requires `bwrap` and enabled unprivileged
user namespaces, with read-only system mounts, private network/IPC/PID namespaces
and writable case data for non-readonly idempotent cases. `required` refuses unavailable isolation; `auto` can
fall back only for the independently reviewed readonly binary allowlist. Other
platforms report required isolation as unavailable. Cases never run without an
explicit case file, and admission failures remain `unverifiable`.

`--record` preserves binding documents and writes
`.apexe-verification/evidence.json` beneath the binding directory. Subsequent
loads attach `metadata.confidence: verified` with `scope: explicit_cases` only
when binding, executable, case-file and case digests match. Modified executor
metadata, missing/changed case files, executable or verifier updates invalidate evidence.
A recording run replaces the snapshot; failed cases cannot retain a previous
passed verdict. Corrupt or symlinked records fail loading. Retain the case files
for reproducibility and restart servers after revalidation.
See the [verification guide](verification.md) and
[runnable example](../examples/verification/README.md) for the full format and
trusted local-state boundary.

### OpenAPI sources on `serve` and `a2a`

See the [OpenAPI guide](openapi.md) and its
[runnable MCP/A2A example](../examples/openapi_import/README.md).

Both servers can import an OpenAPI 3.0/3.1 document at startup, alongside the
CLI bindings. Imported operations are HTTP proxy modules, not CLI targets;
no generated binding files or helper programs are needed.

To run both MCP and A2A against a temporary local API, use Python 3 from the
repository root:

```bash
cargo build --all-features
python3 examples/openapi_import/demo.py --apexe target/debug/apexe
python3 examples/openapi_import/demo.py --apexe target/debug/apexe --remote-spec
```

The demo checks path/query parameters, JSON bodies, ACL denial and independent
transport/API credentials. `--remote-spec` additionally checks protected
specification fetching with a separate credential.

```bash
apexe serve --openapi ./openapi.yaml --openapi-base-url https://api.example.com --acl ./acl.yaml
apexe a2a --openapi https://api.example.com/openapi.json --openapi-prefix api.inventory --acl ./acl.yaml
```

| Option | Default | Description |
|--------|---------|-------------|
| `--openapi <SOURCE>` | absent | Local JSON/YAML file or exact HTTP(S) specification URL |
| `--openapi-base-url <URL>` | document `servers[0].url` | Absolute HTTP(S) API base URL |
| `--openapi-prefix <PREFIX>` | `api` | Canonical apcore namespace for imported operation IDs; `getPet` becomes `api.get_pet` |
| `--openapi-header <NAME: VALUE>` | absent | Repeatable specification-fetch header; never forwarded to API calls |
| `--openapi-token <TOKEN>` | `APEXE_OPENAPI_TOKEN` | Upstream API bearer credential; separate from MCP `--auth-token` |

`--tags` and `--prefix` filter both sources before serving. Imported operations
use the same schema validation, ACL, approval configuration and audit sink.
ACL rules may target `api.*` or individual operation IDs. API operations are
marked `open_world`; DELETE operations also require approval when the approval
gate is enabled. `--timeout` governs specification fetching and proxied requests.
Missing origins, malformed documents, registration failures and ID collisions
fail startup. The source is read again on each startup; it is not persisted.
`--show-config` preserves the source, base URL and namespace, resolving local
sources to absolute paths. Credentials and specification-fetch headers are
omitted; supply credentials separately in the client launch environment.

The current SDK proxy handles path parameters, query parameters on GET/DELETE/
HEAD/OPTIONS/TRACE, and JSON bodies on POST/PUT/PATCH. A body method declaring
query parameters is refused because the SDK would put them in the body.
Multipart uploads, per-input header/cookie parameters and non-JSON responses
are outside this initial import surface. The local subprocess path guard does
not govern paths on a remote API. Review the imported API's permissions and
write operations when authoring its ACL.

### 4.10 `apexe init`

Initialize this machine with matching command rules, bindings and a starter ACL.
This is the normal first step after installation. `config --init` only writes a
configuration template, and `scan <TOOLS>` remains the custom scanning entry.
The old `overlays` subcommand has been removed.

```bash
apexe init
apexe list --verbose
# Review ~/.apexe/acl.yaml before execution.
apexe exec cli.ls
# Optional native argv and shims: require modules in native_ready.
apexe exec --dry-run -- ls -la
# Optional shell integration or MCP service.
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe serve --acl ~/.apexe/acl.yaml
```

| Option | Default | Behavior |
|---|---|---|
| `--overlays <SOURCE>` | `default_overlay_repository` | HTTPS/GitHub `OWNER/REPO` source or local Git repository for this invocation |
| `--ref <REF>` | stored reference, initially `HEAD` | Select a branch, tag or commit; a changed explicit reference fetches that revision |
| `--refresh` | off | Update the selected rules snapshot and regenerate unchanged init-owned bindings |
| `--offline` | off | Disable network downloads; cached snapshots and local Git sources remain available. Also accepts `APEXE_OFFLINE=true` |
| `--format <table\|json>` | `table` | Print initialization results; JSON includes `corpora`, `degraded`, `warnings`, `tools`, `written`, `preserved`, `native_ready`, binding directory and ACL path |

Table output shows progress while preparing rules, scanning each candidate
tool and saving bindings and ACL. Each progress line is flushed immediately,
even with logging disabled. JSON output contains only the final report.

The initializer resolves the configured source, normally cli-permissions, and
loads candidate command names from available rules. It checks executables on a
PATH with shim directories removed, then uses the shared scanner to detect
variants and match platform, version and probes. Absent commands and unmatched
variants are skipped. It does not scan the entire PATH or install programs.
Native exec and env still require reviewed native capabilities; a binding alone
does not imply native support.

Git snapshots are validated and stored under `<config_dir>/corpora/`. Reports
include repository, recorded commit and corpus metadata. An optional root
`manifest.json` defines the name, SemVer release and overlay directory; legacy
repositories use their basename and `overlays/`. Repository scripts, Git hooks
and submodules are not run by the data installer. Tool help/version/probe calls
remain part of scanning.

Normal initialization reuses the recorded snapshot, discovers newly installed
covered tools and preserves existing bindings. Refresh only replaces bindings
that still match the fingerprint recorded by init; custom scans and edited
bindings are preserved. No bindings are deleted when a tool or rule disappears.
Manual ACL rules, existing defaults and config files are preserved. An invalid
existing ACL fails before binding generation. A missing ACL is recreated from
available initialized bindings. A new policy defaults to deny. Initialization
also leaves shell startup and server enforcement unchanged.

```bash
apexe init --overlays ../cli-permissions --ref <COMMIT>
apexe init --overlays ../cli-permissions --refresh
apexe init --offline --format json
# Refresh active shim links after binding changes.
apexe env --reload
# Deliberately regenerate a custom or edited binding when wanted.
apexe scan my-tool --no-cache
```

`--overlays` does not rewrite configuration. To make a mirror or source the
standing default, set `default_overlay_repository` in `config.yaml` or
`APEXE_DEFAULT_OVERLAY_REPOSITORY`. A commit-pinned source stays pinned until its
reference is explicitly changed. Older validated snapshots remain available for
concurrent readers. Refresh updates the selected source, not every installed
corpus.
Cached sources match by repository identity, including installations under an
alias; a corpus name alone never substitutes for a newly configured source.

Initialization writes canonical `*.binding.yaml` filenames. A configured
apcore `bindings.pattern` must include new filenames, or initialization fails
before writing bindings or ACL. Existing bindings excluded by the pattern are
preserved; the report marks degraded coverage and warns that they are unavailable
to execution.

If the default source is unavailable, available local rules are still used. With
no usable rules, init reports degraded fallback coverage and scans only cat,
head, ls and wc using built-in classification. These bindings have no reviewed
native shim contract. Any explicit-source acquisition/validation failure, failed refresh, scan failure
or run with no applicable initialized/preserved modules exits nonzero. JSON
reports are emitted for completed discovery runs, including partial failures;
source/configuration failures can stop earlier with diagnostics on stderr.

For uncommitted local rule development, use `overlay_dirs` or an invocation-local
`APEXE_OVERLAY_DIRS` instead of installing a Git snapshot. Selection precedence
and author verification requirements remain documented in
[Authoring Tool Overlays](overlays.md).

## 5. Configuration

Configuration resolves in 4 tiers (highest priority wins):

```
CLI flags  >  Environment variables  >  Config file  >  Defaults
```

**`bindings_dir` has a fifth tier, and apcore owns it.** `bindings.dir` is
apcore's canonical key for the directory holding `.binding.yaml` files
(PROTOCOL_SPEC §9.1.1). apexe used to carry a second name for the same
location, which is the kind of duplication that ends with the two disagreeing,
so where an `~/.apexe/apcore.yaml` **declares** `bindings.dir`, that value beats
apexe's own `config.yaml`:

```
--bindings-dir  >  APEXE_BINDINGS_DIR  >  apcore.yaml bindings.dir  >  config.yaml bindings_dir  >  default
```

The two override tiers stay on top because they are per-invocation instructions
rather than a second standing opinion. Only a *declared* value counts: apcore
answers `./bindings` for a configuration that declares nothing, and taking that
would replace apexe's absolute default with a working-directory-relative path on
every machine that merely has an `apcore.yaml` — `apexe scan` would write beside
wherever you were standing, and `apexe serve` from elsewhere would find nothing.
apexe logs at `info` when apcore's declaration is the one that decided.

**`bindings.pattern` is apcore's too, and apexe holds no copy of it.** The glob
selecting binding files inside that directory is read from the same
`apcore.yaml`, on the same declared-only terms. There is no apexe config key and
no default value here: when nothing declares the key, apexe passes nothing and
apcore-toolkit's loader applies the identical canonical default
(`*.binding.yaml`) itself — so there is no second string that could drift. The
pattern matches the **file name** only; `*` and `?` are the metacharacters, and
recursion is not expressible through it (apexe loads one directory,
non-recursively). Honouring this key at all needs apcore-toolkit 0.12 or later:
before it the loader took no pattern, so the key could be resolved and not
applied.

**Binding validation (apcore-toolkit 0.14.0).** `serve`, `a2a`, and `list`
load bindings in strict mode. Unknown document or entry keys, a missing half
of the `input_schema` / `output_schema` pair, and non-list `tags` fail loading
with a diagnostic naming the file. Move custom entry fields into `metadata`,
provide both inline schemas, and use a YAML list for `tags`. Runtime-only
`auto_schema` and `schema_ref` are unsupported because apexe loads pure data
and constructs its own governed CLI modules. Bindings generated by `apexe
scan` already satisfy these requirements; hand-edited bindings may need these
changes.

### Config file

Located at `~/.apexe/config.yaml`. Create with `apexe config --init`.

```yaml
bindings_dir: ~/.apexe/bindings
cache_dir: ~/.apexe/cache
audit_log: ~/.apexe/audit.jsonl
log_level: warn
default_timeout: 30
scan_depth: 2
json_output_preference: true

# Read reviewed command facts from an external overlay corpus. Paths are loaded
# in order; a local ~/.apexe/overlays entry still takes precedence.
overlay_dirs:
  - /opt/cli-permissions/overlays

# Appended to the compiled-in path-guard baselines. See section 9.7.
additional_denied_paths:
  - /srv/production-data

# Carved OUT of those baselines. Empty by default; you own what you open.
allowed_paths:
  - /etc/nginx/conf.d
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `bindings_dir` | path | `~/.apexe/bindings` | Binding file storage |
| `cache_dir` | path | `~/.apexe/cache` | Scan result cache |
| `audit_log` | path | `~/.apexe/audit.jsonl` | Audit trail file |
| `log_level` | string | `warn` | Diagnostic file log level: off, error, warn, info, debug, trace |
| `default_timeout` | integer | `30` | CLI subprocess timeout (seconds) |
| `scan_depth` | integer | `2` | Default subcommand recursion depth |
| `json_output_preference` | boolean | `true` | Prefer JSON output from CLI tools when available |
| `overlay_dirs` | list of paths | `[]` | Explicit local corpus directories; skip automatic downloads and load before personal overlays |
| `default_overlay_repository` | string | `https://github.com/aiperceivable/cli-permissions` | Default initialization and automatic scan source; HTTPS mirror or local Git repository supported |
| `additional_denied_paths` | list of paths | `[]` | Locations to deny **in addition to** the path-guard baselines (§9.7) |
| `allowed_paths` | list of paths | `[]` | Carve-outs **out of** the path-guard baselines (§9.7). The only setting that relaxes the guard; you own the consequences |

The key set above is closed. A key apexe does not read is **ignored**, and a
startup diagnostic names the key and the one it was probably meant to be.
Read the log files or use `--log-stderr` to display it:

```
WARN Ignoring unrecognised config key 'additional_denied_path' -- did you mean
     'additional_denied_paths'? It has no effect. That path-guard setting is
     NOT in force.
```

The last sentence appears only for `additional_denied_paths` and
`allowed_paths`, because those are the two whose loss changes what the process
will permit: a misspelled `additional_denied_paths` leaves only the path-guard
baseline in force, and nothing else in the run would tell you.

An unrecognised key does **not** stop apexe or discard the rest of the file —
the remaining keys still apply.

### Environment variables

| Variable | Overrides | Example |
|----------|-----------|---------|
| `APEXE_BINDINGS_DIR` | `bindings_dir` | `/opt/apexe/bindings` |
| `APEXE_CACHE_DIR` | `cache_dir` | `/tmp/apexe-cache` |
| `APEXE_LOG_LEVEL` | `log_level` | `debug` |
| `APEXE_TIMEOUT` | `default_timeout` | `120` |
| `APEXE_SCAN_DEPTH` | `scan_depth` | `3` |
| `APEXE_OVERLAY_DIRS` | `overlay_dirs` | `/opt/cli-facts/overlays:/srv/team-policy` |
| `APEXE_DEFAULT_OVERLAY_REPOSITORY` | `default_overlay_repository` | `https://example.com/team/cli-permissions.git` |
| `APEXE_OFFLINE` | scan `--offline` | `true` |

`APEXE_OVERLAY_DIRS` is `:`-separated like `PATH`, and *replaces* the config
file's list rather than appending to it, as every other variable here does.
Empty segments are dropped, so a stray leading or trailing `:` does not resolve
to the working directory.

---

## 6. Scanning Engine

apexe uses a three-tier deterministic scanning engine. No LLM is involved.

A scan that matches a curated overlay reports **tier 4** (§6.5). The three tiers
below are the ones that read the binary; tier 4 is a human-reviewed description
applied on top of what they found, so a tool with no matching overlay tops out
at tier 3.

### Tier 1: `--help` Parsing

Runs `<tool> --help` and auto-detects the help format. Six built-in parsers, tried in the order below:

| Parser | Detects | Examples |
|--------|---------|---------|
| **Man** | A `--help` that is actually a man page (`NAME` + `SYNOPSIS` at column 0) | every `git <subcommand>` — `git log --help` runs `man git-log` |
| **BSD Usage** | Single-line bundled usage (BSD/macOS built-ins that reject `--help`) | ls, cat, chmod, sort (macOS) |
| **GNU** | Standard GNU-style help | ls, grep, curl, git (GNU/Linux) |
| **Click** | Python Click / argparse | aws, pip |
| **Cobra** | Go Cobra framework, including grouped Docker command lists and typed `Options`/`Global Options` | kubectl, docker, gh |
| **Clap** | Rust Clap framework | ripgrep, fd, bat |

Extracts: subcommands, flags (long/short), positional args, types, defaults, enum values, descriptions. Each extracted flag records source confidence (`high` > `medium` > `low`); `verified` is earned module evidence for explicit cases through `apexe verify --record`, rather than a scan-source label.

### Tier 2: Man Page Enrichment

Parses `man <tool>` output — both GNU (`OPTIONS` section) and BSD (options listed inside `DESCRIPTION`) layouts:

- **DESCRIPTION section**: Enriches commands that have sparse descriptions (< 20 chars).
- **OPTIONS section**: Contributes flags directly to `global_flags` (not just enrichment) — this is what makes tools whose `--help` is a single bundled usage line (most BSD/macOS built-ins) scan to a full flag list instead of zero.
- **EXAMPLES section**: Hand-written invocations are extracted into `ScannedCLITool.examples` / `CommandContract.examples` — the only human-reviewed usage a scan can reach, since flag parsing alone can't say which flag combinations make sense together.

### Tier 3: Shell Completion Discovery

Parses zsh/bash completion scripts from standard paths:
- `/usr/share/zsh/functions/Completion/_<tool>`
- `/usr/local/share/zsh/site-functions/_<tool>`
- `/etc/bash_completion.d/<tool>`

Discovers subcommands that Tier 1 missed and merges them into the result (added as stubs with a warning).

### Subcommand Discovery

For tools with subcommands, apexe recursively runs `--help` on each subcommand up to `--depth` levels. For example, with `--depth 2`:

```
git --help          → discovers: commit, push, remote, ...
git remote --help   → discovers: add, remove, show, ...
```

### Caching

Scan results are cached in `~/.apexe/cache/`. Cache entries are keyed by tool name + **variant** + version (`<name>@<variant>_<version>.scan.json`), so a machine with both BSD `/bin/ls` and Homebrew's GNU `ls` caches them separately instead of one overwriting the other. Use `--no-cache` to force a fresh scan.

### 6.4 Tool Variant Detection

Every scan probes the binary (`<binary> --version`) and classifies it into `ScannedCLITool.variant`:

| Variant | Example |
|---------|---------|
| `bsd` | macOS system `/bin/ls`, `/usr/bin/grep` |
| `gnu` | Linux coreutils, Homebrew GNU tools on macOS |
| `apple` | Apple-authored ports (`sort`, `git` shipped with Xcode CLT) |
| `busybox` | BusyBox-based Linux (Alpine, embedded) |
| `unknown` | Version probe inconclusive |

The same command name can be a different program depending on the host, and BSD/GNU/Apple builds of the same tool frequently expose different flag sets — variant detection is what lets a scan (and a matching overlay, see below) pick the right one.

### 6.5 Tool Overlays

An overlay is a curated, human-reviewed description of one tool variant, keyed by `(command, variant, version_range)`. apexe ships no overlays itself. The independently maintained [cli-permissions](https://github.com/aiperceivable/cli-permissions) corpus contains reviewed POSIX-core and `sed` variants, and apexe automatically obtains and consumes a cached snapshot at scan time.

- **`mode: authoritative`** replaces the scan result for that command entirely.
- **`mode: merge`** keeps the scan as the base and only overrides the flags the overlay declares — a gap in the overlay degrades to the scanner's answer instead of erasing a real flag.
- **`confidence: verified`** requires a `provenance` block (platform, version, source document, date) recording how the overlay was checked; the schema rejects a `verified` overlay without it.
- Overlays are the only source that can express `conflicts_with` (mutually exclusive flags) and `long_running` (a flag that may block indefinitely, e.g. `tail -f`) — no `--help`/man format expresses either machine-readably.
- Overlays can also override behavioral annotations (`readonly`/`destructive`/`idempotent`/`requires_approval`/`open_world`) for a specific command. `open_world` is the one inference reads off a *name list* rather than the command's own surface, so it is the one an overlay most often has to correct: GNU sed runs `s///e` as a shell command while BSD sed rejects the flag, and the name `sed` cannot tell them apart.

The default corpus needs no separate install step. Install another managed corpus
with `apexe init --overlays <REPOSITORY>`, load one explicit overlay with `apexe scan <tool> --overlay <PATH>` (JSON or YAML), add a corpus directory through `overlay_dirs` or `APEXE_OVERLAY_DIRS`, or place a local correction under `~/.apexe/overlays/`. The format is defined by [`tool-overlay.schema.json`](https://github.com/aiperceivable/cli-permissions/blob/main/schemas/tool-overlay.schema.json), which ships with
the corpus rather than with apexe. See the [cli-permissions example](../examples/cli_permissions/README.md) for an operator walkthrough, [`docs/overlay-consumers.md`](overlay-consumers.md) for reading overlays from
outside apexe, and [`docs/overlays.md`](overlays.md) for the full authoring and verification procedure — writing a `verified` overlay from memory instead of a real installation is exactly what it warns against.

---

## 7. Schema Generation

Each scanned flag/argument becomes a JSON Schema property.

### Type Mapping

| CLI Type | JSON Schema | Example |
|----------|-------------|---------|
| String | `"type": "string"` | `--message "hello"` |
| Integer | `"type": "integer"` | `--count 5` |
| Float | `"type": "number"` | `--ratio 0.5` |
| Boolean | `"type": "boolean"` | `--verbose` |
| Path | `"type": "string", "format": "path"` | `--config /etc/app.yaml` |
| URL | `"type": "string", "format": "uri"` | `--url https://...` |
| Enum | `"type": "string", "enum": [...]` | `--format json\|yaml\|table` |

### Special handling

- **Required flags**: Added to the schema's `required` array.
- **Repeatable flags** (`--include a --include b`): Wrapped as `"type": "array", "items": {...}`.
- **Default values**: Included with type-correct coercion (`"10"` becomes `10` for integers).
- **Boolean defaults**: `false` unless explicitly set.
- **Format hints**: `Path` and `URL` types emit `"format"` so AI agents can distinguish paths from plain strings.

### Output Schema

Tools with detected JSON output flags get an enhanced output schema:

```json
{
  "type": "object",
  "properties": {
    "stdout": { "type": "string" },
    "stderr": { "type": "string" },
    "exit_code": { "type": "integer" },
    "json_output": { "type": "object" }
  }
}
```

### 7.1 Contract Keywords

A generated schema is valid JSON Schema, plus a set of `x-` extension keywords
that carry what a JSON Schema cannot say about a command line: which literal to
render, where in argv it goes, and what the executor must refuse. They are
extension keywords rather than constraints on purpose — `follow: true` is a
perfectly valid *value*, and what these tell an executor is how to render it or
whether to refuse, not that the input is malformed.

Read this table if you are writing an MCP client against apexe, consuming
`apexe scan --format json`, or authoring an overlay.

| Keyword | Where | Value | Meaning |
|---------|-------|-------|---------|
| `x-apexe-flag` | property | string | The literal spelling the executor renders for this property (`--message`, `-maxdepth`). The long form when a flag has both |
| `x-apexe-positional` | property | integer | This property is a positional operand, not a flag. The integer is its order among operands |
| `x-apexe-flag-position` | property | `"before-operands"` \| `"before-subcommand"` | Where this flag goes. `before-subcommand` is derived for a subcommand's inherited global flags; `before-operands` is a curated assertion and is never overwritten by the derived rule |
| `x-apexe-operand-position` | property | `"before-flags"` | This operand precedes every flag. Set for `find`'s paths, whose grammar is `find path … [expression]` — both variants reject the other order outright |
| `x-apexe-path` | property | `true` | The value is a filesystem path, so the path guard (§9.7) resolves and checks it. Emitted for `ValueType::Path`; a value the scan did not type as a path is not checked |
| `x-apexe-value-optional` | property | `true` | The flag's value is optional (`--color[=WHEN]`), so the executor must render `--flag=value` rather than two argv entries. Paired with a union type so both spellings are expressible |
| `x-apexe-conflicts-with` | property | array of property names | Flags that must not be sent together — whether the binary diagnoses the pair or silently resolves it last-one-wins. A JSON object has no key order, so for a last-wins pair *which* flag wins would otherwise be unpredictable |
| `x-apexe-long-running` | property | `true` | This flag may not terminate on its own (`tail -f`). Bound the timeout or refuse; it is not a reason to reject the input |
| `x-apexe-escalates` | property | `true` \| `false` | Three-state with absence. `true` sends the call to the approval gate when the flag is present; `false` is a human overruling the name list for this flag; absent defers to the name list (§8) |
| `x-apexe-exec` | property | `true` | The value **is** a command the wrapped tool will run. The executor refuses any call that fills it (§8) |
| `x-apexe-end-of-options` | **schema root** | `true` | The tool honours the `--` separator. Root-level because `--` describes the whole invocation, not one property. Permits a `-`-leading value and tells the executor where to emit the separator |
| `x-sensitive` | property | `true` | The value is credential-bearing, so the logging middleware redacts it (§10). Note the different prefix — this one is apcore's, not apexe's |

Two more live on the module's **annotations**, not the schema, and are written
together by the converter:

| Key | Value | Meaning |
|-----|-------|---------|
| `x-apexe-approval-basis` | `"flags"` | `requires_approval` was set because the command *accepts* an escalating flag, not because it is destructive by name — so the gate may stand down (§8) |
| `x-apexe-escalating-params` | array of property names | Which properties those are. A `"flags"` basis without this list is a corrupted annotation, and the gate prompts rather than standing down |

A binding written by an older apexe carries only the keywords that existed then.
Re-scan with `--no-cache` to pick up new ones — upgrading apexe does not rewrite
bindings already on disk.

---

## 8. Behavioral Annotations

apexe infers behavioral annotations from the command's name, from the tool it
belongs to, and — for approval only — from the arguments an individual call
actually sends.

### Command Name Patterns

| Annotation | Trigger Patterns |
|------------|-----------------|
| **readonly** | list, ls, show, get, status, info, version, help, describe, view, cat, log, diff, search, find, check, inspect, display, print, whoami, env, top, ps |
| **destructive** + **requires_approval** | delete, rm, remove, destroy, purge, drop, kill, prune, clean, reset, format, wipe, erase |
| **idempotent** | get, list, show, status, info, describe, version, help, check |
| **cacheable** | (readonly AND idempotent) |

### Command Executors Outrank the Name Lists

An executable whose arguments *are* another command to run is classified
**destructive** whatever its name says: `env`, `xargs`, `nice`, `ionice`,
`nohup`, `timeout`, `chroot`, `setsid`, `stdbuf`, `script`, `watch`, `time`,
`sudo`, `doas`, `su`.

This is not a refinement. `env` is in the readonly list above, and BSD `env`
takes `-S "rm -rf /etc"` — a string it splits into argv and executes. Marking
these `destructive` flips the generated ACL from allow to deny and makes the
path guard judge their arguments as a writer's. It does not make them safe:
nothing can statically decide what argv a caller-supplied string becomes. See
[threat model §5.9](threat-model.md), which recommends keeping them out of the
registry entirely.

### Approval Escalation Is Per Call, Not Per Command

A scan sees the flags a command *accepts*; a call carries the flags a caller
*sent*. Only the second is a reason to interrupt a human, so the two are kept
apart:

| Stage | What it does |
|-------|-------------|
| **Scan** | A command that accepts any of `--force`, `-f`, `--hard`, `--recursive`, `-r`, `--all`, `--prune`, `--no-preserve-root`, `--cascade`, `--purge`, `--yes`, `-y` is marked `requires_approval` as a **ceiling**, and the schema properties carrying them are recorded in `x-apexe-escalating-params`, with `x-apexe-approval-basis: "flags"` saying the mark came from accepted flags alone |
| **Call** | With `--enable-approval`, the gate (§9.6) reads that list against the arguments actually sent and prompts only if one of them carries an effective value. Otherwise it stands down, recording `approved_by: "apexe-approval-gate"` so an audit reader can tell it from a human's yes |

**Example**: `git log` accepts `--all`, so the binding is marked
`requires_approval`. A plain `git log` sends no escalating argument and runs
without a prompt; `git log --all` prompts. `git push --force` prompts;
`git push` alone does not. A command marked `destructive` by name has no such
basis recorded and always prompts — no absent flag makes `rm` safe.

> **There is no `--dry-run` → `idempotent` boost.** It existed and was removed:
> a command that *accepts* `--dry-run` does not run dry, so the boost made
> `cacheable` true for a mutating command whenever the caller left the flag off,
> and the next reader was served a cached result for a call that changed
> something. Nothing consults `idempotent` at invocation time, so there was no
> per-call hook to move it to. Use an overlay's `annotation_overrides` to assert
> it for a specific command.

### Flag Risk

Three assertions an overlay can make about one flag, which no `--help` format
expresses and no scan can infer. Help text does not distinguish
`--exec-path=<path>`, which sets a search path, from `--upload-pack=<cmd>`,
which runs one — both read as "takes a value" to every parser.

| `risk` | Meaning | Effect | Contract keyword |
|--------|---------|--------|------------------|
| absent / `none` | Nobody has said | The name list above decides | — |
| `benign` | A human checked; the name list is wrong here | Escalation suppressed for this flag only | `x-apexe-escalates: false` |
| `escalates` | Sending it changes the operation's blast radius | Goes to the approval gate, but only when actually sent | `x-apexe-escalates: true` |
| `executes` | The value **is** a command the tool will run | **The call is refused**, not gated | `x-apexe-exec: true` |

`escalates` reaches the flags no name list generalizes — nothing about the
string `--privileged` suggests danger. `benign` goes the other way, because the
name list matches on spelling and spelling is sometimes a coincidence:
`bsdtar -y` is bzip2, `sort -f` folds case, `cut -f` selects fields, and none of
them mean "force".

`executes` is a different claim rather than a louder one. apexe's central
guarantee is that argv reaches `execve` with no shell on the path; a flag whose
value becomes a command hands that guarantee back, and no JSON Schema constrains
what the string becomes. So such a parameter is **refused with `ACL_DENIED`**
before anything spawns — there is nothing for a human to approve, because
approving would mean predicting what an arbitrary string will do. Known members
include `git --upload-pack` / `--receive-pack`, `tar --use-compress-program`,
`rsync -e` / `--rsync-path`, and `find -exec` / `-ok`.

See [`docs/overlays.md`](overlays.md) for how to verify a `risk` claim before
asserting it.

### Risk / `open_world`

Risk is derived from annotations plus an `open_world` signal — the executable itself (`curl`, `wget`, `ssh`, `scp`, `rsync`, …) or a networked subcommand of an otherwise local tool (`push`, `pull`, `fetch`, `clone`, `deploy`, `login`, …). Precedence when a command matches more than one: `destructive` > `open-world` > `readonly`. This is name-based, so it's a floor rather than a guarantee — an overlay's `annotation_overrides` is the way to assert the truth for a specific tool that doesn't fit the pattern.

---

## 9. Governance

### 9.1 Access Control (ACL)

`apexe scan` automatically generates `~/.apexe/acl.yaml` from the scanned modules
using a **default-deny** model. Matching corpus annotations override the built-in
name-based inference; a mismatched or absent overlay does not grant access on its
own. The corpus describes command capabilities, rather than caller permissions.
New ACLs use the following rules:

| Module type | Default rule |
|-------------|-------------|
| Readonly modules **that stay local** | `effect: allow` |
| Destructive modules | `effect: deny`, unconditional (no `conditions:` key) |
| Open-world modules (`git push`, `git fetch`, `curl`, …) | `effect: deny`, unconditional |
| All others | Default deny (no explicit rule) |

The three generated lists are **disjoint**. Destructive annotations take
precedence over readonly annotations, so contradictory labels never auto-allow
a destructive module. Separate hand-authored rules and the existing
`default_effect` are preserved when scanning again. Automatically managed target
lists are recalculated for the current scan; put persistent operator restrictions
in separate rules before broader allow rules.
`open_world` disqualifies a module from the auto-allow rule even when it is
`readonly`: `readonly` says the command does not modify local state and says
nothing about what it *sends*, and a read that reaches the network is the
exfiltration shape rather than the safe one. A module that is both destructive
and open-world is listed once, under destructive.

The open-world rule denies what `default_effect: deny` already denied by
silence, so it changes no decision on a stock policy. It earns its place twice
over regardless: it still denies under `default_effect: allow`, where silence
would have become blanket network access, and it gives network reach **one place
to be granted** — an operator who wants `git fetch` back edits this rule instead
of discovering by trial which module ids were denied by nothing but the absence
of a rule. It is deliberately not `allow` + `approval: required`: that is the
other defensible reading, but it loosens the shipped default, which is a choice
to make explicitly rather than inherit from a generated file.

ACL format (editable):

```yaml
default_effect: deny
rules:
  - callers: ["*"]
    targets: ["cli.git.status", "cli.git.log", "cli.git.diff"]
    effect: allow
    description: "Auto-allow readonly git commands"
  - callers: ["*"]
    targets: ["cli.git.clean", "cli.git.reset"]
    effect: deny
    description: "Block destructive git commands"
  - callers: ["*"]
    targets: ["cli.git.push", "cli.git.fetch", "cli.curl"]
    effect: deny
    description: "Block open-world CLI commands by default"
```

#### 9.1.1 Wrap a CLI and enforce its ACL

Use this sequence when introducing an existing CLI to an agent. It produces
bindings, lets you review the policy against the modules actually generated,
and then enables the reviewed policy on either transport:

```bash
# Scan commands on PATH and write bindings plus ~/.apexe/acl.yaml.
apexe scan git

# Show modules and the decision the ACL makes for the served anonymous caller.
apexe list --verbose --acl ~/.apexe/acl.yaml

# Review and edit the generated YAML before relying on it.
${EDITOR:-vi} ~/.apexe/acl.yaml

# Serve the same bindings through MCP or A2A with ACL enforcement enabled.
apexe serve --acl ~/.apexe/acl.yaml
# Or:
apexe a2a --acl ~/.apexe/acl.yaml
```

The generated policy is a starting point, not an authorization decision made
for your environment. By default it permits local readonly modules and denies
destructive, open-world, and unclassified modules. For example, a scan of Git
will normally permit `cli.git.status`, `cli.git.log`, and `cli.git.diff`, while
denying `cli.git.reset`, `cli.git.clean`, `cli.git.push`, and `cli.git.fetch`.
An operator can grant a narrowly selected module by adding it to an allow rule;
place narrow deny rules before broad allow rules because evaluation is
first-match-wins.

`--acl` is the switch that attaches the policy to the Executor. Omitting it
does not mean “use the generated default”; it means no ACL is enforced. The
path guard remains active either way, so a path-typed argument still cannot
write a protected system location or read a credential store.

> **Current served-identity limitation:** `apexe serve` and `apexe a2a` do not
> populate a distinct `Identity` or role set for each network caller. Rules
> that use `roles` or `identity_types` are useful when embedding the Executor
> with an explicit apcore `Context`, but do not currently provide per-user RBAC
> over apexe's built-in MCP or A2A commands. Use the generated module-level
> allow/deny rules, `--prefix`/`--tags`, transport authentication where
> available, and the path guard as the current served boundary.

#### 9.1.2 A wrapped tool is not a shell pipeline

Each generated module executes exactly one scanned binary with a direct argv
array. apexe does not parse shell source, spawn a shell, or connect one module's
stdout to another module's stdin. A `|` in an input value is just an argument
byte; it cannot create a pipeline. The child receives stdin from `/dev/null`.

This is intentional: accepting `ls | grep secret` as shell text would bypass
the binding contract and make ACL, argument validation, path checks, and audit
records ambiguous. For now, an agent must make separate tool calls and use the
first result in its own reasoning. A future pipeline feature must be a
declarative, no-shell graph whose every stage is a scanned module and receives
its own ACL, path-guard, timeout, output-limit, and audit decision; it must not
be emulated with `sh -c`.

> **A deny rule must be unconditional to deny.** apcore registers exactly five
> condition keys — `identity_types`, `roles`, `max_call_depth`, `$or`, `$not`.
> Any other key is treated as *unsatisfied*, so the rule never matches and the
> call falls through to the next rule or to `default_effect`. Earlier versions
> of this manual showed the destructive-deny rule carrying
> `conditions: {require_approval: true}`; copied verbatim under
> `default_effect: allow`, that rule denies nothing and the destructive command
> runs. apexe logs the reason when it happens:
>
> ```
> WARN apcore::acl: Unknown ACL condition 'require_approval' — treated as unsatisfied
> ```
>
> The `~/.apexe/acl.yaml` that `apexe scan` generates has always been correct;
> only the manual was wrong. There is **no ACL condition that means "ask a
> human first"** — approval is a separate layer (§9.6), not an ACL condition.

Rule ordering is **first match wins**, not most-specific-wins: an
`allow` rule for `cli.*` placed before a `deny` rule for `cli.rm` lets `cli.rm`
through. Put the narrow denials above the broad allows.

**A denial says why on both transports.** Over MCP the caller gets:

```
[ACLDenied] Access denied: caller 'None' cannot access module 'cli.cp'
```

Over A2A the task lands in `TASK_STATE_REJECTED` carrying its refusal class in
`status.message`, and the JSON-RPC error carries a governance-specific code:

```
-32040  Access denied: caller 'None' cannot access module 'cli.cp'
```

apcore-a2a 0.6 answers a governance refusal with its own code — `-32040`
access-denied, `-32041` approval-denied, `-32042` approval-timeout — instead of
the `-32001 Task not found` and `-32603 Internal server error` earlier versions
used. That distinction is what an agent should branch on: the code itself now
says *stop and pick a different skill*, where `Task not found` used to invite a
retry of the one thing that was fine.

Authenticated A2A returns the fixed per-class string (`Access denied`), naming
no caller, target or rule. `disclose_refusal_reason` is disabled to preserve the
boundary between principals. Explicit `--auth none` and an unauthenticated
library builder retain the detailed reason shown above. A2A JSON-RPC
notifications (requests with no `id`) produce HTTP 204 with an empty body;
explicit `id: null` requests still receive replies. A2A 1.0 SSE events use
`statusUpdate` / `artifactUpdate` wrappers, and endpoint versions live in
`supportedInterfaces`, rather than the obsolete 0.3 top-level fields.

The same applies to an approval denial and an approval timeout (§9.6); an
approval *pending* is untouched and still arrives as
`TASK_STATE_INPUT_REQUIRED` carrying its `approval_id`.

`audit.jsonl` remains the record of what was decided and why (§9.2), with
`decision: deny`, the matched rule index and the same `trace_id` the caller
saw — the response now agrees with it instead of pointing elsewhere.

### 9.2 Audit Trail

`~/.apexe/audit.jsonl` records both the calls that ran and the calls that were
refused. Discriminate on `event`; `trace_id` joins a record to the corresponding
`tracing` line and to the ACL entries below.

A call that reached the wrapped binary — `event: "execution"`:

```json
{
  "timestamp": "2026-08-20T01:24:04.535Z",
  "event": "execution",
  "trace_id": "a09dfb2ab24d4faab69b222c52aac5e4",
  "caller_id": "@external",
  "module_id": "cli.cp",
  "status": "success",
  "exit_code": 0,
  "duration_ms": 3
}
```

A call the governance stack stopped — `event: "refusal"`. `error_code` replaces
`exit_code`, since nothing ever ran to exit:

```json
{
  "timestamp": "2026-08-20T01:20:34.532Z",
  "event": "refusal",
  "trace_id": "71a7bbc6e5f44437a97441952755d9a5",
  "caller_id": "@external",
  "module_id": "cli.cp",
  "status": "refused",
  "error_code": "APPROVAL_DENIED",
  "duration_ms": 0
}
```

- **Refusals are recorded regardless of `--no-logging`.** That flag governs the
  `tracing` stream; turning the operational log down must not turn the audit
  trail off. A caller probing the argv guards produces one `refusal` row per
  attempt either way — the sequence an audit exists to capture.
- **`caller_id` is the authenticated principal**, or `@external` for an
  unauthenticated inbound request (apcore's canonical name for one). It is
  omitted entirely rather than guessed when the call names nobody at all.
  Resolved from `Context::identity` first and `Context::caller_id` second, and
  the order matters in both directions: `Context::caller_id` names the calling
  *module* in a nested chain, so preferring it would report the wrong thing for
  an authenticated call — but apcore 0.31 stopped synthesizing an `Identity`
  for a call that supplied none (decision D-103, correctly: a module written to
  check `if not context.identity` was being admitted by the manufactured
  principal), and marks such a call by setting `Context::caller_id` to
  `@external` instead. Reading only `identity` would therefore drop the field
  from every unauthenticated `execution` and `refusal` row while apcore's own
  `acl_decision` rows for the same call kept recording `@external` — two record
  kinds in one file disagreeing about one caller.
- **`duration_ms` is 0 for a refusal that short-circuited** ahead of the
  middleware phase — in practice the approval gate, since an ACL denial
  produces no apexe `refusal` row at all (see below). No clock had started.
- **No input values, hashed or otherwise.** Earlier versions wrote an
  `input_hash`; it was salted with random bytes that were then discarded, so
  nothing could ever be checked against it. A field that cannot be verified is
  not a privacy control, so it was dropped rather than kept for appearance.
- **Resilience**: audit logging never causes execution failures. Write errors
  are reported via tracing and the call proceeds.
- **Permissions**: the file is created `0600` — the mode is set through `OpenOptions::mode`, so it never exists world-readable even briefly, and an operator's own later `chmod` is left alone.

**A third shape appears in the same file.** ACL decisions are written by apcore
itself, not by apexe, and carry its richer entry — `decision`, `reason`,
`identity_type`, `roles`, `call_depth`, and an RFC 3339 timestamp with a
`+00:00` offset rather than `Z`:

```json
{
  "timestamp": "2026-07-30T01:24:35.300132+00:00",
  "caller_id": "@external",
  "target_id": "cli.ls",
  "decision": "deny",
  "reason": "default_effect",
  "identity_type": "external",
  "roles": [],
  "call_depth": 1,
  "trace_id": "36df7e553f354ffcb572e5b961ec4627"
}
```

apexe deliberately does **not** also emit a `refusal` row for an ACL denial: it
would double-count the same event with strictly less detail. A consumer counting
refusals should count `event == "refusal"` plus `decision == "deny"`. A call
that reached the wrapped binary and then failed — a timeout, a spawn failure,
an output overflow — is **not** a refusal: it appears once, as
`event: "execution"` with `status: "error"` and `exit_code: -1`.

> **Breaking change (unreleased).** The `user` and `input_hash` fields are gone,
> and `event`, `trace_id` and `caller_id` are new. `user` came from `getlogin()`,
> which returns the owner of the controlling terminal — under a service manager
> that is the terminal's owner or `root`, not whoever made the call, so it
> attributed every request to the wrong principal. Consumers keying on either
> removed field need updating.

### 9.3 Subprocess Isolation (always-on)

Every `CliModule` call runs through `execute_subprocess` (`src/module/executor.rs`), which applies isolation **unconditionally** — there is no `--sandbox` toggle and no "unsandboxed" mode. (This is not `apcore-cli`'s `Sandbox`, which expects the host binary to re-exec itself with an `--internal-sandbox-runner` subcommand and rediscover modules from `APCORE_EXTENSIONS_ROOT` — a model apexe's runtime-scanned CLI modules don't fit.)

- **Environment scrubbing**: the subprocess does **not** inherit apexe's full environment. The env is cleared and only a base allowlist is passed through (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TZ`, `TMPDIR`), so secrets in apexe's environment (API tokens, cloud credentials) can't leak to — or be surfaced by — a wrapped tool. File-based credentials under `$HOME` still work. (Per-tool credential-env passthrough is planned as an opt-in config knob.)
- **No shell**: arguments are passed as direct argv and handed to `execve` — no shell is spawned anywhere on the execution path, so shell metacharacters are inert data and pass through unchanged. (`curl --data '{"a":1}'` and every `jq` filter depend on that.) A caller-supplied value is rejected only for NUL and the five line terminators, which would corrupt the audit trail's framing, and for a leading `-`, which the wrapped tool would parse as an option the caller was not granted — see `CONTROL_CHARS` and `validate_argument_value` in `src/module/executor.rs`. The shell-metacharacter blacklist that remains (`BINDING_INJECTION_CHARS`) applies only to `json_flag` and the command path read out of a *binding file*, which is a build-time artifact rather than caller input.
- **Output cap**: stdout/stderr are each capped at 64 MiB (`executor::DEFAULT_MAX_OUTPUT_BYTES`). A command that exceeds it gets `stdout_truncated`/`stderr_truncated: true` in the result instead of exhausting memory.
- **Timeout kills the process**: the child is spawned with `kill_on_drop(true)`; when `--timeout`/`default_timeout` elapses, the subprocess is actually terminated rather than left running as an orphan.
- **stdin** is connected to `/dev/null`, so a tool that waits for input fails fast instead of hanging.

Stronger OS-level sandboxing (seccomp/landlock, namespaces, cgroup limits) is tracked as roadmap; v0.x relies on the governance stack (ACL + approval + preview) plus the process isolation above.

### 9.4 Preview (Dry-Run Prediction)

Destructive modules (`annotations.destructive == true`) implement apcore's `Module::preview()` hook. It does not attempt to predict the actual side effects of an arbitrary CLI binary (`apexe` has no way to know what `git push --force` will do to a remote) — it only surfaces the exact resolved command line, so an approver can see what they're about to allow. Readonly/non-destructive modules return no preview (nothing to predict). Reachable via apcore-mcp's `__apcore_module_preview` meta-tool.

### 9.5 Resilience Middleware

`build_executor` wires two middleware on by default (`--no-circuit-breaker`/`--no-retry` to disable, on both `apexe serve` and `apexe a2a`):

- **`HealthOnlyCircuitBreaker`** — short-circuits calls to a `(module_id, caller)` pair after repeated failures (default: ≥5 samples, ≥50% error rate), instead of letting every caller keep hammering a hanging or broken CLI tool. Auto-recovers after a cooldown window via a single probe call. Only outcomes that say the *wrapped binary* is unhealthy feed the window — spawn failure, timeout, signal death, internal error. Input-validation rejections, conflict refusals and governance decisions (ACL denial, approval denied/timeout/pending, cancellation, depth and frequency limits) do **not** count: the module was reachable and enforced its contract exactly as designed, and schema trial-and-error is the normal behaviour of an LLM caller. Counting those meant five malformed calls disabled a tool for every caller while a binary that failed every time kept its circuit closed.
- **`RetryMiddleware`** — retries a call after a transient failure, but *only* when the error is explicitly marked `retryable`. `CliModule` marks a timeout `retryable` **only when the module is annotated `idempotent`** (§8) — a killed non-idempotent command (e.g. `rm -rf` timing out mid-delete) is never auto-retried, since it may have partially applied its side effect.

### 9.6 Approval: `--enable-approval` prompts the connected client

`--enable-approval` gates every call to a module annotated `requires_approval`
on a human decision, delivered to the connected MCP client as an
`elicitation/create` request. Accept and the call runs; decline or cancel and it
is refused.

**It only works with a client that declared elicitation support** in its
`initialize` handshake. A client that did not cannot be prompted, so its gated
calls are refused — fail-closed, with a reason that says the prompt could not be
delivered and names what to use instead. Check your client before turning the
flag on: against one without elicitation, this is still an unconditional deny
gate over every `requires_approval` module.

This needs apcore-mcp **0.18 or later**. Earlier versions could not reach the
prompt at all: the `ElicitCallback` lives inside apcore-mcp's router, and an
`ApprovalHandler` built outside it — which is all a CLI entry point can build —
received only the JSON `Context`, whose `data` map holds the string
`"available"` rather than the callback, because no closure is a
`serde_json::Value`. 0.18 registers the callback per tool call and puts its
*id* in the context instead, which is a `Value`, so the handler can exchange it
for the live callback.

apexe wraps apcore-mcp's `ElicitationApprovalHandler` in its own `ApprovalGate`
for two things upstream cannot do from where it sits: every refusal is written
to the audit trail (§9.2) — the approval gate runs ahead of the middleware
phase, so nothing else in the stack observes it — and a refusal for want of a
prompt is reported with a remedy rather than as `Elicitation returned no
response`. A human's own "no" is passed through verbatim.

For a per-caller boundary that needs no human at all, use `--acl` (§9.2).

For approval flows where the approver isn't in the session (e.g. a Slack bot),
`apexe`'s `ExecutorOptions`/`McpServerBuilder`/`A2aServerBuilder` accept an
`Arc<dyn apcore_mcp::ApprovalStore>` — when set, approvals become non-blocking
(`StorageBackedApprovalHandler`): the call returns an `ApprovalPending` error
immediately, and a separate mechanism you build resolves the decision later via
`ApprovalStore::resolve`.

> **The gate weighs the arguments a call actually sent.** A module marked
> `requires_approval` only because it *accepts* an escalating flag (§8) records
> which schema properties those are, and the gate stands down for a call that
> carries none of them — `git log` runs unprompted, `git log --all` prompts.
> This is the one decision apcore cannot make: neither `ApprovalHandler` nor its
> `ExecutionPolicy` hook is handed a call's arguments, and `ApprovalRequest`
> is the first place they are visible. A stand-down is recorded with
> `approved_by: "apexe-approval-gate"`, so an audit reader can tell it from a
> human's yes. A module marked `destructive` **by name** carries no such basis
> and always prompts.
>
> **Known limitation.** The escalating-flag list is still name-based, so a
> dangerous flag under an unremarkable name is not on it — that is what an
> overlay's `risk: escalates` is for (§8). And `cli.ssh` and `cli.curl` are
> gated by nothing here: they are `open-world`, not `destructive`, and accept no
> flag on the list.

There is no CLI flag for this — `apcore-mcp`'s `InMemoryApprovalStore` is documented as unsuitable for production (state isn't shared across process invocations, so a hypothetical `apexe approval resolve` command couldn't reach a running server's store anyway). Embed apexe as a library and supply your own persistent store (Redis, a database, etc.) to use this for real.

---

### 9.7 Path Guard (always-on)

The ACL (§9.1) decides *whether a module may be called*. It cannot decide *what
a call may touch* — apcore ACL rules match on caller and target module id, not
on argument values. `rm` is not a command to forbid outright; `rm /etc` is.

The path guard covers that gap. Every argument the module's input schema types
as a filesystem path is resolved and checked before the subprocess is built.
It is **on by default on every surface**, needs no flag, and has no off switch.

**Two lists, because reading and writing are different risks.** Which one binds
a call comes from the module's `readonly` annotation (§8):

| | System paths | Credential paths |
|---|---|---|
| **`readonly` module** — `cat` `ls` `grep` `find` | allowed | **refused** |
| **Everything else** — `rm` `mv` `cp` `chmod` | **refused** | **refused** |

| List | Locations | Compiled in |
|------|-----------|-------------|
| System | `/bin` `/boot` `/dev` `/etc` `/lib` `/lib32` `/lib64` `/proc` `/run` `/sbin` `/sys` `/usr` `/var` — plus `/System` `/Library` `/Applications` `/private/etc` `/private/var` on macOS | yes, cannot be removed |
| Credential | `~/.ssh` `~/.aws` `~/.gnupg` `~/.kube` `~/.docker` `~/.apexe` `~/.config/gh` `~/.config/gcloud` `~/.config/apcore` `~/.git-credentials` `~/.netrc` | yes, cannot be removed |

`cat /etc/hosts` and `ls /usr/bin` are ordinary work, so a `readonly` module may
name a system path — refusing it protected nothing, since the file is
world-readable and an agent's own file tools reach it regardless. Credential
paths are refused to readers as well: deleting a private key announces itself
the next time the key is used, while copying one into a model context leaves no
trace, so the stricter treatment goes to the risk that is harder to notice.
`~/.apexe` is on that list because it holds the ACL and audit trail governing
the call being made.

A module with no `readonly` annotation is treated as a writer. Note that §9.1's
caveat carries over: a command misclassified as readonly gets the weaker
treatment here too — fix it with an overlay.

`~/.config` is deliberately **not** guarded. It holds ordinary application
settings a wrapped tool has legitimate reason to touch.

`/` is not an entry either — every absolute path starts with it, so listing it
would refuse the whole filesystem. `rm /` is still refused, because for a
writer a target that *contains* a protected location is refused too.

**Extend it in `config.yaml`:**

```yaml
additional_denied_paths:
  - /srv/production-data
  - ~/customer-exports
```

Configured entries join the **credential** list, so they bind readers as well
as writers: naming a path explicitly asserts that it is sensitive.

**Reopen a subtree with `allowed_paths`:**

```yaml
allowed_paths:
  - /etc/nginx/conf.d
```

This is the **only** setting that relaxes the guard, and it is **empty by
default** — the feature is inert until you write it down. It exists because the
alternative is worse: an agent that legitimately has to write
`/etc/nginx/conf.d` would otherwise have to be handed that tooling outside
apexe entirely, which drops the audit trail and the ACL along with the path
check.

**You own what you open here.** Nothing validates that an entry is wise —
naming `/etc`, or a credential directory, is honoured. What the guard does
instead is make the decision visible: every carve-out is logged at startup, and
one that opens a whole system location or exposes credentials is logged at
`warn`.

Two things worth knowing before you use it:

- **Prefer the narrowest subtree that does the job.** `/etc/nginx/conf.d`, not
  `/etc`. A carve-out grants everything beneath it.
- **A config directory grants the service's behaviour.** Write access to
  `/etc/nginx/conf.d` is enough to add a `proxy_pass` and redirect traffic
  anywhere, without touching anything else under `/etc`. That is the real scope
  of the permission, not "one directory".

Carve-outs and denials share one specificity ladder, so you can open a subtree
and still fence off part of it:

```yaml
allowed_paths:
  - /etc/nginx/conf.d
additional_denied_paths:
  - /etc/nginx/conf.d/secrets   # more specific, so this wins
```

`~/.config` needs no entry here — it is not guarded in the first place.

**What gets compared.** Not the string the caller sent — the path the kernel
would act on. A relative path is joined to the working directory the subprocess
actually runs in, symlinks are followed, and `..` is folded afterwards. All
three matter:

```jsonc
// cli.rm (writer)
{ "file": ["/etc/passwd"] }          // Refused: resolves into /etc
{ "file": ["../../../etc/passwd"] }  // Refused: climbs out of the workspace
{ "file": ["/tmp/x/hosts"] }         // Refused: /tmp/x is a symlink to /etc
{ "file": ["/"] }                    // Refused: contains a protected location
{ "file": ["/etcetera/notes"] }      // Allowed: /etcetera is not /etc

// cli.cat (readonly)
{ "file": ["/etc/hosts"] }           // Allowed: system paths are legible
{ "file": ["~/.ssh/id_rsa"] }        // Refused: credentials bind readers too
{ "file": ["/"] }                    // Allowed: ancestry binds writers only
```

A refusal is `ACL_DENIED` and names all three of the requested path, the
resolved path and the protected location that matched:

```json
{
  "code": "ACL_DENIED",
  "message": "Element 0 of parameter 'file' resolves to '/private/etc/passwd', which is protected by '/private/etc'",
  "details": {
    "requested_path": "../../../etc/passwd",
    "resolved_path": "/private/etc/passwd",
    "protected_path": "/private/etc"
  }
}
```

The requested path alone is often not enough to see the problem — that is the
point of resolving it — so both are reported.

**The temp directory stays writable.** On macOS `$TMPDIR` lives under
`/var/folders/`, which the `/var` baseline would otherwise refuse. A compiled-in
carve-out keeps it usable. The carve-out list is not configurable, and a
`TMPDIR` pointing at a system location is discarded rather than honoured. A
credential path nested inside the temp directory — a sandboxed `HOME` — is
still refused, by the more-specific-rule-wins tiebreak rather than by voiding
the carve-out.

**Limits.** The guard acts on values the schema *types* as paths. A tool whose
help text never reveals that an option takes a filename yields an unmarked
value the guard cannot see; nothing bounds what the tool does after it starts
(`find . -delete` is past the boundary); because ancestry binds writers only, a
recursive *read* rooted above a credential directory — `grep -r … /` — is not
caught; and the mode is per module rather than per argument, so
`cp /etc/hosts ~/backup` is refused even though the system path is only being
read. See [`docs/threat-model.md`](threat-model.md) §4.8 and §5.8 for
the full accounting.

---

## 10. MCP Server

### Transport Options

| Transport | Use case | Command |
|-----------|----------|---------|
| **stdio** | Claude Desktop, Cursor (default) | `apexe serve` |
| **streamable-http** | Remote agents, browser UI | `apexe serve --transport http --port 8000` |
| **sse** | ⚠️ Deprecated upstream, but served | `apexe serve --transport sse --port 8000` |

> **`--transport sse` is deprecated, but it works again and no longer needs an acknowledgement.**
> apexe used to refuse it: apcore-mcp shared one process-global channel across
> every connection, so responses went round-robin to whichever stream was next
> and, with two clients connected, **one client received the other's tool
> output**. apcore-mcp 0.18 scopes a session per connection and emits the
> `event: endpoint` a spec-compliant MCP SSE client waits for, so the defect is
> gone and the refusal went with it. apexe requires `apcore-mcp = "0.18"`, so a
> build cannot quietly resolve back to the affected 0.17.
>
> SSE is still **deprecated upstream** and apexe warns at startup. Prefer
> `--transport http` (streamable HTTP) for anything new.
>
> `--allow-deprecated-sse` is still accepted so existing invocations keep
> parsing, but it decides nothing and is hidden from `--help`. It will be
> removed in a later release.

### Transport Authentication

The three transports have different trust boundaries, so they get different
defaults:

| Transport / bind | Default | Why |
|---|---|---|
| **stdio** | no auth | The boundary is the parent/child process relationship — whatever can spawn apexe already holds your privileges. A token adds nothing and would break every Claude Desktop / Cursor config. |
| **HTTP/SSE on `127.0.0.1`** | bearer token, **generated and written to stderr at startup** | Any local process can reach the port, including a page in your browser. You should not have to manage a secret for a local dev server. |
| **HTTP/SSE on any other host** | authentication required | apexe wraps arbitrary local binaries. An unauthenticated non-loopback bind is a remote-execution entry point for every executable on the host. |

> **apexe terminates no TLS.** On a non-loopback bind the bearer token or JWT
> travels in cleartext, and anything on the path can replay it to reach the same
> remote-execution surface the credential exists to close. apexe warns at
> startup — both through `tracing` and in the token notice on stderr, since the
> two can be filtered differently — but it cannot refuse: apexe behind a
> TLS-terminating reverse proxy is the correct deployment and looks identical
> from inside the process. Put one in front, or bind to `127.0.0.1` and reach it
> through an SSH tunnel.

```bash
apexe serve --transport http                              # token generated + printed
apexe serve --transport http --auth-token "$MY_TOKEN"     # or APEXE_AUTH_TOKEN
apexe serve --transport http --auth jwt --jwt-secret "$S" # or APEXE_JWT_SECRET
apexe serve --transport http --auth none                  # loopback only
apexe serve --transport http --host 0.0.0.0 --auth none \
            --allow-unauthenticated-bind                  # states that you mean it
```

A generated token is written **directly to stderr**, not through the log
pipeline. Two consequences worth relying on: it appears at every `--log-level`,
including `warn` and `error`, so the server can never demand a credential you
have no way to read back (`--show-config` omits credentials by construction);
and it is never written into a log file, journald, or a log aggregator you have
pointed `tracing` at. The log itself only records *that* token auth is on.

Clients send `Authorization: Bearer <token>`. The Explorer UI's `Authorization`
field is wired to this — browsing (GET) is open, execution (POST, including
`POST /explorer/tools/{tool}/call`) requires the credential.

`/health` is exempt so container and load-balancer probes work. `/metrics` is
**not** exempt: its `module_id` labels and per-module call volumes are
reconnaissance about what this host wraps and what actually gets used.

`--auth none` on a non-loopback bind refuses to start without the separate
`--allow-unauthenticated-bind` acknowledgement — a `--disable-*` flag gets
copied out of a tutorial once and then lives in everyone's startup script
forever.

### Built-in Middleware

| Middleware | Status | Effect |
|-----------|--------|--------|
| **LoggingMiddleware** | Enabled by default | Structured logging of inputs/outputs, redacting properties the scanner marked `x-sensitive` — see below |
| **FailureLogMiddleware** | Automatic with `--no-log-arguments`, and whenever an audit log is configured | One payload-free `ERROR` record per failed call, plus the `refusal` rows in `audit.jsonl` — see below |
| **CircuitBreakerMiddleware** | Enabled by default (`--no-circuit-breaker`) | Short-circuits a hanging/broken tool — see §9.5 |
| **RetryMiddleware** | Enabled by default (`--no-retry`) | Retries idempotent timeouts only — see §9.5 |
| **ApprovalGate** | Opt-in (`--enable-approval`) | Prompts the connected MCP client for a human decision on a `requires_approval` module; refuses when the client cannot be prompted — see §9.6 |

> **Credentials in tool arguments.** The logging middleware records each call's
> `inputs` and `output` at INFO. Redaction is schema-driven: the scanner marks
> credential-bearing options (`curl --user`, `--oauth2-bearer`, `--header`, key
> and certificate paths, and their equivalents) with `x-sensitive: true`, and
> those values are replaced before anything is written.
>
> **The marker lives in the binding file, so a binding scanned before the
> release that introduced it carries none and redacts nothing.** Redaction is not retroactive: it is
> the scanner that writes `x-sensitive`, and upgrading apexe does not rewrite
> bindings already on disk. Check with
>
> ```bash
> grep -L x-sensitive ~/.apexe/bindings/*.yaml
> ```
>
> — every file listed still logs credentials verbatim. Re-scan those tools
> (`apexe scan <tool> --no-cache`) to pick the marker up, or run with
> `--no-log-arguments` until you have.
>
> A heuristic cannot be exhaustive over every wrapped tool's option set — a request body (`curl
> --data`) and a key sitting in a URL's query string announce themselves in no
> schema — so if you pass secrets through options apexe may not recognize, run
> with `--no-log-arguments`.
>
> `--no-log-arguments` drops the payload from **every** log event, the error
> record included. That matters because apcore's error record renders the same
> partially-redacted argument object as the `START` line, so a call rejected by
> schema validation used to print the body a successful call would have hidden.
> The operational record survives in a different shape: apexe installs its own
> `FailureLogMiddleware`, which emits one `ERROR` per failed call carrying
> `module_id`, `trace_id`, `caller_id`, `error_code` and `duration_ms` — and
> nothing the caller sent, not even the error message, since a validation
> message quotes the value it rejected. So a refusal is still visible, and
> `error_code` is what an alert keys on.
>
> `--no-logging` remains available to drop logging altogether, failure records
> included. The `audit.jsonl` trail records no raw argument values in any case.

### Observability

`apexe serve --transport http --metrics` enables two endpoints (HTTP/SSE only, ignored on stdio):

| Endpoint | Format | Content |
|----------|--------|---------|
| `/metrics` | Prometheus text | `apcore_module_calls_total`, `apcore_module_duration_seconds` histogram, per `module_id` |
| `/usage` | JSON | Per-module call count, error count, average latency, unique callers, trend |

### Tool Filtering

`--tags` and `--prefix` (and their `McpServerBuilder` equivalents) restrict
which scanned modules the server exposes. The filter is applied at
**registration** time, so an excluded module is not merely absent from
`tools/list` — it does not exist on this server at all, and `tools/call`,
`resources/list` and `resources/read` all return `ModuleNotFound` for it.

```bash
apexe serve --prefix cli.git            # a git-only server
apexe serve --tags readonly             # every listed tag must match (AND)
```

```rust
McpServerBuilder::new()
    .tags(vec!["readonly".to_string()])     // only expose readonly tools
    .prefix("cli.git")                       // only expose git tools
    .build()?;
```

This is coarse-grained: it selects a subset of the tool surface for everyone.
For per-caller rules, use `--acl` (§9.2).

> **A stray comma is refused, not applied.** `--tags readonly,` splits to
> `["readonly", ""]`, and every listed tag must match — no module carries an
> empty tag, so the filter would admit nothing and the server would start with
> an empty registry and no callable tools at all. `apexe serve` exits with an
> error naming the empty tag instead. A tag that is merely unknown to *this*
> host is not an error (one invocation is meant to be portable across
> differently scanned machines), but if the filter excludes every loaded
> module apexe logs a **warning** saying so, rather than the `admitted=0` info
> line that looked identical to an empty bindings directory.

### Availability Filtering

Registration also drops any module whose binary is not actually reachable on
this machine — checked against the `target` (`exec://{binary_path} ...`)
recorded in its binding file at scan time. A binding file can outlive the tool
it wraps: uninstalled since, moved, or the `bindings_dir` copied to a different
host. Unlike `--tags`/`--prefix`, this check is **unconditional** and has no
opt-out flag — `apexe serve` and `apexe a2a` share the same `build_executor`,
so it applies to both, and it is not optional because an MCP/A2A client has no
graceful way to recover from a tool it was told exists failing every call with
`ModuleNotFound`/"not installed".

An excluded module logs a warning naming the `module_id` and the target that
could not be resolved:

```
WARN apexe::module::registry: Binary not reachable on this machine; excluding
from the registered tool surface module_id="cli.ghost" target="exec:///..."
```

`apexe list` shows binding files as-is by default (useful for reviewing what
was ever scanned, e.g. before moving `bindings_dir` to a new host); pass
`--available-only` to apply the same check there:

```bash
apexe list --available-only              # only binaries reachable right now
```

### Explorer UI

Enable with `--explorer` (HTTP transport only):

```bash
apexe serve --transport http --port 8000 --explorer
```

Provides a browser-based interface to explore available tools, view schemas, and test invocations.

> **The Try-It editor prefills only what a call needs.** It emits exactly the
> keys in the tool's `required` list — each with its declared `default` when the
> schema has one, and `null` otherwise — and omits every optional property. For
> `cli.curl`, whose contract has 257 properties, that is `{"url": null}` rather
> than 259 lines of blanks.
>
> A `null` placeholder is deliberately **not** valid against a typed property,
> so `Validate` refuses an untouched prefill and names the field you still have
> to fill. That is the point: an earlier version filled every string with `""`
> and every number with `0`, which satisfied both `required` and the declared
> types, so `Validate` certified an empty call as correct and `Execute` sent it
> — the wrapped tool was the first thing to object. Needs apcore-mcp 0.18.1 or
> later (`mcp-embedded-ui` 0.5).

### OpenAI Tools Export

Export tool definitions in OpenAI function calling format (programmatic API):

```rust
let tools = McpServerBuilder::new()
    .bindings_dir("~/.apexe/bindings")
    .export_openai_tools()?;
```

---

## 11. A2A Server

`apexe a2a` exposes the same scanned modules as an A2A agent via
[apcore-a2a](https://github.com/aiperceivable/apcore-a2a-rust), instead of
(or alongside) MCP. It shares governance with `apexe serve` — both build
their `Executor` through the same `apexe::module::build_executor()`, so an
`--acl` policy, the logging middleware, the audit trail, and the always-on
subprocess isolation behave identically regardless of which transport a caller
uses. **Approval** is the one exception: `apexe serve` offers
`--enable-approval`, which prompts the connected MCP client for a human
decision (§9.6), while `apexe a2a` has no such flag at all — A2A has no
elicitation transport, so a prompt could never be delivered over it — approval on A2A is
library-only (supply an `ApprovalStore` to `A2aServerBuilder`).
Transport authentication (§10) is likewise `apexe serve` only.

```bash
apexe a2a                                       # http://127.0.0.1:8000
apexe a2a --url http://127.0.0.1:9000 --explorer  # custom port + browser UI
# A public unauthenticated bind requires both explicit opt-outs:
apexe a2a --url http://0.0.0.0:9000 --auth none --allow-unauthenticated-bind
apexe a2a --acl ~/.apexe/acl.yaml               # governed by an ACL policy
```

### Calling a skill: send a DataPart, not prose

Every apexe skill takes a JSON object — `build_input_schema` always produces
`"type": "object"` — so its arguments ride in a **DataPart**, and the skill's
`skillId` goes in the message metadata:

```bash
curl -X POST http://127.0.0.1:8000/ -H 'content-type: application/json' -d '{
  "jsonrpc": "2.0", "id": 1, "method": "message/send",
  "params": { "message": {
    "role": "user",
    "messageId": "m1",
    "metadata": { "skillId": "cli.cp" },
    "parts": [ { "kind": "data", "data": { "source_file": ["/tmp/a"], "target": "/tmp/b" } } ]
  } }
}'
```

A **TextPart carrying prose does not work**, and the error says why in terms of
the mechanism rather than the remedy:

```
TextPart text is not valid JSON: expected value at line 1 column 1
```

For an object-typed skill, apcore-a2a parses a TextPart's `text` *as* JSON — so
`{"kind": "text", "text": "{\"source_file\": [...]}"}` is accepted and
`{"kind": "text", "text": "copy this file"}` is not. That rule is shared with
apcore-a2a's Python and TypeScript siblings; a DataPart is the direct way to
say the same thing.

> **The agent card's per-skill `inputModes` is the field to read.** Each skill
> advertises `["application/json"]`, computed from its own schema. The card also
> carries an agent-level `defaultInputModes` of `["text/plain",
> "application/json"]` — that is apcore-a2a's hardcoded default and describes
> what the *framework* can accept, not this agent: apexe never produces a
> string-rooted skill, so `text/plain` is unreachable here. Per the A2A spec a
> skill's own `inputModes` overrides the default, so a client that reads it gets
> the right answer; one that reads only the agent default will try prose and hit
> the error above. apexe cannot narrow the default — `APCoreA2AConfig` exposes
> no field for it.

### Endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /.well-known/agent-card.json` | A2A Agent Card (skills derived from registered modules) |
| `GET /health` | Liveness check |
| `GET /explorer` | Browser-based Explorer UI (with `--explorer`) |

### Skill aliasing

Like MCP, A2A reads the `display` overlay `apexe scan` computes
(`metadata["display"]["a2a"]["alias"]`) to name each skill; see
[Display Names](#display-names) below.

### Authenticated caller identities

Token authentication establishes the shared `apexe-token` identity. JWT maps
`sub` and `roles` into the executor context and audit records. ACL `callers`
match module call-chain IDs, not JWT subjects: inbound transport requests use
`@external`. Gate authenticated roles through conditions, for example:

```yaml
version: '1.0'
default_effect: deny
rules:
  - callers: ['@external']
    targets: ['cli.echo']
    effect: allow
    conditions: {roles: [ops]}
```

An expired or invalid credential is rejected before module execution.
Authenticated A2A returns masked governance messages; an explicit
unauthenticated surface may disclose detailed refusal reasons.

---

## 12. Integrating with AI Agents

### `--show-config` reproduces the invocation you type

`--show-config` renders the *rest of the command line* into the snippet, so add
every flag you intend to serve with:

```bash
apexe serve --show-config claude-desktop \
  --bindings-dir /srv/apexe/modules --prefix cli.git --acl /etc/apexe/acl.yaml
```

```json
{
  "mcpServers": {
    "apexe": {
      "command": "apexe",
      "args": ["serve", "--transport", "stdio",
               "--bindings-dir", "/srv/apexe/modules",
               "--prefix", "cli.git",
               "--acl", "/etc/apexe/acl.yaml"]
    }
  }
}
```

`--bindings-dir`, `--tags`, `--prefix`, `--acl`, `--name`, `--enable-approval`,
`--no-logging`, `--no-log-arguments`, `--no-circuit-breaker` and `--no-retry`
are all carried through. `--bindings-dir` matters most: a client launching
`apexe serve` without it reads the default `~/.apexe/bindings`, so anyone who
scanned elsewhere would get a server with **no tools at all**.

**Paths are made absolute.** A relative `--bindings-dir ./modules` is written
into the snippet as `/abs/path/to/cwd/modules`, resolved against the directory
you ran `--show-config` in. The client that later runs the snippet launches
`apexe` from its own working directory, so a relative path would resolve
somewhere else entirely — the server would log `Bindings directory not found,
starting with zero tools` and exit 0, and a relative `--acl` would silently
apply no policy at all.

> **Credentials are never written into a snippet.** `--auth-token` and
> `--jwt-secret` are excluded by construction, because a config file is shared
> and often committed. For an HTTP server behind `--auth token`, the snippet
> carries only the URL — configure the `Authorization: Bearer` header in the
> client, from your own secret store.

An unrecognised target (`--show-config vscode`) is rejected on **stderr** with
a non-zero exit, so `apexe serve --show-config … > mcp.json` cannot write an
error message into the config file.

### Claude Desktop

```bash
apexe scan git curl grep
apexe serve --show-config claude-desktop
```

Copy the JSON output into:
- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Linux**: `~/.config/claude/claude_desktop_config.json`

Restart Claude Desktop. Scanned tools appear as MCP tools.

### Cursor

```bash
apexe serve --show-config cursor
```

Add the JSON to Cursor's MCP settings. `cursor` honours `--transport`: with
`--transport http` it emits the `url` form rather than a command block.

### HTTP Mode (Remote Agents)

```bash
apexe serve --transport http --host 0.0.0.0 --port 8000
apexe serve --show-config claude-desktop --transport http --port 8000
```

The MCP endpoint is at `POST /mcp` (the deprecated SSE transport is served at
`GET /sse` instead, and the generated snippet uses whichever path matches
`--transport`).

### Display Names

apexe generates display metadata for MCP clients:

| Module ID | MCP Display Alias |
|-----------|------------------|
| `cli.git.commit` | `git_commit` |
| `cli.docker.container.ls` | `docker_container_ls` |
| `cli.curl` | `curl` |

Aliases are auto-sanitized for MCP compatibility (dots replaced with underscores, digit prefixes escaped).

---

## 13. Error Handling and AI Guidance

Every error includes `ai_guidance` to help AI agents self-correct:

| Error | ai_guidance |
|-------|-------------|
| Tool not found | "The tool 'xyz' is not installed. Install it and try again." |
| Command timeout | "The command took too long. Try with simpler arguments or increase timeout." |
| Option injection (value starts with `-`) | The value would be read as an option the caller was never granted. Send it without the leading dash, or use a tool whose overlay declares `--` support (`x-apexe-end-of-options`) |
| Path refused by the guard | "'/etc/passwd' resolves to '/private/etc/passwd', inside or above the protected location '/private/etc'. … Choose a path outside the protected system and credential directories." |
| Parameter refused as a command executor | "Parameter 'upload_pack' is refused: its value is a command the wrapped tool would run … send the call without it, or run the tool outside apexe." |
| Permission denied | "Permission denied. Check file permissions or run with appropriate privileges." |
| Non-zero exit code | "Command 'git push' exited with code 1. stderr: (first 200 chars)" |

> **There is no "shell injection" error on the execution path.** Earlier versions
> listed one. No shell is spawned anywhere (§9.3), so `;`, `|`, `&`, `$` and
> quotes are inert bytes in an argument and pass through unchanged — `curl
> --data '{"a":1}'` and every `jq` filter depend on it. The guards that do fire
> are the three rows above plus NUL and the five line terminators, which are
> rejected because they would corrupt the audit trail's framing. An agent that
> escapes metacharacters in response to a refusal is solving the wrong problem.
> `ApexeError::CommandInjection` still exists, but it is a tamper signal for a
> hand-edited `.binding.yaml` (`BINDING_INJECTION_CHARS`), never a response to
> caller input.

Additionally, each execution response includes:
- `trace_id` for end-to-end correlation
- `duration_ms` for performance tracking
- `exit_code` for programmatic error detection

### A non-zero exit is **not** an MCP error — key on `exit_code`

A wrapped command that runs and exits non-zero comes back as an ordinary,
successful tool result: **`isError: false`**, with `exit_code`, `stdout`,
`stderr` and `ai_guidance` in the payload.

`exit_code` is **not** a sibling of `isError`. MCP's `tools/call` result has a
fixed shape — `content` plus `isError` — and apcore-mcp puts the whole payload
into `content[0].text` as a **JSON string**:

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"exit_code\":2,\"stdout\":\"\",\"stderr\":\"curl: option --max-time=1: is unknown\",\"ai_guidance\":\"Command 'cli.curl' exited with code 2. stderr: ...\"}"
    }
  ],
  "isError": false
}
```

So a client reads `exit_code` by JSON-parsing `content[0].text` first:

```js
const payload = JSON.parse(result.content[0].text);
if (payload.exit_code !== 0) { /* the command ran and answered */ }
```

Reading `result.exit_code` directly yields `undefined`, and a client that then
falls back to `isError` lands exactly in the trap the rest of this section
exists to prevent.

This is deliberate and will not change. In a CLI bridge, a non-zero exit is
frequently the *answer*, not a failure:

| Command | Exit 1 means |
|---------|--------------|
| `grep pattern file` | no match found |
| `diff a b` | the files differ |
| `test -f path` | the predicate is false |

Setting `isError: true` on a non-zero exit would report all three as failed
tool calls, and an agent would retry or abandon a tool that answered its
question correctly.

`isError: true` is reserved for **the call never reaching the binary**:

- schema validation rejected the arguments,
- the ACL denied the caller,
- `--enable-approval` blocked a `requires_approval` module,
- the circuit breaker was open, the timeout killed the process, or the binary
  could not be spawned at all.

On that path `content[0].text` is a plain diagnostic sentence rather than a
JSON payload, so parsing it is only meaningful when `isError` is `false`.

**So: an MCP client must not treat `isError: false` as "the command
succeeded".** Parse `content[0].text` and read `exit_code` from it (it is
`required` in every generated output schema, alongside `stdout` and `stderr`),
then apply the wrapped tool's own convention.

The A2A surface follows the same split: a non-zero exit yields
`TASK_STATE_COMPLETED` with the `exit_code` inside the artifact, while a
governance or validation refusal yields `TASK_STATE_FAILED` (or
`TASK_STATE_INPUT_REQUIRED` when an approval is pending, and
`TASK_STATE_CANCELED` on cancellation). There too, the task state answers "did
the command run", not "did it succeed".

---

## 14. File Locations

| Path | Purpose | Created by |
|------|---------|------------|
| `~/.apexe/config.yaml` | Configuration | `apexe config --init` or `config set` |
| `~/.apexe/bindings/*.binding.yaml` | Tool binding files | `apexe init` or `scan` |
| `~/.apexe/cache/` | Scan result cache | `apexe init` or `scan` |
| `~/.apexe/acl.yaml` | Access control rules | `apexe init` or `scan` |
| `~/.apexe/corpora/` | Validated rules snapshots and ownership catalog | `apexe init` or automatic scan acquisition |
| `~/.apexe/log/apexe.log.YYYY-MM-DD` | Rotating diagnostic logs | Commands and governed shims |
| `~/.apexe/bin/.apexe-shims.json` | Owned shim routes | `apexe env --enable` |
| `~/.apexe/bin/.apexe-shell-init.json` | Owned shell startup blocks | `apexe env --enable` |
| `~/.apexe/bin/apexe-activate.sh` | Persistent activation helper for sh/bash/zsh | `apexe env --enable` or `--reload` |
| `~/.apexe/bin/apexe-deactivate.sh` | Persistent uninstall and environment restoration helper | `apexe env --enable` or `--reload` |
| `~/.apexe/bin/.apexe-source-scripts.json` | Ownership record retained after disable | `apexe env --enable` or `--reload` |
| `~/.apexe/bin/activate`, `~/.apexe/bin/deactivate` | Compatibility helpers removed by disable | `apexe env --enable` or `--reload` |
| `~/.apexe/audit.jsonl` | Audit trail | MCP/A2A and local governed execution |
| `~/.apexe/apcore.yaml` | apcore ecosystem config (optional) | Manual |

These are default paths; configured directories and `--shim-dir` can override
them. The namespaced shell helpers and their ownership record remain after
disable; compatibility helpers in the shim directory are removed.
Commands create their required directories automatically; `env --check` is
read-only for installation state; it may create or append diagnostic logs in
`~/.apexe/log/`.

---

## 15. Global Flags, Logging and Debugging

Three flags are global — they parse before or after the subcommand, whichever is
more natural to type:

| Flag | Default | Description |
|------|---------|-------------|
| `--log-level <LEVEL>` | `warn` unless configured or overridden by the environment | Diagnostic log level; overrides `RUST_LOG`, `APEXE_LOG_LEVEL` and `config.yaml` |
| `--log-stderr` | Off | Also mirror diagnostic logs to stderr |
| `--timeout <SECS>` | `default_timeout` from `config.yaml` (30) | Per-call timeout override, applied to every subcommand that runs or probes a wrapped binary. Refuses `0`, which would kill every call before it started |

```bash
apexe --timeout 120 scan ffmpeg     # a slow tool needs longer than the default
apexe scan ffmpeg --timeout 120     # identical; both positions parse
```

All commands and governed shims use structured logging via `tracing`.
Diagnostics go to `~/.apexe/log/apexe.log.YYYY-MM-DD`, with daily UTC rotation
and retention of the latest 14 files. The terminal does not show diagnostic
logs unless `--log-stderr` is supplied. Command results, failures, child process
output and necessary operator notices still appear normally. The governance
audit trail is independent of this log level.
On Unix, the diagnostic log directory is restricted to its owner (mode `0700`),
including when a previous installation created it with broader permissions.

Precedence: `--log-level` > `RUST_LOG` > `APEXE_LOG_LEVEL` > `config.yaml`
(`log_level`) > `warn`. Existing saved levels remain effective.

```bash
# Via CLI flag (global)
apexe --log-level debug scan git

# Via environment variable
RUST_LOG=debug apexe scan git

# Persist the level (updates log_level in config.yaml)
apexe config set log.level debug

# Also display logs in the terminal
apexe --log-level debug --log-stderr scan git
```

| Level | Records |
|-------|---------|
| `off` | No diagnostics |
| `error` | Failures only |
| `warn` | Warnings (e.g., failed to write ACL, cache miss) |
| `info` | Normal operation: tool loaded, modules registered, server started |
| `debug` | Internal detail: parser selection, cache hits, enrichment decisions |
| `trace` | Very verbose: raw help text, parsed structures |

---

## 16. Troubleshooting

### "Tool not found" during scan

The tool must be on `$PATH`:
```bash
which <tool>
```

### Scan produces incomplete results

1. Increase depth: `apexe scan <tool> --depth 3`
2. Force re-scan: `apexe scan <tool> --no-cache`
3. Check parser selection: `RUST_LOG=debug apexe scan <tool>`

### "No eligible shim modules" when enabling

Check `apexe list` first. Zero registered modules means there are no callable
bindings in the configured directory. The default is `~/.apexe/bindings`;
pre-0.8.0 bindings in `~/.apexe/modules` are not loaded there. Regenerate current
bindings with reviewed overlays rather than copying stale files:

```bash
apexe scan cat head ls --no-cache
# Review ~/.apexe/acl.yaml, then install and activate.
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
```

The scan obtains the default corpus automatically when needed. Loaded modules
still need matching authoritative native contracts to become shims; check the
exclusion diagnostics and the [eligibility requirements](#48-apexe-env).
Other platforms and variants need their own reviewed overlays.

### Native commands still resolve outside the shim directory

In bash or zsh, inspect the current terminal:

```bash
printf 'shim dir: %s\n' "$APEXE_SHIM_DIR"
type -a cat head
command -v cat
command -v head
apexe env --check
```

For the default installation, the resolved paths should be
`~/.apexe/bin/cat` and `~/.apexe/bin/head`. Source the installed
`~/.apexe/bin/apexe-activate.sh` helper (or the printed custom helper) to
activate this terminal. Aliases and functions can take precedence over PATH; absolute
binary paths bypass shims. Inspect these with `type -a`; a separate `rehash`
is not normally required after sourcing the helper.

### The installed apexe does not recognize `env --check`

Confirm which build the terminal uses, then update it using your installation
method. For a source installation, run `cargo install --path . --force` from
the updated checkout. After updating the binary:

```bash
command -v apexe
apexe env --help
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
```

### Serve command does nothing (stdio mode)

Stdio mode reads JSON-RPC from stdin and writes to stdout. It is launched by AI agents, not run interactively. Use `--show-config` to get the agent integration snippet.

### Tool invocation fails with ACL denied

The default ACL denies destructive and unknown commands. Edit `~/.apexe/acl.yaml`:

```yaml
rules:
  - callers: ["*"]
    targets: ["cli.<tool>.<command>"]
    effect: allow
```

### Stale scan results

```bash
apexe scan <tool> --no-cache
# Or clear cache entirely:
rm -rf ~/.apexe/cache/
```

### Known Limitations

- **Authenticated identities**: MCP and A2A token/JWT modes carry identities into execution. JWT roles gate ACL conditions; module-chain `callers` and authenticated subjects are separate — see [§11](#11-a2a-server).
- **Windows**: Not supported.
- **Interactive CLI tools**: PTY-based interactive sessions (e.g., `vim`) remain unsupported. Module-mode `apexe exec` supplies no stdin; native exec and shims inherit stdin only with an explicit reviewed capability.
- **Streaming output**: Captured execution buffers output; native shims pass stdout and stderr through directly, including in pipelines.
