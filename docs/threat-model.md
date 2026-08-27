# Threat model

What `apexe` stops, how it stops it, and — the longer half of this document —
what it does not stop.

Read this before you rely on it for anything. A governance tool that overstates
its reach is worse than none, because it moves work from "I am careful here" to
"the tool handles it".

---

## 1. Position

`apexe` is **not a sandbox.** It does not isolate the filesystem, restrict
network access, drop capabilities, or filter syscalls. A wrapped tool that is
permitted to run, runs with the full privileges of the `apexe` process.

`apexe` is an **execution boundary**: a place where a call must present a
declared contract before the subprocess is spawned, and where the decision to
permit or refuse it is recorded.

The two are complementary and you should want both. The recommended deployment
is `apexe` **inside** a sandbox (`sandbox-runtime`, `bubblewrap`, a container):
the sandbox bounds what any escape can reach, `apexe` decides what should be
attempted and leaves the evidence. Neither substitutes for the other — a sandbox
cannot tell an authorised deletion from a catastrophic one, and an execution
boundary cannot contain a process that has already been permitted to run.

---

## 2. The problem being addressed

An agent asked to do ordinary work reaches for ordinary tools. The failure mode
that has been repeatedly documented in the field is not exotic: the agent takes
an irreversible action it was instructed not to take.

The instructive detail from the PocketOS incident (April 2026) is the
post-mortem finding that the agent acknowledged violating the destructive-command
rule in its own system prompt.

That generalises to a design premise:

> A constraint expressed in an instruction is a **request**. Only a constraint
> enforced at the point of execution is a **fact**.

Everything below follows from taking that literally.

---

## 3. Why command-string matching is the wrong enforcement layer

The common enforcement design is a pattern list over command strings: allow
`git status`, deny `rm -rf`. It fails as a category, not as an implementation,
because the set of strings that produce a given effect is open.

Four shapes of this have been reported publicly against various agent hosts. They
are listed as *classes of problem inherent to string matching*, not as defects of
any particular product — every one of them was fixed or mitigated where reported:

| Class | Shape |
|---|---|
| **Compound commands** | The matcher evaluates the first token; `a && b` presents `a` and runs both ([claude-code#36637](https://github.com/anthropics/claude-code/issues/36637)) |
| **Delegation** | A sub-agent or channel-served session is spawned on a path that skips the policy filter ([claude-code#40343](https://github.com/anthropics/claude-code/issues/40343), [zeroclaw#7063](https://github.com/zeroclaw-labs/zeroclaw/issues/7063)) |
| **Path aliasing** | `/proc/self/root/usr/bin/npx` resolves to the same binary and matches no deny pattern |
| **Approval fatigue** | Hundreds of prompts per session make review nominal, and the documented response is to disable prompting entirely |

The first three share a root cause: the policy is written against a *rendering*
of the intent rather than the intent. The fourth is a UX consequence of the same
thing — when a rule cannot distinguish `git status` from `rm -rf /`, it must ask
about both.

`apexe`'s response is to never be in that position: **it does not parse a command
line.** There is no string for a caller to vary.

---

## 4. What apexe actually enforces

Each row is implemented and test-covered. Source locations are given so the claim
can be checked rather than believed.

### 4.1 No shell on the execution path

`src/module/executor.rs` builds a `tokio::process::Command` and passes argv
straight to `execve`. No shell is spawned anywhere on the execution path. The
single `sh`/`zsh` invocation in the crate lives in `scanner::completion` and runs
a hardcoded constant script with no interpolation.

The consequence is structural: `;`, `|`, `&`, `$`, backtick and quotes are
ordinary bytes in an argument. They cannot start a command, open a pipe, or
expand a variable, because nothing is there to interpret them. Compound-command
injection is not mitigated — **it has no mechanism to exist**.

This is why `apexe` deliberately does *not* reject shell metacharacters in
argument values: doing so bought no safety and broke real use (`curl` could not
send a JSON body, no `jq` filter containing `|` could be passed). See the
`CONTROL_CHARS` documentation in `executor.rs` for the full reasoning, and
`test_build_arguments_allows_shell_metacharacters` for the guarantee.

### 4.2 The injection that does exist on an execve path

A value that reaches argv is read *positionally* by the wrapped tool's own
parser. A value of `--output=/etc/passwd` is indistinguishable from an option the
caller was never granted. This — not shell metacharacters — is the real injection
on this path.

`validate_argument_value` therefore rejects any value starting with `-`.

Two documented exemptions, both narrow:

- **`numeric`** — a value that arrived as a JSON *number*, so legitimate negative
  numbers work. A *string* `"-1"` is still rejected.
- **`separator_available`** — set only for a tool whose overlay states it honours
  the `--` end-of-options separator. After `--` the wrapped parser *cannot* read
  the value as an option, which is a stronger guarantee than refusing values that
  merely look like one.

### 4.3 Control characters that would corrupt the audit trail

`\0`, `\n`, `\r`, U+2028, U+2029 and U+0085 are rejected in any value. The
argument is log integrity, not execve safety: a caller who can plant one of these
decides where a log reader sees a line boundary. Tab, VT, FF and ESC are
deliberately *not* rejected — they are already neutralised on output by `tracing`'s
`Debug` rendering, and rejecting them would cost real values.

### 4.4 Fail-closed ACL — when one is loaded

`apexe scan` writes `~/.apexe/acl.yaml` with `default_effect: "deny"`. Modules
annotated `readonly` get an explicit allow rule; modules annotated `destructive`
get an explicit deny rule. Everything else — including everything the scanner
could not classify — falls through to deny.

The direction of that failure mode matters: an unclassified command is
**refused**, not permitted. See §5.2 for the case where it does not hold.

**"Fail-closed" describes the loaded policy, not apexe's default state.**
`install_acl` returns early when no `--acl` path is given, so a server started
without the flag enforces no access control at all. What *is* fail-closed about
the flag itself: passing `--acl` with a path that does not exist refuses to
start rather than continuing unguarded. See §5.3.

### 4.5 Environment scrubbing

Every subprocess runs with `env_clear()` and a base allowlist plus `LC_*` locale
variables. Secrets in the parent environment do not reach wrapped tools.

### 4.6 Resource bounds

Output is capped at 64 MiB per stream (`DEFAULT_MAX_OUTPUT_BYTES`). A timeout is
enforced with `kill_on_drop(true)`, so an expired timeout actually kills the
child rather than merely returning an error to the caller — covered by
`test_execute_subprocess_kills_the_child_when_the_timeout_elapses`.

### 4.7 Audit trail

`audit.jsonl` (mode `0600`) records executions, refusals, **and** ACL allow/deny
decisions, each carrying `caller_id` (the authenticated principal) and `trace_id`
in one timestamp format, so an allow-decision can be joined to the execution it
permitted.

It deliberately holds **no argument values**, hashed or otherwise. See §5.5 for
what that costs.

### 4.8 Path guard on path-valued arguments

`--acl` decides *whether a module may be called*. It cannot decide *what a call
may touch*: apcore's ACL matches on `callers` and `targets` (module ids), and
its condition keys — `identity_types`, `roles`, `max_call_depth`, `$or`, `$not`
— are all facts about the principal, never about an argument. So an ACL can say
"`cli.rm` is allowed" or "`cli.rm` is denied", and nothing in between.

The realistic policy is in between. `rm` is not a command to forbid outright;
`rm /etc` is a command to forbid. `src/governance/path_guard.rs` closes that
gap: every argument the input schema types as a filesystem path
(`x-apexe-path`, emitted by the schema builder for `ValueType::Path`) is
resolved and checked before it becomes argv.

**Two baselines, because reading and writing are different risks.** A single
list forced one answer onto two unrelated questions and got both wrong:
`cat /etc/hosts` was refused, which buys nothing — the file is world-readable
and the agent's own file-reading tool reaches it anyway (§5.7) — while
`cat ~/.ssh/id_rsa` and `rm -rf /etc` were refused for the same stated reason
despite being nothing alike. The split follows the risk:

| | System paths | Credential paths |
|---|---|---|
| Module annotated `readonly` | allowed | **refused** |
| Everything else | **refused** | **refused** |

- **System paths** — `/bin`, `/boot`, `/dev`, `/etc`, `/lib`, `/lib32`,
  `/lib64`, `/proc`, `/run`, `/sbin`, `/sys`, `/usr`, `/var`, plus `/System`,
  `/Library`, `/Applications`, `/private/etc`, `/private/var` on macOS.
  Destruction is the risk here; legibility is not, and refusing `ls /usr/bin`
  bought friction rather than safety.
- **Credential paths** — `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.kube`, `~/.docker`
  and `~/.apexe` under the invoking user's home, refused to readers and writers
  alike. Deleting a private key announces itself the next time the key is used;
  copying one into a model context leaves no trace at all, so the stricter
  treatment goes to the risk that is harder to notice. `~/.apexe` is on this
  list rather than the other because it holds the ACL and the audit trail
  governing the very call being made — a wrapped tool that can read the policy
  can plan around it.

The mode comes from the module's `readonly` annotation, and an unannotated
module is treated as a writer. That inherits §5.2's caveat: a **false
readonly** — a command whose name matches a readonly pattern but which
writes — gets the weaker of the two treatments here as well as an explicit
allow rule in the ACL. Both point at the same remedy, which is to correct the
classification with an overlay.

`config.yaml`'s `additional_denied_paths` appends to the **credential** list,
so a configured path binds readers too: an operator naming a path explicitly is
asserting it is sensitive, and the stronger of the two readings is the one that
cannot be wrong in the dangerous direction.

`allowed_paths` is the one setting that goes the other way, and it is **empty
unless an operator writes it**. It exists because the alternative was worse:
an agent that legitimately has to write `/etc/nginx/conf.d` otherwise had to be
handed that tooling outside apexe altogether, which drops the audit trail and
the ACL along with the path check — §5.1 in miniature, reached by way of a
control that was too rigid to use. A narrow declared carve-out keeps the call
inside the governed path.

Nothing validates that a carve-out is *wise*. Naming `/etc`, or a credential
directory, is honoured — overruling an explicit instruction would put the guard
in the business of second-guessing the person who deployed it, and an operator
staring at a config file that appears to do nothing is its own failure mode.
What the guard does instead is refuse to be quiet: every carve-out is logged at
startup, and one that opens a whole system location or exposes credentials is
logged at `warn`. **Two properties are worth stating plainly**: a carve-out
grants everything beneath it, and a carve-out over a service's configuration
directory grants that service's behaviour — write access to
`/etc/nginx/conf.d` is enough to add a `proxy_pass` and redirect traffic,
without touching anything else under `/etc`.

Carve-outs and denials share one specificity ladder, so a subtree can be opened
and part of it fenced off again. An exact tie is broken by whose carve-out it
is: a configured `allowed_paths` entry wins, because it is an instruction; the
derived temp-directory exemption loses, because nobody asked for it and an
exact overlap there is a coincidence — which is what stops a `TMPDIR` pointed
at `~/.ssh` from reading a key out.

**Comparison happens on the resolved path, never the supplied string.** Three
transformations sit between what a caller sends and what the kernel acts on,
and skipping any one makes the guard decorative:

1. **Relative paths** are joined to the guard's root — which is also what
   `Command::current_dir` is set to, so the guard and the child cannot disagree
   about where `../../etc/passwd` lands. Before this, the child inherited
   apexe's own working directory, and the meaning of every relative argument
   depended on where the operator happened to launch `apexe serve` from.
2. **Symlinks** are followed. `/tmp/x -> /etc` makes `rm -rf /tmp/x/` a request
   to delete `/etc`, and no string comparison on `/tmp/x` sees it. The longest
   existing prefix is canonicalized, so a destination that does not exist yet
   (a `mkdir -p` target, a `cp` destination) is still judged.
3. **`..` components** are folded — but only across the non-existent tail, and
   only *after* the symlinks ahead of them are resolved. `std::path::absolute`
   preserves `..` because POSIX requires it, and collapsing it lexically first
   is wrong: `/tmp/link/..` is the parent of link's target, not `/tmp`.

**The check runs in both directions, and only one of them binds a reader.**
*Containment* — the path sits inside a protected location, `/etc/passwd` under
`/etc` — applies to both modes, and is what refuses `cat ~/.ssh/id_rsa`.
*Ancestry* — the path sits above one — applies to writers only, because it
exists for the recursive operation that takes the protected directory with it:
`rm -rf /` never names `/etc` and destroys it regardless. Holding a reader to
it would refuse `ls ~` and `ls /` on the grounds that a home directory contains
`.ssh`, which is most of what a reader legitimately does; the resulting gap is
§5.8's third bullet. Matching is by whole path component either way, so
`/etcetera` does not collide with `/etc`.

**One carve-out exists, and it is compiled in.** On macOS the per-user
temporary directory *is* `/var/folders/<x>/<y>/T`, so guarding `/var` would
otherwise refuse every use of the directory whose purpose is being written to.
`config.yaml` cannot extend the carve-out list, and a `TMPDIR` that equals or
contains a *system* location is discarded rather than honoured. The credential
list is deliberately not weighed there: a home directory under `$TMPDIR` is
ordinary in a sandbox, and counting it would void the carve-out and re-arm
`/var` across the whole temp directory. Specificity already refuses a
credential path nested inside a carve-out. Where a denied
entry and the carve-out both cover a path, the more specific one decides; a tie
is a refusal.

A refusal is `ACL_DENIED`, carrying the requested path, the resolved path and
the protected location that matched. The resolved path is in the message
because the requested one frequently does not look protected — that is the
whole point of resolving — and a caller told only "denied" cannot tell a
mistake from a misconfiguration. Being a governance code rather than an input
code also keeps it out of circuit-breaker health (`breaker.rs` excludes
governance refusals), so a caller repeatedly probing `/etc` cannot open a
circuit and deny `cli.rm` to everyone else.

Unlike `--acl` and `--enable-approval`, this is **on by default and has no
off switch** (§5.3 does not apply to it).

---

## 5. What apexe does not stop

This section is the reason the document exists.

### 5.1 An agent that has a shell does not need apexe

`apexe` governs the tools it exposes. If the same agent host also offers a
general `Bash`/`Shell`/`Terminal` tool, the agent can bypass every control here
by simply not using the governed path.

**`apexe` raises the floor only if the ungoverned floor is removed.** Wrapping
`git` while leaving a shell tool enabled buys you an audit trail of the calls the
agent chose to make politely. Deploy it together with host-level restriction of
raw shell access, or inside a sandbox where raw execution is unavailable.

This is the single most important limitation on this page.

### 5.2 Annotations are name-based heuristics, and can be wrong

`src/adapter/annotations.rs` classifies commands by matching their names against
fixed pattern lists (`READONLY_PATTERNS`, `DESTRUCTIVE_PATTERNS`), with flag
escalation (`APPROVAL_FLAGS`: `--force`, `-f`, `--hard`, `--recursive`, …).

The two error directions are not symmetric:

- **Missed destructive** (a destructive command whose name matches nothing) →
  falls through to `default_effect: deny`. Safe direction.
- **False readonly** (a command whose name matches a readonly pattern but which
  has side effects) → gets an **explicit allow rule**. **Unsafe direction.**

Concrete example: `find` matches `READONLY_PATTERNS`, but `find … -delete` is
destructive. Any tool whose subcommand name understates its effect is in this
class. (The shipped `find` overlay corrects it to `destructive: true`, which is
what overlays are for.) The sharpest instance is §5.9: `env` matched the same
list while being a general-purpose command executor.

The classification also feeds §4.8. A false readonly gets the *weaker* of the
path guard's two treatments as well as an ACL allow, so one wrong annotation
now moves two boundaries.

**Review the generated ACL before serving it.** It is a starting point produced
by a heuristic, not a policy you have approved. Use overlays to correct
classifications for tools you rely on.

### 5.3 Access control and approval are both opt-in

Neither `--acl` nor `--enable-approval` is on by default. A bare `apexe serve`
gives you the contract (schema validation), the isolation (§4.1, §4.2, §4.5,
§4.6) and the audit trail — and **no policy**: every registered module is
callable by every caller.

This is the gap most likely to produce a false sense of safety, because the scan
*does* generate a deny-by-default ACL and then does not apply it for you. Wire
both flags in explicitly:

```bash
apexe serve --acl ~/.apexe/acl.yaml --enable-approval
```

### 5.3.1 The approval gate additionally needs client support

Without `--enable-approval`, `requires_approval` annotations are recorded but
never enforced by a human decision.

With it, the prompt is delivered as an MCP **elicitation**, so a client that
declared no elicitation support cannot be prompted and the call is refused. This
is deliberate (refusing beats silently proceeding) but means the gate's behaviour
depends on the connected client.

There is **no approval gate on the A2A surface at all** — A2A has no elicitation
channel, and `apexe a2a` rejects the flag rather than accepting it and doing
nothing.

For a boundary that needs no human and works on every surface, use `--acl`.

### 5.4 Approval is not an ACL condition

`require_approval` is not a registered apcore ACL condition key. Approval gating
happens in the Executor's `ApprovalHandler`, not in ACL evaluation. Writing it as
an ACL condition would silently never match — the rule would fall through rather
than deny. Do not attempt it in a hand-written policy file.

### 5.5 The audit trail cannot tell you what was deleted

Because no argument values are recorded, `audit.jsonl` answers "which principal
invoked which module, when, with what outcome" — and not "which file did it
remove". This is a privacy-and-tamper tradeoff, taken deliberately. If your
forensic requirement is the latter, `apexe`'s log alone will not meet it.

### 5.6 Scan quality is bounded by the tool's own help output

Schemas are derived deterministically from `--help`, man pages and shell
completions. A tool with incomplete, inconsistent or absent help produces an
incomplete schema. Nothing here infers intent — that is the point, but it means
coverage varies by tool. Curated overlays exist for this reason.

### 5.7 Not covered at all

- Anything the agent does **without** invoking a wrapped tool: direct HTTP calls,
  file writes through its own edit tool, code it executes in a sandbox
- Prompt injection reaching the agent through tool *output* (`apexe` bounds size,
  not content)
- Compromise of the `apexe` binary, its binding files, or its ACL file. Binding
  files are checked against `BINDING_INJECTION_CHARS` as a tamper signal, which
  is a signal and not a defence
- Anything that happens after the wrapped tool starts running

### 5.8 The path guard sees types, not intent

The guard checks the arguments the input schema types as paths, at the strength
the module's `readonly` annotation selects. Five limits follow directly, and
none of them is a bug to be fixed later:

- **A schema that does not mark a value as a path leaves it unchecked.** The
  marker comes from the scanner's `ValueType::Path` inference or from an
  overlay's `"type": "path"`. A tool whose help text does not reveal that an
  option takes a filename yields an unmarked value, and the guard has nothing
  to act on. Guessing is not the alternative — treating every string as a path
  would refuse `grep /etc/hosts logfile`, whose first argument is a pattern.
- **It bounds arguments, not what the tool does with them.** `find . -delete`,
  a config file that names a target, a tool that reads paths from stdin, and
  any shell-out the wrapped tool performs itself are all past the boundary.
  The last bullet of §5.7 — nothing after the wrapped tool starts running —
  still holds in full.
- **The mode is per module, not per argument.** `cp /etc/hosts ~/backup` is
  refused even though `/etc/hosts` is only being *read*, because `cp` is not a
  `readonly` module and both its operands are judged at the module's strength.
  Distinguishing a source operand from a destination would need a
  per-argument direction the scanner cannot infer and no overlay declares. The
  failure direction is a refusal rather than a permit, so this is friction
  rather than exposure.
- **A recursive read rooted above a credential directory is not caught.**
  Ancestry binds writers only (§4.8), so `grep -r … /` and `ls -R ~` pass the
  guard even though a recursive read can reach `~/.ssh`. The alternative was
  refusing `ls ~` outright, which is most of what a read-only tool is for. The
  guard sees paths, not the `-r` that changes what a path means — closing this
  needs flag semantics it does not have.
- **Glob expansion never happens** (§4.1: no shell), so `rm /etc/*` reaches the
  guard as a single literal path and is refused. That is the safe direction,
  but it is a consequence of having no shell, not something the guard verifies.

The guard raises the floor for the case that actually recurs — an agent
pointing a legitimate destructive command at a system directory. It is not a
sandbox, and §5.1 and §6 still apply in full.

### 5.9 A tool that runs other tools defeats every name-based control

`env`, `xargs`, `nice`, `timeout`, `nohup`, `chroot`, `sudo` and their kin do
not merely *have* dangerous options — running a caller-supplied command **is**
their interface. Everything apexe decides from a command's name is therefore
decided about the wrapper rather than about what it will run.

This was live, and it is worth stating exactly because the shape recurs:

- `env`'s name is in `READONLY_PATTERNS`, so the generated ACL gave it an
  explicit **allow** (§5.2's "false readonly", reached by the worst possible
  name).
- BSD `env` takes `-S <string>`, which splits the string into argv and executes
  it. `-S` is typed `string`, not `path`, so §4.8's guard never inspected it —
  correctly, by its own contract, since nothing there is a path.
- `cli.env` with `{"S": "rm -rf /etc"}` therefore passed both controls.

`EXEC_WRAPPER_TOOLS` in `src/adapter/annotations.rs` now classifies these as
`destructive`, which flips the generated ACL from allow to deny and makes the
path guard judge their arguments as a writer's. Two overlays that carried
`destructive: false` for `xargs` were corrected in the same change, since a
curated overlay replaces the scan result and would otherwise have overridden
the classification silently.

**That fix is a floor, not a solution.** No static analysis can decide what
argv a caller-supplied string will become, so the guard cannot inspect the
command inside `-S` or the one after `xargs`. Treat these tools as equivalent
to handing the agent a shell (§5.1):

```yaml
# In your ACL, ahead of any broad allow rule:
- callers: ["*"]
  targets: ["cli.env", "cli.xargs", "cli.sudo", "cli.timeout", "cli.nice"]
  effect: deny
  description: "Command executors — argv is caller-controlled"
```

Better still, do not scan them into the registry at all. A wrapped `env` buys
nothing that wrapping the underlying tool directly does not, and it costs every
control in §4.



---

## 6. Recommended deployment

1. Run `apexe` inside a sandbox. It governs; the sandbox contains.
2. Remove or restrict raw shell tools in the agent host (§5.1) — otherwise the
   governed path is optional, and optional controls are decorative.
3. Review the generated ACL (§5.2) — especially every rule in the readonly allow
   list — and then **pass it explicitly**. It is not applied for you (§5.3):

   ```bash
   apexe serve --acl ~/.apexe/acl.yaml --enable-approval
   ```

4. Confirm your MCP client supports elicitation, or the approval gate refuses
   rather than prompts (§5.3.1). On A2A there is no gate at all — use the ACL.
   The path guard (§4.8) needs no wiring: it is on by default on every surface.
   Extend it with `additional_denied_paths` if this deployment has data
   directories worth the same protection.
5. Keep `--auth` at its default on HTTP transports. A non-loopback bind with no
   credential refuses to start unless explicitly acknowledged; do not acknowledge
   it casually.
6. Ship `audit.jsonl` somewhere the agent cannot write, and know what it does and
   does not contain (§5.5).
7. Re-review the ACL after every `apexe scan`. A rescan merges freshly generated
   rules into the existing file, so new commands arrive with heuristic
   classifications you have not read yet (§5.2).
8. Do not wrap command executors — `env`, `xargs`, `sudo`, `timeout`, `nice`
   and their kin (§5.9). They are classified `destructive` so the generated ACL
   denies them, but the durable fix is to keep them out of the registry: argv
   is caller-controlled there, and no control in §4 can see inside it.

---

## 7. Reporting

Report security issues privately — see [SECURITY.md](../SECURITY.md), which
turns §4 and §5 of this document into an explicit in-scope / out-of-scope list.

The short version: if you find a way to reach `execve` with argv the contract did
not authorise, that is a vulnerability and we want it. If you find that a
heuristic misclassified a command, that is §5.2 working as documented — an
overlay fix, not a vulnerability.
