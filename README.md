<div align="center">
  <img src="https://raw.githubusercontent.com/aiperceivable/apexe/main/apexe-logo.svg" alt="apexe logo" width="200"/>
</div>

# apexe

**Run your tools. Enforce your rules.**

Governed execution for CLI tools and HTTP APIs. Run CLI commands locally and
expose CLI and OpenAPI operations to AI agents through MCP and A2A.

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

## What is apexe?

`apexe` gives humans and AI agents a shared execution layer for existing tools.
It discovers supported CLI commands, turns their arguments into JSON Schema,
and applies configured access rules and approval checks before execution.
Calls leave audit records. Your CLI binaries keep working as they are; scanning
requires no LLM or changes to their source code.

For terminal-capable AI agents, install the shared [apexe Agent Skill](docs/agent-skill.md)
to route external CLI commands through native `apexe exec -- <command> ...`
by default in apexe-configured environments. The agent resolves project policy
before its first task command and carries an explicit ACL into every preflight,
execution and pipeline stage. Activate eligible
external commands through PATH shims when shell integration is useful, or use
MCP/A2A for protocol clients. Servers can also import HTTP operations from an
OpenAPI document, alongside CLI modules, using
the same executor for schema validation, configured ACLs and auditing.

### Choose your entry point

| Goal | Entry point | What you get |
|---|---|---|
| Prepare installed CLI coverage | `apexe init`, `apexe list --verbose` | Matching reviewed rules, typed bindings and a starter ACL to inspect |
| Run a CLI command locally | `apexe exec` | Module JSON with captured output, or reviewed native argv with passthrough streams |
| Keep native command names in your shell | `apexe env` | PATH shims for eligible external commands, activation helpers and installation checks |
| Route a terminal agent's external CLI commands | [Agent Skill](docs/agent-skill.md) + `apexe exec -- ...` | Default native execution routing in configured environments for Claude Code, Codex, pi and OpenCode |
| Connect a protocol client or import an HTTP API | `apexe serve`, `apexe a2a` | CLI modules and optional imported OpenAPI operations over MCP or A2A |
| Check a CLI contract after changes | `apexe verify` | Source drift reports and optional digest-bound evidence from explicit behavioral cases |
| Cover a custom CLI | `apexe scan` | Deterministic discovery, YAML bindings, generated ACL rules and optional Claude Skills |

Initialize CLI coverage, inspect the generated policy, then use native exec
for commands with matching reviewed grammar:

```bash
apexe init
apexe list --verbose
# Review ~/.apexe/acl.yaml and check native_ready in the init report.
apexe exec --dry-run -- ls -la
apexe exec -- ls -la
```

### Key capabilities

- **Discover and describe CLI commands** — Read `--help`, man pages and shell
  completions with 6 built-in parsers (Man, BSD Usage, GNU, Click, Cobra, Clap).
  Detect the installed variant and apply matching reviewed overlays as a fourth tier.
  Generate schemas with types, defaults, enums, required fields and argument placement.
- **Initialize from reviewed rules** — Acquire and cache external rules from
  cli-permissions or your own Git source. Pin a revision, work offline, refresh
  init-owned bindings and preserve custom bindings and existing policy.
- **Execute locally** — Use module JSON or eligible native argv, inspect checks
  with `--dry-run`, and preserve native streams and child status. PATH shims
  reuse the local governance path; native eligibility requires explicit reviewed
  grammar and capabilities. The current BSD pilot covers cat/head/ls.
- **Serve through MCP and A2A** — Share the executor across both protocols. MCP
  offers stdio, streamable HTTP, legacy SSE, structured results, an Explorer UI,
  client configuration snippets and optional metrics. HTTP-family transports
  require token authentication by default; JWT supports caller identities and roles.
- **Import HTTP APIs** — Load OpenAPI 3.0/3.1 JSON or YAML from a file or URL at
  server startup. CLI and API modules share validation, filters, configured ACLs
  and audit records. Specification-fetch, upstream API and client credentials
  stay separate. See [supported proxy operations](docs/openapi.md).
- **Apply policy and record decisions** — Generate a default-deny starter ACL,
  classify readonly/destructive/idempotent/network-reaching behavior, and gate
  calls on their actual escalating arguments when approval is enabled. Audit
  executions, refusals and ACL decisions without recording input values.
- **Check paths and bound subprocesses** — Always check schema-typed filesystem
  paths against system and credential boundaries. Execute CLI argv without a
  shell, scrub the child environment, enforce timeouts and cap captured output.
  Servers also offer retry and circuit breaking; local execution disables both.
- **Verify CLI contracts** — Detect source drift against the pre-overlay
  baseline. Optional readonly, isolated and explicitly authorized dry-run cases
  test concrete invocations; recorded evidence is invalidated by changed
  bindings, binaries or case files. Verification does not certify every invocation.
- **Support integrations** — Install the shared Agent Skill for Claude Code,
  Codex, pi and OpenCode; export YAML bindings and optional per-module Claude Skills,
  embed the Rust library, and export OpenAI tool definitions programmatically.
  Structured CLI errors and module guidance help callers inspect and correct failures.

Coverage depends on the installed CLI, its discoverable grammar and matching
rules. Local `exec` and shims use the default ACL when present; `serve` and `a2a`
require an explicit `--acl` to enforce access control. MCP interactive approval
is opt-in with `--enable-approval`; A2A approval stores are a library integration.
OpenAPI import is a server feature; local exec and shims execute CLI commands.

> **`apexe` is not a sandbox.** Normal execution runs with the host process's
> privileges. PATH shims cover eligible external commands and can be bypassed
> by builtins, aliases or absolute paths. OS isolation for explicit verification
> cases does not sandbox normal execution. See the [threat model](docs/threat-model.md)
> for the boundary and deployment requirements.

### Built on the apcore ecosystem

| Crate | Role |
|-------|------|
| [apcore](https://github.com/aiperceivable/apcore-rust) 0.32.1 | Module trait, Registry, ACL, ModuleError, Context |
| [apcore-toolkit](https://github.com/aiperceivable/apcore-toolkit-rust) 0.14.0 | ScannedModule, YAMLWriter, DisplayResolver |
| [apcore-mcp](https://github.com/aiperceivable/apcore-mcp-rust) 0.23.0 | MCP server with structured results, middleware, auth, Explorer UI |
| [apcore-a2a](https://github.com/aiperceivable/apcore-a2a-rust) 0.8.3 | A2A agent server sharing the same governed `Executor` |
| [apcore-cli](https://github.com/aiperceivable/apcore-cli-rust) 0.12.4 | `--man` page generation |

---

## Installation

### Prebuilt binary

No Rust toolchain required. Download the archive for your platform from the
[Releases page](https://github.com/aiperceivable/apexe/releases), verify the
checksum, and put the `apexe` binary on your `PATH`:

```bash
tar -xzf apexe-<target>.tar.gz
shasum -a 256 -c apexe-<target>.tar.gz.sha256
sudo mv apexe-<target>/apexe /usr/local/bin/
apexe --version
```

Available targets: `x86_64-apple-darwin`, `aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

### From source

Use the current **stable Rust** toolchain and Cargo. CI tests stable Rust;
the project does not declare or test an older minimum supported Rust version.

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

### Platform support

apexe scans Unix CLIs. What you get depends on the host:

| Host | Scanning | Curated overlays |
|---|---|---|
| **macOS** | Help, man pages, completions and matching overlays | Matching `bsd` / `apple` rules from the external corpus |
| **Linux** | Help, man pages, completions and matching overlays; install `man-db` for tier 2 | Matching `gnu` rules from the external corpus |
| **FreeBSD** | Full | `bsd` overlays apply |
| **OpenBSD / NetBSD / DragonFly** | Full | none — falls back to scanning |
| **Other Unix** (Solaris, illumos, AIX) | Tiers 1–2 | none |
| **Windows** | **Not supported** | none |

The GNU overlays carry no platform condition — they are selected by probing the
binary — so they apply anywhere GNU tools are installed, including WSL and a
macOS box with Homebrew coreutils. The BSD overlays declare `macos` and
`freebsd`, which is why the other BSDs fall back to scanning: their tools are
separately evolved code, not the same source, so claiming otherwise would be a
guess.

**On Windows apexe runs but produces little.** Tier 2 shells out to `man` and
tier 3 to `zsh`; both are absent, so both return nothing rather than failing
loudly. Tier 1 still runs `--help`, but the parsers target Unix conventions
while Windows help is `/?` output with `/X` switches. Expect near-empty results,
not an error message. Native Windows support means a PowerShell reflection path
and a `/?` parser, and is not implemented.

---

## Quick Start

Initialize once and review policy. Terminal agents use the shared Skill and
native exec; env is optional shell integration and MCP/A2A are protocol options:

```bash
# Fetch cli-permissions and prepare bindings for matching tools on this machine.
apexe init
# Inspect module IDs, annotations and the generated ACL decisions.
apexe list --verbose
# Review ~/.apexe/acl.yaml before executing or serving commands.
# Default native argv: only for modules reported under native_ready.
apexe exec --dry-run -- ls -la
apexe exec -- ls -la

# JSON alternative when native grammar is unavailable and policy permits.
apexe exec cli.ls

# Optional shell shims: require matching reviewed native grammar.
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check

# Optional: expose initialized commands to an MCP client with the reviewed ACL.
apexe serve --acl ~/.apexe/acl.yaml
```

`init` uses the configured `default_overlay_repository`, normally
[cli-permissions](https://github.com/aiperceivable/cli-permissions). It discovers
commands declared by the loaded rules, checks the installed binaries and their
variants, and generates local bindings and a starter ACL. It does not install
CLI binaries, activate your shell or start a server. Native exec and shims
require reviewed native grammar; initialized modules without it remain usable
through module JSON when their ACL and approval checks permit.

Repeat `apexe init` to discover newly installed covered tools while preserving
existing bindings. Use `apexe init --refresh` to update the selected rules and
regenerate unchanged bindings owned by init. The report identifies skipped
commands, fallback coverage, preserved bindings and resolved Git commits.
`config --show` displays configured values; a repository URL there does not
mean initialization has completed.

For a new ACL, unknown, destructive and network-reaching modules default to
deny. Existing manual rules and policy defaults are preserved. Local exec/env
load the default ACL when present; `serve` and `a2a` still need an explicit
`--acl`. The current native pilot covers BSD cat/head/ls; GNU scanning support
does not imply native eligibility. When matching native grammar is unavailable,
use module JSON if its checks permit; a server is not required for local use.
See the [Quick Start guide](docs/quickstart.md) for the full workflow.

### Claude Code, Codex, pi and OpenCode: Agent Skill + native exec

Follow the [four-step Agent quick integration](docs/agent-skill.md#quick-integration):
prepare and review CLI coverage/ACL, install the shared Skill, declare the
route in `CLAUDE.md` or `AGENTS.md`, then check an ordinary listing and pipeline.
From an apexe source checkout, install for your user:

```bash
python3 scripts/install_agent_skill.py --scope user
```

Claude Code uses `~/.claude/skills/apexe`; Codex and pi share
`~/.agents/skills/apexe`, which OpenCode also discovers. In a configured
project, the Skill routes ordinary CLI requests through `apexe exec -- ...`,
with inspected preflights, the selected ACL on every call and immediate stopping
on refusal. No MCP server or shim activation is needed; `env` is optional.

Installing the Skill alone does not enroll projects or intercept unrestricted
shell tools. Copy the [project instruction template](docs/agent-skill.md#3-declare-the-projects-execution-route)
and inspect actual calls during acceptance. See [troubleshooting](docs/agent-skill.md#troubleshooting)
for discovery, stale copies, native coverage and ACL issues, and the
[maintainer verification procedure](docs/agent-skill-verification.md) for controlled
three-scenario tests. Generated per-module Skills complement this shared route.

### Optional MCP integration: Claude Desktop

```bash
apexe serve --show-config claude-desktop --acl ~/.apexe/acl.yaml
# Copy output to ~/Library/Application Support/Claude/claude_desktop_config.json
# Restart Claude Desktop — initialized commands appear as MCP tools
```

The snippet carries the rest of the command line (`--bindings-dir`, `--prefix`,
`--tags`, `--acl`, `--name`, the governance toggles), so pass the flags you
intend to serve with. `--auth-token` / `--jwt-secret` are deliberately never
included.

### Optional MCP integration: Cursor

```bash
apexe serve --show-config cursor --acl ~/.apexe/acl.yaml
# Add to Cursor's MCP settings
```

---

## Commands

### `apexe init`

Prepare this machine's bindings and starter ACL from reviewed rules:

```bash
apexe init
apexe init --overlays https://github.com/aiperceivable/cli-permissions
apexe init --overlays ../cli-permissions --ref <COMMIT>
apexe init --offline
apexe init --refresh
```

The default table output shows initialization stages and the tool being scanned
as work proceeds, independently of the log level. `--format json` outputs only
the final report.

The former `overlays` command has been removed. Rules installation, pinning and
refresh are part of initialization. `config --init` only creates a configuration
template; it does not prepare command bindings.

### `apexe list`

List registered modules.

```bash
apexe list                     # Table format
apexe list --format json       # JSON format
apexe list --available-only    # Only binaries reachable on this machine
apexe list --verbose --format json  # Schemas, annotations and ACL context for agents
```

### `apexe exec`

Execute reviewed native argv without activation, or a module with JSON input:

```bash
apexe exec -- ls -la
apexe exec --dry-run -- ls -la
apexe exec cli.ls --input '{"file":["."]}'
apexe exec cli.ls --input-file request.json --dry-run
cat request.json | apexe exec cli.ls --input-file - --error-format json
```

Native mode preserves streams and child status. The JSON property names depend
on the binding. Unknown commands default to denial; explicit unmanaged policies
are described in the [exec reference](docs/user-manual.md#47-apexe-exec).

### `apexe env`

Install and activate eligible external-command shims:

```bash
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
source ~/.apexe/bin/apexe-deactivate.sh
```

The namespaced helpers are installed with apexe; no source checkout is needed.
See the [env reference](docs/user-manual.md#48-apexe-env) for shell support and
activation boundaries.

### `apexe serve`

Serve CLI modules and optional imported OpenAPI operations through MCP.

```bash
apexe serve --acl ~/.apexe/acl.yaml # stdio (default)
apexe serve --transport http --port 8000 --acl ~/.apexe/acl.yaml # HTTP
apexe serve --transport http --port 8000 --explorer --acl ~/.apexe/acl.yaml # HTTP + browser UI
apexe serve --transport sse --port 8000 --acl ~/.apexe/acl.yaml # Server-Sent Events (deprecated upstream; prefer http)
apexe serve --show-config claude-desktop --acl ~/.apexe/acl.yaml # Print integration config (carries the other flags; never a credential)
apexe serve --name my-tools --acl ~/.apexe/acl.yaml # Custom server name
apexe serve --transport http --metrics --acl ~/.apexe/acl.yaml # + /metrics (Prometheus) and /usage
apexe serve --no-circuit-breaker --no-retry --acl ~/.apexe/acl.yaml # Disable resilience middleware
apexe serve --transport http --auth-token "$TOKEN" --acl ~/.apexe/acl.yaml # Pin the bearer token (else one is generated)
apexe serve --prefix cli.git --acl ~/.apexe/acl.yaml # Serve only git tools (not listed AND not callable)
apexe serve --no-log-arguments --acl ~/.apexe/acl.yaml # Drop argument payloads from every log event, errors included
```

> HTTP/SSE transports require a bearer token by default; a token is generated
> and written to stderr at startup on a loopback bind (not through the log
> pipeline, so `--log-level warn` still shows it). `--auth none` on a non-loopback
> bind refuses to start without `--allow-unauthenticated-bind`. stdio is
> unaffected. See [`docs/user-manual.md`](docs/user-manual.md).


### `apexe a2a`

Serve CLI modules and optional imported OpenAPI operations through A2A. Shares configured governance (ACL, logging, audit) with `apexe serve` via the same `Executor`.

```bash
apexe a2a --acl ~/.apexe/acl.yaml # http://127.0.0.1:8000 (default)
apexe a2a --url http://127.0.0.1:9000 --explorer --acl ~/.apexe/acl.yaml # Custom port + browser UI
apexe a2a --acl ~/.apexe/acl.yaml               # Governed by an ACL policy
apexe a2a --cors-origin https://example.com --acl ~/.apexe/acl.yaml # Allow a browser origin
```

> **A2A requires bearer authentication by default.** apcore-a2a provides an
> `Authenticator`; apexe wires it through token/JWT modes. Use `APEXE_AUTH_TOKEN`
> or the generated startup token. `--auth none` off loopback requires
> `--allow-unauthenticated-bind`. JWT subjects and roles reach ACL and audit.

> A2A has no interactive elicitation transport, so there is **no `--enable-approval`
> flag** on `apexe a2a` (it's available on `apexe serve`, and on A2A only via the
> library `ApprovalStore` API). See [`docs/user-manual.md`](docs/user-manual.md).


### `apexe verify`

Compare fresh CLI source contracts with the recorded pre-overlay baseline,
without replacing bindings. Breaking drift, unclassified changes and missing
baselines fail the command. Optional cases test concrete readonly, isolated or
explicitly authorized dry-run invocations. Recorded evidence binds outcomes to
binding, binary and case digests; overlay provenance remains a separate author claim.

```bash
apexe verify --report verification.json
apexe verify --cases readonly-cases.json --record --report verification.json
```

The ordinary readonly allowlist covers reviewed system cat/head/ls binaries.
Other isolated and dry-run cases require executable digest pinning and working
OS isolation. See the [verification guide](docs/verification.md) for admission
rules, evidence scope and invalidation.

### `apexe config`

Show, initialize or update configuration.

```bash
apexe config --show     # Print resolved config (YAML)
apexe config            # Same as --show
apexe config --init     # Create ~/.apexe/config.yaml
apexe config set log.level debug  # Update the saved diagnostic log level
```

### `apexe policy`

Show the filesystem boundary every wrapped tool is checked against, or check one
path against it. The summary is read off the **installed** guard rather than
re-derived from config, so it always describes the policy actually in force.

```bash
apexe policy                                   # The whole boundary
apexe policy --format json                     # Machine-readable
apexe policy --path /etc/passwd                # Refused to a writer
apexe policy --path /etc/passwd --mode read    # Allowed to a readonly module
apexe policy --path ~/.ssh/id_rsa --mode read  # Refused: credentials bind readers
```

`--path` calls the guard's own check — the same call a wrapped tool's argument
goes through — so its verdict cannot drift from a real invocation's. See
[user manual §4.6](docs/user-manual.md) and §9.7.

---

### `apexe scan <TOOLS>...`

Scan CLI tools and generate binding files + ACL rules.

```bash
apexe scan ls jq curl                   # Scan multiple tools
apexe scan git --depth 3               # 3 levels of subcommands (default: 2, max: 5)
apexe scan git --no-cache              # Force re-scan
apexe scan git --format json           # Output as JSON (also: yaml, table)
apexe scan git --output-dir ./out      # Custom output directory
apexe scan git --skills-dir ./out      # Also write .claude/skills/<id>/SKILL.md per module
apexe scan ls --overlay ./ls.json      # Apply a curated overlay instead of trusting the scan
```

#### Verify source drift and explicit cases

```bash
apexe verify --report ./drift-report.json
apexe verify --cases ./readonly-cases.json --record --report ./verification.json
# Explicitly reviewed and digest-pinned dry-run cases require working OS isolation.
apexe verify --cases ./dry-run-cases.json --allow-dry-run --isolation required --record
# Self-contained temporary CLI demo: dry-run, fixture effects and evidence invalidation.
python3 examples/verification/demo.py --apexe target/debug/apexe
```

Case modes are `readonly`, `dry_run` and `isolated`. macOS uses `sandbox-exec`;
Linux uses optional `bwrap` and unprivileged namespaces. Required isolation
failures are reported as `unverifiable` without executing the case. See the
[verification guide](docs/verification.md) and
[runnable example](examples/verification/README.md).

#### Tool variants and overlays

A command name does not identify a program: macOS `/bin/ls` is BSD, Linux
`/usr/bin/ls` is GNU coreutils, Alpine is BusyBox, and Homebrew coreutils puts
GNU `ls` on a macOS box. Every scan probes the binary (`<binary> --version`) and
records the result as `variant`, then looks for a **curated overlay** matching
`(command, variant, version_range)`.

The vocabulary is `bsd`, `gnu`, `apple`, `busybox`, `unknown`. `gnu` is the
whole GNU family, not just coreutils — `diffutils`, `tar`, `sed` and `bash` are
separate projects, and the specific package is recorded in `provenance.package`
and pinned, where it matters, in `match.probe.output_contains`. `apple` covers
Apple's own ports, whose banner names Apple rather than BSD (macOS `sort`
reports `2.3-Apple (197)`, `git` reports `(Apple Git-155)`).

`match.platform`, by contrast, is an **open string** taken verbatim from Rust's
`std::env::consts::OS` — `macos`, `linux`, `freebsd`, `openbsd`, `netbsd`,
`dragonfly`, `solaris` and anything else a host reports. Operating systems are
not an enumerable set, so naming a new one needs no apexe change. There is
deliberately no Linux distribution dimension: GNU coreutils `ls` has the same
83 flags on Debian, Ubuntu and Fedora across three different releases, while
BusyBox `ls` has 21 — divergence tracks the implementation, which `variant`
already captures.

The classification order is load bearing: BSD is tested **before** GNU, because
macOS `grep` reports `grep (BSD grep, GNU compatible) 2.6.0-FreeBSD` and is a
BSD tool advertising GNU compatibility rather than a GNU tool. Apple is tested
only after `<arch>-apple-<os>` target triples are stripped, because
`curl 8.7.1 (x86_64-apple-darwin25.0)` is not an Apple port. See
[Authoring Tool Overlays](docs/overlays.md).

An overlay is a reviewed description of one variant of one command. In
`authoritative` mode it replaces the scan's flags, positional args and
subcommands entirely; in `merge` mode it overrides matching flags and adds
missing ones. Overlays are the only source that can state `conflicts_with`,
because no help or man page format expresses mutual exclusion machine-readably.

**Initialize command coverage with `apexe init`.** Reviewed facts come from
[cli-permissions](https://github.com/aiperceivable/cli-permissions); apexe does
not embed a second copy. Validated Git snapshots are cached under
`~/.apexe/corpora/`. Normal initialization reuses the recorded commit. Selection
checks the actual tool variant, platform, version constraints and probes.

```bash
apexe init
apexe init --format json
# Refresh rules and bindings originally generated by init.
apexe init --refresh
# Refresh an active shim installation after binding changes.
apexe env --reload
```

Use `init --overlays <SOURCE>` to select an HTTPS/GitHub source or local Git
repository for that invocation; use `init --ref <REF>` to pin a revision. For a
standing mirror, set `default_overlay_repository` in `config.yaml` or
`APEXE_DEFAULT_OVERLAY_REPOSITORY`. `init --offline` uses cached or local rules
without network access. An unavailable default source reports degraded fallback
coverage; an explicit source or refresh failure exits nonzero.

`scan` remains available for custom tools and targeted rescans. Its first online
use can also obtain the default corpus automatically, and `scan --offline`
prevents that download. Automatic scan acquisition is limited to 10 seconds or
the shorter global timeout. Initialization uses the configured Git timeout.
See the [init reference](docs/user-manual.md#410-apexe-init) for preservation,
source selection and fallback behavior. Git and CLI binaries are separate
prerequisites. An optional corpus `manifest.json` defines its name, release and
overlay directory; legacy repositories use an `overlays` directory.

For local overlay development, read the working directory directly:

```bash
# From the apexe checkout, with cli-permissions checked out alongside it.
APEXE_OVERLAY_DIRS=../cli-permissions/overlays apexe scan ls --no-cache
```

This includes uncommitted edits without installing a new corpus snapshot. The
variable applies only to this scan. Rescan after editing overlays; use
`apexe env --reload` afterward if the eligible native command set changes.
For a managed installation from a local Git repository instead, use
`apexe init --overlays ../cli-permissions`: it initializes from committed Git content.

For equally strong matches, overlays are loaded in increasing precedence: a packaged corpus in a
well-known location (`$XDG_DATA_HOME/cli-permissions/overlays`,
`/usr/local/share/…`, `/usr/share/…`, and Homebrew's prefix on macOS),
the managed default corpus, other installed corpora in installation order,
each directory listed in `overlay_dirs` in the order given,
`~/.apexe/overlays/*.{json,yaml}`, and `--overlay <PATH>`. `overlay_dirs` is how
a corpus someone else maintains — a team policy repository, a plugin that ships
overlays — is consumed without copying files into the operator's own directory;
that directory is still read last, so a hand-written local file wins the tie.
The format is defined by [`tool-overlay.schema.json`](https://github.com/aiperceivable/cli-permissions/blob/main/schemas/tool-overlay.schema.json), which lives with
the corpus rather than here — apexe is one consumer of it, not its owner.
It describes the command rather than apexe, so a second implementation can read
the same files — see [Reading Overlays Without apexe](docs/overlay-consumers.md).

Every emitted flag carries `sources` and source-derived `confidence`:
`high` (author-reviewed overlay or completion spec) > `medium` (two independent
heuristic sources agree) > `low` (a single unconfirmed source). The corpus's
legacy `verified` declaration remains an author claim; it cannot alone earn
runtime confidence. `apexe verify --cases <PATH> --record` earns module
`metadata.confidence: verified` only for passed explicit cases with matching
binding, executable and case-file digests. Changes invalidate that evidence.
See [contract verification](docs/verification.md) for its scope and case modes.

An overlay flag may also declare `long_running: true` — "this option may make
the command never terminate on its own", which is why `tail -f` hangs an agent
until the harness timeout. It reaches the emitted contract as the JSON Schema
extension keyword `x-apexe-long-running`, so an executor can bound the timeout
or refuse instead of blocking. The claim is possibility, not certainty: BSD
`tail -f` returns immediately when its input is a pipe.

**Writing one.** An overlay claiming `confidence: verified` must carry a
`provenance` block recording how it was checked — which build was consulted,
which document was read (`man-page` / `help` / `vendor-docs`), and when. The
schema rejects a `verified` overlay without it, because a `verified` +
`authoritative` overlay replaces the entire scan result with an assertion
nobody can re-check.

Read the flag list off the tool itself, never from memory:

```bash
man -P cat ls | col -b                          # BSD / macOS (strips overstrike)
docker run --rm debian:stable-slim ls --help    # GNU coreutils
docker run --rm alpine ls --help                # BusyBox
```

There is deliberately **no `apexe overlay verify` command**. A tool can compare
flag names, but not descriptions, `conflicts_with`, types or enum values — and
those are the reason overlays exist. A green check covering only the mechanical
half would read as "this overlay is correct".

See **[Authoring Tool Overlays](docs/overlays.md)** for the full procedure,
including how to cross-check a draft against a reference installation and the
traps already hit in practice (of the 38 flags `ls` shares between its BSD and
GNU variants, **zero** have identical descriptions — never copy one across).


## How It Works

```text
CLI binaries
     |
     v
Help / man / completions + matching reviewed overlays
     |
     v
Typed CLI modules --> YAML bindings + starter ACL + optional Claude Skills
                           |
             +-------------+-------------------+
             |                                 |
       exec / PATH shims                   MCP / A2A <--- OpenAPI document
             |                                 |            |
             |                                 |       HTTP modules
             +----------------+----------------+
                              |
                       Shared executor
              schema validation / configured ACL
                approval configuration / audit
                              |
                 +------------+------------+
                 |                         |
          CLI subprocess                HTTP API
       path checks, clean env       imported proxy operation
       direct argv, timeout        upstream call timeout
```

`verify` checks stored CLI contracts against fresh source observations and can
record evidence for explicit cases. HTTP modules are imported at server startup
and are not persisted as CLI bindings. CLI path checks and subprocess controls
apply to the CLI branch; remote API data needs its own authorization boundary.
See the [feature manifest](docs/FEATURE_MANIFEST.md) for the implementation map.

### Behavioral annotations

| Signal | Inference |
|--------|-----------|
| Command `list`, `show`, `status`, `get` | `readonly: true`, `cacheable: true` |
| Command `delete`, `rm`, `kill`, `destroy` | `destructive: true`, `requires_approval: true` — needs approval when the gate is enabled |
| Tool `env`, `xargs`, `sudo`, `timeout`, `nice` | `destructive: true`, whatever the name suggests: their arguments *are* another command |
| Tool `curl`, `wget`, `ssh`, or subcommand `push`, `clone`, `login` | `open_world: true` |
| Command *accepts* `--force`, `-f`, `--hard`, … | Marked `requires_approval` as a ceiling, with the escalating properties recorded — the gate then prompts only for a call that actually **sends** one |
| Overlay `risk: escalates` / `executes` / `benign` | Human assertion about one flag: goes to the gate when sent / refused outright / exempted from the name list |

A scan sees the flags a command accepts; a call carries the flags a caller sent.
When the approval gate is enabled, only a call that sends an escalating
argument prompts: `git log` needs no prompt while `git log --all` does, provided
the ACL permits the operation. See [user manual §8](docs/user-manual.md).

### Schema generation

| CLI Type | JSON Schema |
|----------|-------------|
| `--message "hello"` | `"type": "string"` |
| `--count 5` | `"type": "integer"` |
| `--config /path` | `"type": "string", "format": "path"` |
| `--url https://...` | `"type": "string", "format": "uri"` |
| `--format json\|yaml` | `"type": "string", "enum": ["json","yaml"]` |
| `--include a --include b` | `"type": "array", "items": {"type":"string"}` |

---

## Configuration

Resolved in 4 tiers (highest wins): **CLI flags > env vars > config file > defaults**

```bash
apexe config --init    # Creates ~/.apexe/config.yaml
```

| Env Variable | Default | Description |
|-------------|---------|-------------|
| `APEXE_BINDINGS_DIR` | `~/.apexe/bindings` | Binding file storage |
| `APEXE_CACHE_DIR` | `~/.apexe/cache` | Scan cache |
| `APEXE_LOG_LEVEL` | `warn` | Diagnostic file log level |
| `APEXE_TIMEOUT` | `30` | CLI subprocess timeout (seconds) |
| `APEXE_SCAN_DEPTH` | `2` | Subcommand recursion depth |

---

## File Locations

| Path | Purpose |
|------|---------|
| `~/.apexe/config.yaml` | Configuration |
| `~/.apexe/bindings/*.binding.yaml` | Generated tool bindings |
| `~/.apexe/cache/` | Scan result cache |
| `~/.apexe/acl.yaml` | Access control rules |
| `~/.apexe/audit.jsonl` | Audit trail |
| `~/.apexe/log/apexe.log.YYYY-MM-DD` | Daily diagnostic logs (UTC; latest 14 files retained) |

---

## Examples

See [examples/README.md](examples/README.md) for full details.

| Example | Description | Run |
|---------|-------------|-----|
| [basic](examples/basic/) | Shell script: scan → list → serve | `./examples/basic/run.sh` |
| [programmatic](examples/programmatic.rs) | Rust library: scan → convert → export OpenAI tools → build MCP server | `cargo run --example programmatic` |
| [acl_demo](examples/acl_demo/) | Rust library: role-based ACL rules on `CliModule` calls via `Executor` | `cargo run --example acl_demo` |
| [path_guard](examples/path_guard/) | Rust library: the always-on path boundary — which directories a call may touch | `cargo run --example path_guard` |
| [cli_permissions](examples/cli_permissions/) | Scan with the external reviewed command corpus | `./examples/cli_permissions/run.sh` |
| [local_execution](examples/local_execution/) | Local execution and optional native PATH shims in a temporary home | `python3 examples/local_execution/demo.py` |
| [verification](examples/verification/) | Isolated case verification and evidence invalidation | `python3 examples/verification/demo.py` |
| [openapi_import](examples/openapi_import/) | HTTP API import through MCP/A2A with ACL enforcement | `python3 examples/openapi_import/demo.py` |

---

## Developer Guide

### Build & Test

```bash
cargo build                                             # Build
cargo test --all-features --locked                      # Full suite and real CLI parameter gate
cargo test --all-features --examples --locked            # Rust example tests
make check CORPUS=../cli-permissions/overlays             # All quality gates and runnable examples
make test-scripts                                      # Runtime-validator regression tests
cargo clippy --all-targets --all-features -- -D warnings  # Lint
cargo fmt --all -- --check                              # Format check
cargo run --example programmatic                        # Run example
```

Runtime checks require Python 3.9+, Git and working OS isolation (`sandbox-exec`
on macOS; `bwrap` and unprivileged user namespaces on Linux). The full example
gate requires `jq`, `curl` and the reviewed cli-permissions corpus. CI runs the
full gates on Linux and macOS and uploads JSON receipts. See the
[Developer Guide](docs/developer-guide.md) for coverage rules and platform scope.

### Adding a Custom Parser

Implement the `CliParser` trait in `src/scanner/protocol.rs`:

```rust
pub trait CliParser: Send + Sync {
    fn name(&self) -> &str;
    fn can_parse(&self, help_text: &str) -> bool;
    fn parse(&self, help_text: &str, tool_name: &str) -> anyhow::Result<ParsedHelp>;
    fn priority(&self) -> u32; // lower = tried first
}
```

### Logging

All commands and governed shims write diagnostics to `~/.apexe/log/`, with a
`warn` default and no diagnostic logs on the terminal. Files rotate daily in
UTC; the latest 14 files are retained. Command results, errors and necessary
operator notices remain visible. Logging does not change the governance audit
trail.
On Unix, the log directory is private to its owner (mode `0700`).

```bash
apexe config set log.level info          # Persist the level in config.yaml
apexe --log-level debug scan git         # Override for this invocation
apexe --log-level debug --log-stderr scan git  # Also mirror logs to stderr
```

Level precedence: `--log-level` > `RUST_LOG` > `APEXE_LOG_LEVEL` >
`config.yaml` (`log_level`) > `warn`. `off` disables diagnostic records.
Existing configurations keep their saved level until explicitly changed.

### Releasing

`apdev-rs release` handles tagging, the GitHub Release, and publishing to
crates.io; pushing the `rust/vX.Y.Z` tag it creates automatically triggers
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds
the prebuilt binaries described in [Installation](#installation) and attaches
them to that release. No separate step is needed for a normal release.

To backfill binaries onto a release that's missing them (e.g. one published
before this workflow existed), dispatch it manually against the existing tag:

```bash
gh workflow run release.yml -f tag=rust/vX.Y.Z
```

---

## Local execution and OpenAPI import

```bash
# Use native argv without activating PATH shims.
apexe exec --dry-run -- ls -la
apexe exec -- ls -la

# Inspect a module with JSON input, then execute with captured JSON output.
apexe exec cli.ls --input '{"file":["."]}' --dry-run
apexe exec cli.ls --input '{"file":["."]}'

# Import an HTTP API alongside scanned CLI tools.
apexe serve --openapi ./openapi.yaml --openapi-base-url https://api.example.com --acl ./acl.yaml
apexe a2a --openapi ./openapi.yaml --openapi-base-url https://api.example.com --acl ./acl.yaml
```

Local execution defaults to the `local` caller and loads `~/.apexe/acl.yaml`
when present; a malformed policy fails closed. Native mode requires the same
reviewed grammar as shims and preserves child streams and exit status. Unknown
commands default to refusal; `--unmanaged allow|approval` opts in without
overriding the ACL. Unmanaged argv has no schema-based path checks and receives
null stdin. `run` has been replaced by `exec`, with no alias. `--as` is available
only for `--dry-run`. OpenAPI 3.0/3.1 JSON/YAML sources may be local files or HTTP(S)
URLs. Operations default to the `api.*` namespace and share schema validation,
ACL and auditing with CLI modules. Set `APEXE_OPENAPI_TOKEN` for the upstream
API credential; `--openapi-header` is only for fetching the specification.
See the [command reference](docs/user-manual.md#47-apexe-exec),
[OpenAPI guide](docs/openapi.md), and [runnable API example](examples/openapi_import/README.md)
for flags, proxy limits and tested setup.

After initialization with rules that declare reviewed native grammar, install the
shell helpers once, then source them in bash or zsh. No source checkout is needed:

```bash
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
printf 'one\ntwo\n' | cat | head -n1
ls -la
# Uninstall owned shims and restore this terminal's previous environment.
source ~/.apexe/bin/apexe-deactivate.sh
# The installed helper remains available and automatically re-enables shims.
source ~/.apexe/bin/apexe-activate.sh
```

The scripts are generated by the installed apexe binary, including an executable
installed in `~/.cargo/bin`. Activation uses `--reload`, preserving installed
bindings, ACL and filters; when absent, an installation is enabled automatically.
Deactivation uses `--disable`, removing owned shims and managed startup blocks,
then restores the PATH and APEXE_SHIM_DIR saved before the first activation.
The namespaced helpers remain installed after disable. Repeated activation
preserves that snapshot. No separate `eval` or `rehash` is needed. POSIX sh uses
`.` instead of `source`; executing a script changes only its child process.

Like Python's venv activation, sourcing changes the current terminal. apexe
selects governed external commands; it does not select Python, add a prompt
prefix or define a `deactivate` function. To combine them, activate the venv first, then apexe;
deactivate apexe first, then call the venv's `deactivate`.
For custom shim directories, the helpers are installed directly in `<shim-dir>`;
enable/reload prints their full paths. See the
[command reference](docs/user-manual.md#48-apexe-env) for configuration and lifecycle.

`--enable` installs shims and configures startup in one step, detecting the shell
from `SHELL`; use `--shell` and `--rc-file` for a
custom startup layout. bash, zsh and fish load shims in new interactive
terminals; sh uses its login profile. PowerShell requires
`apexe env --enable --shell powershell --rc-file "$PROFILE"` in the target host.
Enabling preserves existing configuration and avoids duplicate blocks. Once
enabled, use native commands directly without repeating setup. `--reload`
retains the installed binding directory, ACL and filters when refreshing the
command set. With no installation, it automatically enables shims using current
settings.
`apexe env --check` diagnoses the active PATH, owned links, manifest, installed
bindings, source scripts and ACL without executing wrapped commands. It prints
`PASS`, `FAIL`, `WARN` and `SKIP` results and a summary; failures return status 1,
including missing ACLs. Warnings alone return status 0.
Custom installations can use `--shim-dir`; otherwise the check honors
`APEXE_SHIM_DIR`. See the [command reference](docs/user-manual.md#48-apexe-env)
for its scope and exit status.

Shims inherit output streams, preserve exit status and use the same local
policy. Only matching authoritative overlays declaring order-independent
accepted invocations are eligible. stdin and short bundles require explicit
capabilities; the verified BSD corpus pilot enables `cat`, `head` and `ls`.
Builtins, aliases and absolute paths bypass PATH shims. See
[F8](docs/features/v2-f8-local-execution.md) and the
[runnable local example](examples/local_execution/README.md) for latency, scope
and temporary-home demonstrations.

Run the self-contained examples from the repository root with Python 3:

```bash
cargo build --all-features
python3 examples/local_execution/demo.py --apexe target/debug/apexe
# On macOS, with the reviewed companion corpus available:
python3 examples/local_execution/demo.py --apexe target/debug/apexe \
  --corpus ../cli-permissions/overlays --shims
python3 examples/openapi_import/demo.py --apexe target/debug/apexe
python3 examples/openapi_import/demo.py --apexe target/debug/apexe --remote-spec
```

The local demo checks preflight, execution, ACL denial and audit in a temporary
HOME; `--shims` also checks a native pipeline and owned uninstall. The OpenAPI
demo starts its own local HTTP API and exercises both MCP and A2A, including
JSON requests, ACL denial and separate specification/API credentials. See the
[Quick Start](docs/quickstart.md#7-customize-and-maintain) for the walkthrough.

## Documentation

| Document | Description |
|----------|-------------|
| **[Documentation hub](docs/README.md)** | Choose the user or developer documentation path |
| **[Quick Start](docs/quickstart.md)** | Initialize command coverage, review the ACL, and connect an agent |
| **[User Manual](docs/user-manual.md)** | Operator reference — commands, configuration, governance, transports, and troubleshooting |
| **[Threat Model](docs/threat-model.md)** | What apexe enforces and what still requires sandboxing or host policy |
| **[Security Policy](SECURITY.md)** | How to report a vulnerability privately, and what counts as one |
| **[Basic example](examples/basic/README.md)** | Runnable terminal walkthrough with ACL enforcement |
| **[cli-permissions example](examples/cli_permissions/README.md)** | Scan verified POSIX command facts from the external corpus |
| **[Changelog](CHANGELOG.md)** | Release history and migration notes |

### Developer and contributor references

| Document | Description |
|----------|-------------|
| **[Developer Guide](docs/developer-guide.md)** | Library embedding, extension points, and contributor navigation |
| [Programmatic example](examples/programmatic/README.md) | Rust library API usage |
| [ACL library example](examples/acl_demo/README.md) | Role-based ACL with an explicit apcore context |
| [Authoring Tool Overlays](docs/overlays.md) | Write and verify a curated overlay |
| [Technical Design](docs/apcore-integration/tech-design.md) | v0.1.0 architecture with apcore ecosystem integration |
| [Feature Manifest](docs/FEATURE_MANIFEST.md) | Module map, crate dependencies, project status |
| [Feature Specs](docs/features/v2-overview.md) | Detailed specifications for features F1-F7 |

#### Feature specs

| Spec | Description |
|------|-------------|
| [F1: Scanner Adapter](docs/features/v2-f1-scanner-adapter.md) | ScannedCLITool → ScannedModule conversion |
| [F2: Module Executor](docs/features/v2-f2-module-executor.md) | apcore Module trait for CLI subprocess execution |
| [F3: Binding Output](docs/features/v2-f3-binding-output.md) | apcore-toolkit YAMLWriter integration |
| [F4: MCP Server](docs/features/v2-f4-mcp-server.md) | apcore-mcp server builder |
| [F5: Governance](docs/features/v2-f5-governance.md) | apcore ACL wrapper + apexe's own audit trail |
| [F6: Error Migration](docs/features/v2-f6-error-migration.md) | ApexeError → ModuleError conversion |
| [F7: Config Integration](docs/features/v2-f7-config-integration.md) | apcore Config integration |

---

## License

Apache-2.0
