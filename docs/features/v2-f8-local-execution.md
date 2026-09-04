# F8: Local Execution -- governed CLI invocation from the terminal

| Field | Value |
|---|---|
| **Feature ID** | F8 |
| **Priority** | P1 (Surface) |
| **Status** | Design. Stage 1 ready for implementation; Stage 2 scoped to the §5.1 overlay set and blocked on §5.4, §7 and §8.1, plus the binding-staleness prerequisite in §10. |
| **Dependencies** | F2 (Module Executor), F5 (Governance) |
| **Depended On By** | None yet |
| **New Files** | `src/cli/run.rs`, `src/cli/preflight.rs`, `src/cli/shim.rs`, `src/adapter/argv.rs` |
| **Modified Files** | `src/cli/mod.rs`, `src/module/executor.rs`, `src/module/cli_module.rs`, `src/module/registry.rs` |
| **Estimated LOC** | ~950 (Stage 1 ~300, Stage 2 ~650) |
| **Estimated Tests** | ~80 |

---

## 1. Purpose

Give the local terminal the same governed execution path that MCP and A2A
callers already get. Today `Executor::call` is reachable only over a transport:
`apexe serve` and `apexe a2a` are the only two consumers of
[`build_executor`](../../src/module/registry.rs). A shell script, a CI step, a
Claude Code hook, or a human wanting to see what the ACL would decide has no
way in.

Two surfaces, in order:

1. **`apexe run <module_id>`** -- structured input, one module per invocation,
   captured output. The minimal base.
2. **`apexe env` + PATH shims** -- the wrapped tool invoked by its own name and
   its own argv syntax, so composition (pipes, `&&`, existing scripts) comes
   from the user's shell.

Stage 1 reuses the executor as it stands. **Stage 2 does not** -- it requires a
new I/O mode that reaches into `CliModule::execute` and `execute_subprocess`
(§7), a new PATH isolation contract (§5.4), a per-module stdin capability
(§8.1), and a declared bundling grammar (§4.4, §5.1). Those are design work, not
wiring. Stage 2 is also **scoped**: it applies to the modules eligible under
§5.1, not to everything a scan produced.

---

## 2. Motivation

### 2.1 Why the structured surface alone is not enough

`apexe` deliberately does not parse a command line, so a purely structured
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

With `~/.apexe/bin` prepended to `PATH`:

```bash
ls -l /tmp | grep ERROR | head -20
```

Three ACL decisions, three environment-scrubbed subprocesses, three path-guard
evaluations, and an audit trail -- without one character of what the user typed
changing. Under a plain shell that pipeline is a single `sh -c` string, and an
ACL keyed on module ids cannot see into it at all. The shim does not *reduce*
governance relative to a shell pipeline; it is the only arrangement in which a
pipeline is governed per stage.

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

**Aliases, shell functions and `command`/`builtin` prefixes** override or bypass
`PATH` resolution the same way, though unlike builtins these are at least the
user's own doing.

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
governance decision inspectable from a terminal, which today requires standing
up a server and driving it with an MCP client.

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

### 4.1 What the safety argument does and does not claim

The claim that holds: **argv is resolved into a schema-validated input object,
and the argv that executes is rebuilt from that object** by `build_argv`. The
ACL decision, the path guard evaluation and the executed argv all derive from
one validated structure, so there is no path on which the thing judged and the
thing run diverge. Command-string matching -- inspect a command line, decide
whether to allow it -- does not happen.

The claim that does **not** hold, and which the previous draft of this document
asserted incorrectly: that the user's argv and the executed argv are identical.
They are not. `build_argv` partitions arguments into leading flags, leading
operands, trailing flags and trailing operands and emits them in that fixed
order -- its own comment names this "the contract". What F8 can promise is
therefore **normalized semantic equivalence within a supported subset of argv
grammar**, not byte-level round-tripping.

That distinction is not pedantic. A user types a command, a *different* command
runs, and if the difference is ever semantic rather than cosmetic the shim has
silently changed what the user asked for. Safety and fidelity are separate
properties and only the first one is structurally guaranteed.

### 4.2 Which grammars are reversible

Decomposing the risk gives a usable rule rather than a blanket "reject
order-sensitive tools":

| Construct | Status |
|---|---|
| Scalar flag repeated (`-l -l`, `--color=a --color=b`) | **Resolvable.** Last-wins is settled at resolution time; the object holds one value. |
| Array-typed property (`-e A -e B`, `find` expressions) | **Resolvable.** Element order survives -- `build_argv` iterates the group with `flat_map` and preserves it. |
| Relative order *across different flags* (`find -name a -o -name b`, interleaved `-e`/`-f`) | **Not resolvable.** A JSON object has no cross-key ordering. This is the only genuinely lossy case. |

**Rule: a tool whose grammar depends on cross-flag ordering is not eligible for
a shim.** It must be marked in the schema or overlay and refused at shim
generation time, not silently reordered at call time. Note that today's schemas
already dodge this for the worst offender by modelling `find`'s entire
expression as a single ordered array property -- the eligible/ineligible line
follows the modelling, so it can be checked mechanically.

### 4.3 Unresolved tokens: refuse, no bypass (BLOCKING)

A scan is bounded by the tool's own help output (`docs/threat-model.md` §5.6),
so real schemas will be incomplete. When a shim meets a token the schema does not
declare, Stage 2 **refuses**, and the error names the tool, the token, and the
`apexe scan` or overlay edit that would cover it.

The `--allow-unresolved` escape hatch proposed in the previous draft is
**withdrawn**. It cannot be built without either bypassing schema validation and
the path guard, or introducing a second execution path carrying "validated
parameters plus opaque argv" -- which the executor does not have and which would
void §4.1's guarantee for exactly the calls least likely to have been modelled.
A governance surface with a documented transparent bypass is a pass-through
wearing a label.

The cost is real and is the reason this is the blocking item: a flag `--help`
never mentions breaks a command the user runs daily, and the user removes the
shim from `PATH`. The mitigation is coverage and error quality, not a bypass. If
a controlled escape is ever unavoidable, it should be an out-of-band operator
action with its own audit event type, never a flag on the hot path.

Validating this is what Stage 2's initial scope is for (§10).

### 4.4 Other parsing hazards

| Hazard | Note |
|---|---|
| Short-flag bundling (`-la`) | The scan cannot infer it, so an overlay declares it (`short_flag_bundling`; §5.1). Absent that declaration it stays an unresolved token, i.e. a refusal -- which is most of why eligibility is scoped to overlay-covered commands, since `ls -la` and `rm -rf` are not edge cases to measure but the first thing anyone types. |
| `--opt=value` vs `--opt value` | Both map to the same property. |
| `--` end-of-options | Everything after it is positional, including `-`-leading values. |
| `-`-leading values | `validate_argument_value` rejects these unless the schema declares `--` support. `grep -- -foo` must survive by setting the marker, not by working around the check. |
| Variant divergence | The resolved binary decides the grammar; `/bin/ls` and a Homebrew `gls` are different modules and must not share a parser table. |

---

## 5. Shim mechanics

### 5.1 Generation belongs to the installer, not to `scan`

`apexe scan` converts tools and writes bindings, the ACL and skills; it never
builds a registry, so it cannot apply `ModuleFilter` or `filter_available`.
Shim generation therefore lives in `apexe env --install` (or a dedicated
installer), which builds the registry and emits one shim per eligible module.
Eligibility excludes: modules whose binary no longer resolves, modules excluded
by `--tags`/`--prefix`, and modules ineligible under §4.2.

A shim whose module is no longer registered must **fail loudly**, never fall
through to the real binary -- silent fall-through would make the boundary depend
on the freshness of a directory nobody inspects. Name collisions between two
scanned tools, and overwrite policy on re-install, must be decided explicitly
rather than by last-writer-wins.

#### Eligibility is overlay coverage, not an annotation

The exclusions above say what cannot be shimmed. The positive rule is the more
consequential half, and the obvious candidate for it is wrong.

That candidate is "shim the commands that can hurt you" -- the modules annotated
`destructive` or `open_world`, leaving readers to run natively at full speed. It
fails in three separate ways:

- **It under-covers.** The path guard refuses a credential path to a `readonly`
  module too: `cat ~/.ssh/id_rsa`, `grep -r key ~/.aws`, `ls ~/.gnupg`. The
  threat model ranks that leak *above* deletion (`docs/threat-model.md` §4.8: a
  deleted key announces itself, a copied one does not), so selecting by
  annotation drops the entire exfiltration class -- the half of the guard that
  exists for the risk harder to notice.
- **It buys nothing.** The expensive parts of Stage 2 do not disappear along
  with the readers. `curl` and `wget` exist to stream; `git push` and `git clone`
  report progress on stderr; `ssh` needs a TTY. All three are `open_world`, and
  all three require §7's passthrough. `xargs` is `destructive` *and* a stdin
  tool, so §8.1 is required as well. Nothing is saved.
- **It selects for the wrong property.** `git` has no overlay: its ~142 modules
  are derived from man pages, which makes it the tool where §4.3's strict
  refusal is *most* likely to fire. An annotation-based rule puts it in the
  first shim set; `rm` and `chmod`, whose option sets are curated and closed,
  offer far less to learn from.

The rule that actually follows from §4.3 is **feasibility**, and it is already
recorded in the repository:

> A module is eligible when its command has a `mode: authoritative` overlay
> matching the detected variant.

`authoritative` means a human has asserted the option set is complete -- that is
the mode's entire definition, and the reason it is allowed to discard the scan
result. So a §4.3 refusal *inside this set* is not an unbounded risk; it is
evidence the overlay is wrong, which is a defect with an existing correction
procedure (`docs/overlays.md`) rather than a reason for a user to remove the
shim directory from `PATH`.

Two properties follow that the annotation rule cannot provide:

- **`short_flag_bundling` has somewhere to live.** §4.4 currently treats `-la`
  as an unresolved token because nothing records whether a tool accepts
  bundling. Under this rule the fact is declared in the overlay, next to
  `end_of_options` and `before_flags` -- where every other non-inferable grammar
  fact already sits. Outside the overlay set the refusal stands, which is
  correct: nobody has checked.
- **The set spans both risk classes.** The 21 shipped POSIX commands cover the
  destructive operations (`rm`, `mv`, `cp`, `chmod`, `xargs`) and the credential
  readers (`cat`, `grep`, `find`, `ls`, `head`, `tail`) at once.

`git`, `curl`, `wget`, `ssh` and every other scan-only tool are ineligible until
someone writes an overlay for them. That is the conservative direction, and the
one §4.3 already argues for on its own terms: the mitigation is coverage, not a
bypass.

### 5.2 Subcommand routing uses `command_path`, not the module id

The module id folds `-` to `_` (`git cat-file` becomes `cli.git.cat_file`) and
the hyphenated form is not recoverable from it, since `_` is a legal subcommand
character too. The converter already records the authoritative form in metadata
as `command_path: ["git", "cat-file"]`
([`converter.rs`](../../src/adapter/converter.rs)). Shim routing must read that
and must not re-derive command names from ids.

### 5.3 Recursion is prevented by absolute targets

Binding targets are absolute (`target: exec:///bin/cat`), resolved by
`which::which` at scan time ([`resolver.rs`](../../src/scanner/resolver.rs)) and
passed straight to `Command::new`. `PATH` is consulted once, at scan time, never
on the execution path.

**Invariant to test explicitly:** a shim on `PATH` shadowing a scanned tool's
name must not change which binary executes. If targets are ever resolved lazily,
this feature breaks as an infinite loop rather than an error.

### 5.4 PATH isolation (BLOCKING)

Absolute targets stop the *first-order* loop only. Two second-order paths remain
open, and both are live defects rather than hypotheticals:

**Re-scanning picks up shims.** `ToolResolver` resolves by `which::which`
against the ambient `PATH`. With shims installed, `apexe scan ls` resolves `ls`
to `~/.apexe/bin/ls` and stores *that* as the target -- producing a binding that
executes a shim, which executes a binding. Scanning must strip the shim
directory from the `PATH` it resolves against.

**Children inherit a `PATH` containing the shims.** `ENV_ALLOWLIST` passes
`PATH` through verbatim ([`executor.rs`](../../src/module/executor.rs)), so any
wrapped tool that spawns a helper internally -- `git` calling `less`, `ssh`,
`git-lfs`; anything invoking `$EDITOR` -- resolves it through the shim
directory. That re-enters apexe from inside a governed call, with a fresh
process, a fresh trace, and no relationship to the call in progress. The child's
`PATH` must have the shim directory removed.

Both need regression tests; neither is visible in ordinary use until it
misbehaves.

### 5.5 Working directory is already correct

`PathGuard::from_env` takes its root from `std::env::current_dir()`
([`path_guard.rs`](../../src/governance/path_guard.rs)), and `spawn_isolated`
sets the child's `current_dir` to that same root. Under `apexe serve` this is
the server's launch directory; under a shim it is where the user is standing.
Both are correct, and in both the guard and the child agree on what a relative
argument means. No change needed -- documented because it looks like a defect
(`ls` appearing to list the wrong directory) and is not.

---

## 6. Governance semantics

### 6.1 Caller identity

ACL rules match on `callers`; over a transport the id comes from
authentication, locally there is none. `run` uses a fixed `local` principal that
the generated ACL can name. See §3.2 for whether an ACL is loaded at all and
§3.3 for `--as`.

### 6.2 What applies unconditionally

Independent of ACL configuration, a local call keeps environment scrubbing,
no-shell argv, the path guard (on by default, no off switch), the timeout with
`kill_on_drop`, and the audit trail. That set -- not the ACL -- is what the
local surface delivers to a human user.

### 6.3 Audit volume

A pipeline of three shims does not produce three audit records. The audit file
carries three record kinds -- `acl_decision`, `execution`, `refusal`
([`audit.rs`](../../src/governance/audit.rs)) -- and with an ACL loaded each
stage emits an `acl_decision` *and* an `execution`, so three stages produce six
records correlated by `trace_id`. Sibling processes in a shell pipeline cannot
share a trace id, so the six form three pairs, not one chain. Documentation must
not promise pipeline-level correlation that the process model cannot deliver;
Stage 3's daemon is the only thing that would change this.

---

## 7. Execution I/O (Stage 2)

Raw passthrough is **not** a change to `spawn_isolated` alone. The current model
is captured-only end to end: `spawn_isolated` pipes both streams,
`collect_output` drains them into `SubprocessOutput { stdout: String, ... }`,
and `CliModule::execute` builds a JSON result from that. Inheriting stdout would
leave `SubprocessOutput.stdout` empty while `CliModule` still returned a
successful JSON body describing output it never saw.

The contract Stage 2 needs:

```rust
pub enum ExecutionIo {
    /// Current behaviour: both streams piped, drained, capped, returned as JSON.
    Captured,
    /// stdout (and stderr) inherited; nothing buffered; result carries status only.
    Passthrough,
}
```

threaded through `CliModule::execute` -> `execute_subprocess` ->
`spawn_isolated`, with a distinct result shape for `Passthrough` so no caller
can read a stdout field that was never populated. Five consequences:

1. **stdout** is `Stdio::inherit()`. Logging already goes to stderr
   ([`main.rs`](../../src/main.rs)), so the stdout channel is clean -- the part
   hardest to retrofit is already right.

2. **stdin** stays `Stdio::null()` unless the module is explicitly cleared for
   it (§8.1).

3. **SIGPIPE.** Rust's runtime sets `SIGPIPE` to `SIG_IGN` before `main` and a
   forked child inherits that disposition, so in `ls | head` the upstream child
   gets `EPIPE` and reports a write error instead of dying quietly as it would
   under a shell. The child must restore `SIG_DFL` via `pre_exec` before
   `execve`.

4. **Exit codes.** `Module::execute` returns `Value`, so the child's exit code
   must reach the CLI layer through the `Passthrough` result shape, and the CLI
   must `exit()` with it. Without this, `set -o pipefail` and `&&` see success
   on every failed stage.

5. **Output cap.** `DEFAULT_MAX_OUTPUT_BYTES` bounds *apexe's* memory, and
   passthrough does not buffer, so the cap does not apply -- but the risk it
   covered does not disappear, it moves. Back-pressure becomes the downstream
   reader's, and an unbounded producer with a fast reader can fill a disk or
   flood a terminal. What still bounds a passthrough call is the timeout with
   `kill_on_drop`, and nothing else. The audit record's output size becomes a
   streaming count or is omitted; it must not report a cap that was not applied.

Composition itself needs nothing further: the `|` belongs to the user's shell
and moves bytes only, and apexe invokes no shell.

---

## 8. Threat model impact

Four additions required in `docs/threat-model.md` before Stage 2 ships.

### 8.1 stdin is an ungoverned input channel -- allowlist, not denylist

Every control in §4 acts on argv. An inherited stdin is not schema validated,
not path checked and not recorded. For `cat` that is inconsequential; for any
tool that reads a program from stdin it is a complete bypass of argv-level
governance -- the same class as `docs/threat-model.md` §5.9 ("A tool that runs other tools defeats
every name-based control") by a different route.

A denylist cannot work. `EXEC_WRAPPER_TOOLS`
([`annotations.rs`](../../src/adapter/annotations.rs)) is a private
name-matching heuristic, and the interpreter surface is open-ended: `python -`,
`awk`, `perl`, `sed -f -`, `sh -s`, `gdb -x`, tools honouring a config read from
stdin, and git subcommands reached through hooks.

**The contract is an allowlist.** stdin defaults to `null` for every module.
Passthrough stdin requires the module to be marked `streaming_stdin_safe` in its
schema or a curated overlay -- a human assertion, subject to the overlay rules
in `CLAUDE.md`, not an inference. A module without the mark cannot receive
stdin, and requesting it is an error rather than a downgrade.

### 8.2 Shim coverage is partial and the gap is silent

§2.3: builtins are never intercepted, and nothing at the point of use
distinguishes a governed command from an ungoverned one. Belongs in §5 as its
own subsection.

### 8.3 The shim is bypassable by absolute path

§2.3. Belongs in §5.

### 8.4 `--as` asserts, it does not authenticate

§3.3. Constrained to `--dry-run`, but the constraint must be documented rather
than only enforced.

---

## 9. Test scenarios

### 9.1 `apexe run` (Stage 1)

- `test_run_rejects_unknown_module_id`
- `test_run_validates_input_against_schema`
- `test_run_dry_run_does_not_execute`
- `test_run_dry_run_reports_acl_decision_for_non_destructive_module`
- `test_run_dry_run_reports_path_guard_verdict`
- `test_run_loads_default_acl_when_present`
- `test_run_fails_closed_on_malformed_default_acl`
- `test_run_warns_when_no_acl_is_loaded`
- `test_run_rejects_as_without_dry_run`
- `test_run_writes_logs_to_stderr_not_stdout`

### 9.2 argv resolution (Stage 2)

- `test_resolve_argv_maps_long_flag_with_equals_and_space_form`
- `test_resolve_argv_applies_last_wins_to_scalar_property`
- `test_resolve_argv_preserves_element_order_in_array_property`
- `test_resolve_argv_refuses_module_with_cross_flag_ordering`
- `test_resolve_argv_treats_tokens_after_double_dash_as_positional`
- `test_resolve_argv_preserves_leading_dash_value_after_double_dash`
- `test_resolve_argv_refuses_undeclared_flag_with_actionable_message`
- `test_resolve_argv_rebuilt_argv_matches_validated_input`
- `test_shim_routes_subcommand_via_command_path_metadata`
- `test_resolve_argv_expands_a_bundled_short_group_when_the_overlay_declares_it`
- `test_resolve_argv_reads_the_remainder_as_a_value_for_a_valued_short_flag`
- `test_resolve_argv_refuses_a_bundled_group_when_bundling_is_undeclared`

### 9.2.1 Shim eligibility (Stage 2, §5.1)

The criterion is what keeps §4.3's refusal rate at zero inside the pilot, so it
needs to fail loudly rather than degrade.

- `test_shim_generation_admits_a_command_with_an_authoritative_overlay`
- `test_shim_generation_excludes_a_scan_only_command` -- `git` today
- `test_shim_generation_excludes_a_command_whose_overlay_matches_another_variant`
- `test_every_shipped_overlay_declares_short_flag_bundling` -- a data guard on
  `overlays/`, in the style of the existing overlay guards in
  `tests/scanner_integration.rs`: an eligible command with no declared bundling
  grammar would refuse `-la` at runtime, which is the failure the criterion
  exists to prevent

### 9.3 PATH isolation (Stage 2, §5.4)

- `test_scan_ignores_shim_directory_when_resolving_a_tool`
- `test_child_environment_path_excludes_the_shim_directory`
- `test_shim_on_path_does_not_shadow_the_resolved_binary`
- `test_shim_fails_loudly_when_module_no_longer_registered`
- `test_env_is_idempotent_when_already_on_path`

### 9.4 Execution I/O (Stage 2, §7)

- `test_passthrough_result_carries_no_stdout_field`
- `test_passthrough_propagates_child_exit_code_to_process_exit`
- `test_passthrough_child_receives_default_sigpipe_disposition`
- `test_passthrough_upstream_exits_quietly_when_downstream_closes`
- `test_stdin_is_null_for_a_module_not_marked_streaming_stdin_safe`
- `test_stdin_passthrough_requires_explicit_module_mark`
- `test_captured_mode_behaviour_is_unchanged`

---

## 10. Delivery plan

**Prerequisite -- binding staleness detection.** Not part of F8, and blocking
for everything F8 proposes to measure. A binding carries no scan-format version
that apexe reads: `spec_version: 1.0` is apcore-toolkit's *file structure*
version and is identical on a binding written today and one written before the
path guard existed. So an installation scanned before 0.7.0 serves modules with
no `x-apexe-path`, no `x-apexe-escalates` and no `x-sensitive`, and the path
guard, the approval gate and log redaction all evaluate against nothing --
silently, with the only available diagnosis being the manual's suggestion to
`grep -L x-sensitive` the modules directory by hand. A Stage 2 pilot on such an
installation measures 0.5.0's behaviour and reports it as 0.7.0's. `YamlOutput`
must record the scan-format version and `build_executor` must warn per stale
module before any number below is worth reading.

**Stage 1 -- ship independently.** `--input`, JSON output, the §3.1 preflight
path, the §3.2 ACL default semantics, `--as` constrained to `--dry-run`. No raw
I/O, no stdin, no `--param`. Nothing here touches the executor's I/O model.

**Stage 2 -- a bounded pilot over the overlay set.** Three contracts settle
before code: `ExecutionIo` (§7), `streaming_stdin_safe` (§8.1), and
`short_flag_bundling` (§4.4, §5.1). Then argv resolution and shims, scoped to
the modules eligible under §5.1 -- the commands carrying a shipped
`authoritative` overlay, 21 of them today.

An earlier revision of this plan scoped the pilot to three hand-picked modules
(`cat`, `grep`, `head`). Widening it to the whole overlay set costs nothing in
refusal risk, because §5.1's rule admits a module precisely when its option
coverage has been asserted complete -- and it buys two measurements the narrow
scope cannot produce:

| Measurement | Why three readers are not enough |
|---|---|
| Per-invocation latency | `cat`, `grep` and `head` exercise one argv shape. `rm`, `cp` and `chmod` render path operands, where the guard canonicalizes and resolves symlinks per argument -- the case whose cost actually varies |
| The §7 I/O contract | All three are mid-pipeline readers. `rm` produces no output at all, `xargs` consumes stdin, `find` streams for minutes: three passthrough shapes the original scope never reaches |

What this pilot **cannot** answer is §4.3's refusal rate in general, since inside
the eligible set that rate is zero by construction. That question belongs to the
first scan-only tool anyone wants to shim, and it should be asked deliberately
against the worst case (`git`, ~142 modules derived from man pages) rather than
discovered by a user whose daily command stopped working.

**Stage 3 -- deferred.** A session daemon (`apexe shell`) removes per-call
startup cost and gives audit records a session-scoped trace id, which is also
the only way §6.3's pipeline correlation becomes possible. Do not build it
against a latency number nobody has measured.

---

## 11. Non-goals

- **Parsing a shell command line.** `apexe run 'ls | grep foo'` is the premise
  this project starts from and must not become a feature.
- **An internal pipeline executor.** Chaining stages with `Stdio::piped()` under
  one plan-level ACL decision is coherent for an agent with no shell processing
  data too large for its context. Different audience, different governance
  semantics; mixing it with F8 produces a surface that is neither.
- **Sandboxing.** Unchanged from `README.md`: apexe decides what is attempted
  and records what was; it does not contain what runs.

---

## 12. Open questions

1. **§4.3 strict refusal viability.** No longer blocking for Stage 2, which
   §5.1 scopes to a set where the rate is zero by construction. It becomes the
   entry condition for the *first scan-only tool*: run a deliberate trial
   against `git` before making any overlay-less tool eligible. Measured, not
   argued -- see §10.
2. **Shim latency.** Blocking for Stage 2, measured over the §5.1 set rather
   than over three readers.
3. **Name collisions and overwrite policy** when two scanned tools claim one
   command name (§5.1).
4. **Per-subcommand ACL granularity** -- is `cli.git.*` sufficient for the
   deployments this targets, or do shims need finer rules?
5. **Where `streaming_stdin_safe` lives** -- settled by §5.1: the overlay,
   alongside `short_flag_bundling` and every other non-inferable grammar fact.
   `apexe scan` infers neither. What remains open is whether the schema should
   *refuse* a `streaming_stdin_safe` overlay that leaves an interpreter flag
   (`-f`, `-e`, `-x`) unmarked, so the coupling in §8.1 is checked at authoring
   time rather than trusted.
