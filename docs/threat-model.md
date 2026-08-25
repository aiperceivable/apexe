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
class.

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
5. Keep `--auth` at its default on HTTP transports. A non-loopback bind with no
   credential refuses to start unless explicitly acknowledged; do not acknowledge
   it casually.
6. Ship `audit.jsonl` somewhere the agent cannot write, and know what it does and
   does not contain (§5.5).
7. Re-review the ACL after every `apexe scan`. A rescan merges freshly generated
   rules into the existing file, so new commands arrive with heuristic
   classifications you have not read yet (§5.2).

---

## 7. Reporting

Report security issues privately — see [SECURITY.md](../SECURITY.md), which
turns §4 and §5 of this document into an explicit in-scope / out-of-scope list.

The short version: if you find a way to reach `execve` with argv the contract did
not authorise, that is a vulnerability and we want it. If you find that a
heuristic misclassified a command, that is §5.2 working as documented — an
overlay fix, not a vulnerability.
