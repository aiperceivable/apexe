# F8: Local Execution -- governed CLI invocation from the terminal

| Field | Value |
|---|---|
| **Feature ID** | F8 |
| **Priority** | P1 (Surface) |
| **Status** | Stages 1 and 2 implemented. Captured execution, governance preflight and native PATH shims share the executor. The bounded BSD pilot covers cat/head/ls; broader overlay eligibility requires explicit verification. |
| **Dependencies** | F2 (Module Executor), F5 (Governance) |
| **Depended On By** | None yet |
| **New Files** | `src/cli/run.rs`, `src/cli/local.rs`, `src/cli/env.rs`, `src/cli/shim.rs`, `src/adapter/argv.rs`, `src/module/io.rs`, `src/runtime_path.rs` |
| **Modified Files** | `src/cli/mod.rs`, `src/module/executor.rs`, `src/module/cli_module.rs`, `src/module/registry.rs` |

---

## 1. Purpose

Give the local terminal the same governed execution path that MCP and A2A
callers already get. `apexe run`, `apexe serve` and `apexe a2a` share
[`build_executor`](../../src/module/registry.rs). Stage 1 supplies a local entry
for shell scripts, CI steps, Claude Code hooks and humans inspecting an ACL
decision.

Two surfaces, in order:

1. **`apexe run <module_id>`** -- structured input, one module per invocation,
   captured output. The minimal base.
2. **`apexe env` + PATH shims** -- the wrapped tool invoked by its own name and
   its own argv syntax, so composition (pipes, `&&`, existing scripts) comes
   from the user's shell.

Stage 2 adds passthrough I/O, clean PATH resolution and explicit per-module
native grammar capabilities. Both surfaces retain the same ACL, approval,
path guard, schema validation, environment scrubbing and audit chain.
Installation includes only eligible modules (§5).

---

## 2. Motivation

### 2.1 Why the structured surface alone is not enough

The structured surface accepts typed JSON, so a purely structured
local surface reads like this:

```bash
apexe run cli.ls --input '{"long":true,"paths":["/tmp"]}' \
  | apexe run cli.cat --input '{}'
```

Nobody will type that when `ls -l /tmp | cat` exists. A control more expensive
than the thing it governs gets switched off -- the failure mode
`docs/threat-model.md` §3 already describes for approval prompts. For any
surface whose audience is a human or a pre-existing script, native syntax is not
a convenience; it is the precondition for the control being used at all.

### 2.2 What the shim buys

With eligible cat/head/ls bindings installed and `~/.apexe/bin` prepended to
PATH, `cat file | head -n1` becomes two governed calls. The user's shell handles
pipes and composition; each shim handles only its own command and argv. Each
stage has its own ACL decision, path checks and audit record. Commands absent
from the installed set remain ordinary shell commands.

### 2.3 What it does not buy

Three limits, all of which must be stated in the feature's own capability
description rather than buried in a caveats list.

**Bypassable by absolute path.** `/bin/ls` is not on `PATH` lookup, and a user
who can type `|` can type a path. This is `docs/threat-model.md` §5.1 ("An agent
that has a shell does not need apexe") applied locally.

**Shell builtins are never intercepted, and the gap is silent.** `cd`, `echo`,
`test`, `[`, and in most shells `kill`, are resolved by the shell without
consulting `PATH`. No shim can be placed in front of them, `command_not_found`
hooks do not fire, and nothing in the user's experience indicates that these
commands were not governed. A shim set therefore covers *external commands
only*, and the boundary between covered and uncovered is invisible at the point
of use. This is not a defect to be fixed later; it is a permanent property of
the mechanism, and any claim of the form "every command you type is governed" is
false.

**Aliases and shell functions** may take precedence over PATH lookup.
`command -p` uses the shell's default search path and can bypass activated
shims; plain `command cat` still searches the active PATH. `builtin` selects a
shell builtin, whose execution is already outside shim coverage.

The honest reading:

- Against a **human**, the shim provides an audit trail, a consistent policy
  view, and protection against mistakes -- not enforcement.
- Against an **agent or CI job whose `PATH` and filesystem are controlled
  externally** (a container, a runner image, a sandbox profile), the shim is a
  real boundary for external commands, because bypassing it requires naming an
  absolute path the environment does not provide.

---

## 3. Stage 1 -- `apexe run`

```
apexe run <MODULE_ID> --input <JSON> [--dry-run] [--acl <PATH>] [--as <CALLER_ID>]
```

Captured output, JSON result, one module per invocation. Deliberately excluded
from Stage 1: raw/passthrough I/O (§7), stdin (§8.1), and `--param k=v` sugar
(its type coercion against array, boolean and nested schema shapes is a separate
small design, and `--input` covers every case meanwhile).

### 3.1 Preflight, not `Module::preview()`

`--dry-run` must **not** be implemented on top of
[`CliModule::preview()`](../../src/module/cli_module.rs). That method returns
`None` for any module not annotated `destructive`, and never consults the ACL or
the path guard -- it exists to render an approval prompt, which is a different
question from "what would happen if I ran this".

Stage 1 introduces a distinct preflight path returning a `PreflightResult`:

| Field | Source |
|---|---|
| `module_id`, `binary_path` | registry |
| `argv` | `executor::build_argv` + `assemble_arguments` |
| `annotations` | `ModuleAnnotations` |
| `acl_decision` | the ACL, evaluated for the effective caller -- `allowed`, `denied`, or `no_acl_loaded` |
| `path_guard` | per path-typed argument: requested path, resolved path, verdict |
| `approval_required` | annotation plus gate configuration |

Preflight runs every governance check and stops before `execute_subprocess`.
This is the main reason Stage 1 is worth shipping on its own: it makes every
governance decision inspectable from a terminal, without standing up a server and driving it with an MCP client.

### 3.2 ACL default policy

[`install_acl`](../../src/module/registry.rs) attaches nothing when
`opts.acl_path` is `None`, so a local `local` principal has no effect unless an
ACL is actually loaded. The rule for `run`:

| Situation | Behaviour |
|---|---|
| `--acl <PATH>` given | Load it. Missing or malformed is a hard failure (existing `install_acl` semantics). |
| No flag, `<config_dir>/acl.yaml` exists | **Load it and enforce it.** |
| No flag, that file is malformed | **Fail closed.** A policy that exists and cannot be read is not the same as no policy. |
| No flag, that file is absent | Run without access control, and emit a `warn` to stderr saying so. |

Requiring an explicit `--acl` was rejected: shims are generated, so the path
would be baked into them, which is implicit loading with extra steps. Better to
make the implicit path explicit in documentation and loud when it is missing.

`apexe scan` already writes that ACL, so the default path is the file the user
was told to review.

### 3.3 `--as` is a simulation aid

`--as` may be combined **only with `--dry-run`**, and is rejected otherwise.
The local user can pass any value; permitting it on a real execution would make
it an entry point for asserting a principal the caller does not hold. Its
purpose is answering "what would CI's principal be allowed to do", and preflight
is where that question belongs. Audit records from a preflight mark the
principal as asserted, not authenticated.

---

## 4. Stage 2 -- argv resolution

[`src/adapter/argv.rs`](../../src/adapter/argv.rs) resolves native arguments to
a typed JSON object. The executor validates that object and reconstructs argv
using the existing argument builder. No unresolved token is forwarded and no
shell command string is executed.

Supported grammar:

- Declared short and long aliases, separate values and `--name=value`.
- Bundled short flags and attached short values only when explicitly enabled.
- Repeated scalar options use the last value; repeatable arrays preserve value order.
- Optional-value options use a bare boolean or an attached value.
- Declared positional order, including a variadic operand before required final operands.
- `--` only when the schema declares end-of-options support.
- Original `command_path` subcommands, including hyphenated names and declared
  global options before the subcommand.

Unknown flags, unsupported value types, ambiguous operand grammar and
order-sensitive combinations are refused. Existing mutual-exclusion validation
also applies. This is semantic normalization of the declared grammar; byte-for-byte
argv preservation is not promised.

Routes are checked against fresh bindings on every call. The most specific
matching command_path wins. Once a subcommand is selected, its parse or policy
refusal is final; the shim cannot fall back to a root module with broader ACL.
Missing or changed routes refuse rather than executing an ungoverned native binary.

## 5. Installation and eligibility

### 5.1 Explicit overlay contract

Eligible modules must be registered after prefix/tag filtering, target an
available absolute CLI binary outside the shim directories, carry current scan
format metadata and have a matching authoritative overlay variant. Overlay
format 1.1 adds these reviewed facts:

| Field | Requirement |
|---|---|
| `cross_flag_ordering` | Must explicitly be `false`; absent or `true` excludes the module |
| `short_flag_bundling` | `true` enables bundles and attached short values; absence does not enable them |
| `streaming_stdin_safe` | `true` allows inherited stdin; otherwise stdin is null |

The metadata stamp and schema capabilities must agree. Scan format 3 carries
these capabilities plus literal flag aliases. Existing format 1.0 overlays still
scan normally, but do not gain capabilities implicitly. Re-scan after updating
an overlay. See [overlay verification](../overlays.md) for the evidence procedure.

Authoritative coverage alone does not prove order independence. Verification
on the real BSD ls installation found 16 previously unrecorded conflicting flag
pairs among 967 nonconflicting pair probes. Those conflicts are now symmetric
in the companion corpus before ls declares order independence for its remaining
admitted grammar. grep remains excluded because its include/exclude patterns
have cross-flag order dependence. No claim is made for all legacy overlays.

### 5.2 Managed entries

`apexe env --enable` plans the complete command set and validates startup blocks
before installing links.
Different binaries claiming one name, duplicate routes, and unowned existing
files are hard failures. Owned links may be updated; omitted commands remain
refusal tombstones until an explicit uninstall. Removed modules never cause a
silent fallback to the real command.

The default directory is `<config_dir>/bin`. Unix entries are symbolic links to
the apexe binary; Windows entries are hard links. A versioned `.apexe-shims.json`
manifest stores the binding source, filter, explicit ACL override and command
routes. Files are staged before publication; temporary files are removed by RAII.
`--disable` verifies ownership before removing links, the manifest and owned
shell startup blocks, preserving unrelated directory contents and configuration.

### 5.3 Activation

```bash
APEXE_OVERLAY_DIRS=/path/to/cli-permissions/overlays apexe scan cat head ls --no-cache
apexe env --enable
# Open a new terminal before running native commands.
cat sample.txt | head -n1
ls -la
apexe env --disable
```

`apexe env` prints activation for an existing installation. `--shell sh` serves
sh/bash/zsh; fish and powershell syntax are available too. Repeated activation
keeps one copy of the chosen shim directory. Diagnostics go to stderr so shell
evaluation receives only activation syntax.

`apexe env --enable` installs shims and adds an idempotent managed startup block, detecting the shell
from `SHELL` unless `--shell` is explicit. zsh uses `$ZDOTDIR/.zshrc` or
`~/.zshrc`; bash configures both `.bashrc` and its selected login profile; fish
uses `$XDG_CONFIG_HOME/fish/config.fish` or `~/.config/fish/config.fish`; sh uses
the login-only `.profile`. `--rc-file` overrides the selected file and is
required for the target PowerShell host's `$PROFILE`.

Separate initialization is unnecessary in the enable/reload/disable workflow. New shells
activate only when the executable and shim manifest exist; activation computes
PATH dynamically and does not modify an already running parent shell. Existing
startup text, permissions and symlinks are preserved. A versioned
`.apexe-shell-init.json` records exact owned blocks, which are updated without
duplication and removed by disable. Modified or unowned blocks are refused.
Startup updates are planned before writes, published atomically per file and
rolled back if a later update fails. Refresh the command set with `--reload`;
normal command use needs no repeated setup. Reload preserves the installed
binding directory/pattern, explicit ACL and prefix/tag filters by default,
without rewriting startup files or detecting the shell. Explicit overrides
change those settings; `--shell` or `--rc-file` also updates startup configuration.
When the manifest is absent, reload automatically enables with current settings.
Repeated disable succeeds when already disabled.
`eval "$(apexe env)"` remains available for immediate current-session activation.

### 5.4 PATH isolation

Scanner lookup, scanner probes and wrapped subprocesses remove the default
shim directory, `APEXE_SHIM_DIR` and manifest-bearing shim directories from
PATH. Scanning after activation still finds the real binary. Explicit targets
inside managed shim directories are rejected to prevent recursion. Environment
scrubbing remains active for both captured and passthrough execution.

## 6. Governance and caller identity

Shims use the fixed local caller chain `local`. They load an explicit manifest
ACL override or the current default ACL using the same rules as `apexe run`.
A present but unreadable or malformed policy fails closed; an absent policy
produces a warning. Prefix/tag filters gate execution as well as installation.

Approval remains enabled. This local surface has no MCP client capable of
answering elicitation, so approval-required calls are refused. No arbitrary
caller override is accepted. Route and argument-resolution refusals also create
audit records without storing raw argument tokens.

## 7. Execution I/O

The executor supports two explicit modes:

```rust
pub enum ExecutionIo {
    Captured,
    Passthrough { stdin: StdinMode },
}

pub enum StdinMode {
    Null,
    Inherit,
}
```

Captured remains the default for MCP, A2A and `apexe run`. Passthrough inherits
stdout and stderr; stdin inherits only with the positive streaming capability.
It neither buffers nor emits a JSON envelope. The internal module result records
`io`, `exit_code`, `trace_id` and `duration_ms`; it has no captured stdout/stderr
fields. The registered output schema matches the chosen mode.

A child exit code reaches the shell unchanged. POSIX signal termination uses
`128 + signal`; child SIGPIPE handling is restored to its default so an upstream
pipeline stage exits quietly when its reader closes. The configured timeout
still bounds execution and kill-on-drop cleans up the direct child. The captured
output byte cap does not apply to inherited output. No JSON-output flag is
silently appended to a passthrough command.

## 8. Security boundaries

### 8.1 Streaming stdin

Inherited stdin is not schema validated, path checked, buffered or recorded in
the audit trail. It therefore needs an explicit reviewed per-module allowlist.
The verified pilot enables stdin for cat/head and keeps ls stdin null. A module
without the assertion cannot request inherited stdin through the I/O API.

### 8.2 Shell coverage

Builtins, aliases, functions and absolute paths can bypass PATH lookup.
For a human this is an audit and mistake guard. Enforcement requires externally
controlled PATH and filesystem access. The [threat model §5.11](../threat-model.md#511-local-execution-and-the-remaining-stage-2-boundaries)
documents these permanent limits with the shipped feature.

## 9. Verification and examples

| Contract | Tests |
|---|---|
| Captured execution, preflight, simulation-only caller, default/malformed ACL, approval, path checks, failed exits and stale bindings | `tests/local_execution.rs` (13 cases) |
| Native streams, stdin allowlist, bundles, separators, signals, timeouts, routing, ownership, collisions, new-terminal activation, reload configuration/overrides/auto-enable, disable, PATH isolation and audit | `tests/native_shims.rs` (37 cases on macOS) |
| Shell detection, startup selection, custom paths, idempotency, ownership, symlinks, permissions, CLI compatibility and exact startup cleanup | `tests/shell_initialization.rs` (25 cases) |
| Startup publication rollback and original CRLF preservation | `src/cli/shell_init.rs` unit tests |
| Typed values, repeats, aliases, variadics and original command paths | `src/adapter/argv.rs` unit tests |
| Managed directories and symlink aliases removed from PATH | `src/runtime_path.rs` unit tests |
| Runnable captured and native examples | `tests/examples_smoke.rs` |

The [local execution example](../../examples/local_execution/README.md) scans
real tools into a temporary HOME, drives preflight/captured execution, checks
ACL and typed-path refusals, and optionally initializes a temporary startup
file, loads shims automatically in a new bash, runs a real pipeline and removes
owned shims and startup blocks. It uses reviewed overlays for native eligibility; it does not
manufacture verified capabilities. Without a path annotation the captured demo
reports that path coverage is unavailable rather than treating a missing file
error as governance success.

```bash
cargo build --all-features
python3 examples/local_execution/demo.py --apexe target/debug/apexe
python3 examples/local_execution/demo.py --apexe target/debug/apexe \
  --corpus ../cli-permissions/overlays --shims
```

Native example smoke coverage requires the reviewed BSD corpus on macOS.
The full suite also covers existing transport and captured execution paths.
Stage 1/2 are implemented within the declared eligibility set; the session
daemon in §11 remains deferred. No all-platform validation is inferred from
the macOS native pilot.

## 10. Delivery record

A safe readonly pilot was run on 2026-10-12 against the installed macOS 26.3
arm64 BSD cat/head/ls tools and their reviewed companion overlays. It used
three bindings, the generated readonly ACL and audit logging enabled. All six
latency cases matched native stdout and exit status; two warmups preceded 25
samples per case.

The representative synthetic grammar pilot publishes all 47 argv cases in the
script: 10 cat usages, 5 head usages, 20 ordinary ls usages, and both orders of
6 ls conflict pairs. Every native invocation succeeded. All 35 ordinary cases
were admitted with equivalent stdout/status (0/35 refused); all 12 order-conflict
cases were refused. The overall equally weighted refusal rate is 12/47, or
25.53%. This measures the explicit pilot corpus, not real-user frequency.
[Recorded results](../benchmarks/shims-macos-2026-10-12.json) include every argv
and observed status.
Undeclared flags are tested separately and always refused. grep remains outside
the installed set until its ordering semantics can be represented safely.

| Case | Native median ms | Shim median ms | Shim p95 ms |
|---|---:|---:|---:|
| cat file | 2.153 | 17.649 | 19.069 |
| cat stdin | 2.301 | 17.446 | 20.969 |
| cat bundle | 2.215 | 18.032 | 19.686 |
| head file | 2.225 | 17.731 | 21.112 |
| head stdin | 2.189 | 17.185 | 19.279 |
| ls bundle | 4.205 | 20.605 | 25.28 |

Reproduce with the isolated-home pilot script:

```bash
cargo build --release --all-features
python3 scripts/benchmark_shims.py --apexe target/release/apexe \
  --corpus ../cli-permissions/overlays --samples 25
```

These measurements establish startup overhead and the refusal rate of the
published synthetic corpus, not a general refusal rate or corpus-wide equivalence. The script
never benchmarks destructive operations. The approximate 15 ms overhead is
acceptable for this bounded pilot; larger binding sets and frequent short calls
need their own measurements.

## 11. Deferred work

A session daemon may amortize binding/ACL loading if measured workloads justify
its extra lifecycle and policy-reload complexity. It is not required by the
current subprocess shim contract. Expanding the eligible overlay set requires
real-installation grammar evidence, conflict coverage and representative native
comparison tests. Shell builtins remain outside PATH shims.
