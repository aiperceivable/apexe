# Sandbox delegation

**Status: specification. Nothing in this document is implemented yet.**
It exists so that the implementation, the tests and the operator-facing
documentation are written against one definition instead of three.

---

## 1. What apexe is, and is not

| apexe is | apexe is not |
|---|---|
| A **policy** layer: who may call what (`acl.yaml`), what a call may touch (`paths:` in `config.yaml`) | A **security executor**: it does not implement syscall filtering, kernel sandboxes or containers |
| An **interpreter**: it translates a policy into the arguments of a mature enforcement backend | A **container runtime**: it does not build images, manage networks or map users |
| An **auditor**: it records every decision *and* how the call was actually confined | The **sandbox** itself |

The boundary in one sentence: **apexe decides and delegates; the kernel and the
backend enforce.**

---

## 2. Why delegation

The path guard checks the paths that appear in a call's arguments. A command
whose paths are computed by the code it runs is outside that check by
construction:

```
python3 -c 'import os; os.remove("/etc/hosts")'      # no path argument reaches the guard
sed -n 'w /etc/hosts' file                           # the write lives in the script
```

Denying `rm` therefore constrains *which tool is spelled*, not *what can be
done*. Closing that gap needs enforcement at a layer that sees the syscalls, and
that layer belongs to the operating system. Writing one here would make apexe a
security product with its own vulnerability surface; translating into an
existing one keeps apexe small and keeps the security argument upstream.

There is a second reason, specific to this project: the `paths:` vocabulary was
taken from systemd (`ReadOnlyPaths=`, `ReadWritePaths=`, `InaccessiblePaths=`),
which means the Linux backend needs almost no translation and therefore almost
no translation bugs.

---

## 3. Assurance levels

Every installation is in exactly one of two states, and the operator must be
able to see which:

| Level | Meaning | What it promises |
|---|---|---|
| `enforcing` | A backend refuses out-of-policy filesystem operations at the kernel boundary | The policy holds regardless of what the command does internally |
| `advisory` | Only argument-declared paths are checked | A command that computes its own paths can escape the policy |

`enforcing` follows SELinux's vocabulary for the state in which policy is
actually applied, rather than merely reported.

Rules:

1. `advisory` is never described as protection. Every surface that reports the
   level says so in those words.
2. apexe does **not** claim `enforcing` for a policy the backend cannot express.
   A backend declares its capabilities (§5.1); if the requested policy exceeds
   them, the outcome is `refused`, not a quiet downgrade.
3. With `sandbox.required`, a missing backend is a refusal to run, not a
   warning. The execution modes are `off`, `required`, and `auto`; `auto` selects
   only among enforcing, capable backends and refuses when none is available.
   `advisory` is a diagnostic state reported by `sandbox check`, not a run mode;
   no call is silently downgraded from `enforcing` to `advisory`.

Where the level is surfaced: `apexe paths`, `apexe sandbox check`,
`apexe exec --dry-run`, `apexe list --diagnostics`, and every audit record.

---

## 4. Terminology

Names are chosen to match the systems that own the concept, because a reader who
already knows systemd or SELinux should not have to learn a second word for the
same thing.

| Term | Meaning | Precedent |
|---|---|---|
| **sandbox** | The mechanism that confines a child process to a filesystem policy | systemd "sandboxing", App Sandbox, Flatpak |
| **backend** | The program that enforces it (`systemd-run`, `bwrap`, a container runtime) | — |
| **enforcement** | Whether the confinement is real (`enforcing`) or argument-only (`advisory`) | SELinux `enforcing` |
| **probe** | The availability and capability check a backend performs before use | — |
| **capability** | A path permission the backend can actually express | — |
| **translate** | Policy + argv → backend invocation | — |
| **confined invocation** | The argv apexe executes instead of the original one | — |

Rejected candidates: `isolation` (implies network/namespace isolation, which
this does not provide), `jail` (FreeBSD-specific and carries a chroot
connotation), `guard` (already means the path guard, which is a different layer).

---

## 5. Backend contract

A backend is a small, stateless adapter. It answers four questions and produces
one launch spec, not necessarily one argv:

```text
probe()      -> Probe
capabilities() -> &'static [Capability]
translate(policy: &PathPolicy, invocation: &Invocation) -> Result<LaunchSpec, Refusal>
execute(launch: LaunchSpec) -> Result<SandboxChild, Refusal>
wait() / kill() / cleanup() -> exit status, signal, teardown
eforcement() -> Enforcement
```

`LaunchSpec` keeps the shape of the execution surface explicit:

```rust
enum LaunchSpec {
    Command(CommandSpec),      // systemd-run, bwrap, sandbox-exec, nono
    Container(ContainerSpec),  // Docker, Apple container
    MicroVm(MicroVmSpec),      // microsandbox, Firecracker
    Remote(RemoteSpec),        // E2B, OpenSandbox, CubeSandbox later
}
```

`SandboxChild` owns the running child or session: stdin, stdout, stderr, wait,
kill and cleanup. The executor drives this through its one spawn point; a
backend does not get a second execution surface.

### 5.0 Adapter protocol

The built-in backends implement the in-process contract above. Third-party
adapters use a versioned out-of-process protocol, added only after the built-in
pair has passed the conformance suite. The protocol is a driver contract, not a
sandbox policy language:

```text
describe()   -> name / version / contract_version / platforms
probe()      -> availability, enforcement, reason
prepare()    -> effective PathPolicy + Invocation -> LaunchSpec | Refusal
execute()    -> LaunchSpec -> SandboxChild
execute/wait/kill/cleanup, stdout/stderr/stdin streaming, timeout, exit status
```

The adapter declares `capabilities`; if the effective policy needs a capability
it does not have, it refuses rather than dropping an entry. The adapter is a
**trusted** component: it can run commands, so installation is signed/allowlisted
and the operator selects it. A probe is evidence, not proof; `sandbox check` and
the audit record say which backend actually ran. Built-ins do not pay the
out-of-process hop; the protocol exists for languages and distributions apexe
does not compile in.

### 5.1 `probe`

```json
{
  "backend": "systemd-run",
  "available": true,
  "enforcement": "enforcing",
  "reason": null,
  "probed_at": "2026-11-14T12:00:00Z"
}
```

`reason` is required whenever `available` is false or `enforcement` is
`advisory`, and must name the fact, not the symptom: `"systemd-run not found in
PATH"`, `"user manager unavailable (no DBUS session)"`,
`"backend cannot express no_access on macOS 26"`.

### 5.2 `capabilities`

```rust
enum Capability {
    ReadOnly,        // may read, may not write
    ReadWrite,       // may read and write
    NoAccess,        // neither
    WriteOnly,       // may write, may not read
    DenyWithinGrant, // can refuse a subtree that a broader grant covers
}
```

`DenyWithinGrant` is the load-bearing one. An allow-list backend (Landlock) can
grant rights but cannot refuse a subtree inside a granted root; a mount-based or
rule-based backend (bwrap, systemd, seatbelt) can. A policy that needs
`no_access: [~/.ssh]` while `/` is writable requires `DenyWithinGrant`. If the
selected backend lacks it, the outcome is `refused` with a reason naming the
capability, and the operator chooses between a different backend and a narrowed
policy. **Silently dropping the entry would be the exact failure this contract
exists to prevent.**

### 5.3 `translate`

Inputs: the effective `PathPolicy`, and the invocation (argv, working directory,
and the caller id). Output: a `LaunchSpec`, or a `Refusal` carrying
`{ capability | probe }` and a human reason. Translation must be a pure function
of its inputs — same policy and invocation produce the same launch spec byte for
byte. That is what makes it testable without a kernel (§ `docs/sandbox-testing.md`,
layer 1).

### 5.4 Fail-closed rules

| Situation | `sandbox.off` | `sandbox.auto` | `sandbox.required` |
|---|---|---|---|
| Backend available, policy expressible | run confined | run confined | run confined |
| Backend unavailable | run unconfined; the operator chose `off` | **refuse to run** | **refuse to run** |
| Backend available, policy not expressible | run unconfined; the operator chose `off` | **refuse to run** | **refuse to run** |
| Translation error | run unconfined; the operator chose `off` | **refuse to run** | **refuse to run** |

`auto` refuses a policy the backend cannot express because a policy the operator
wrote and the backend silently ignores is worse than a refusal: it teaches the
operator that the entry works. `off` is an explicit no-sandbox choice; it is not
the automatic consequence of a missing backend.

---

## 6. Selection: which calls are confined

Confinement is **opt-in per module**, never a blanket wrapper. Two reasons, one
of them measurable.

*Performance.* A wrapper costs a process launch and, for a container, a
filesystem view: 100–300 ms per call. A shim runs on every command a developer
types and must stay in the tens of milliseconds, so `ls`, `cat`, `grep` and the
rest are never wrapped.

*Meaning.* Wrapping only helps where the path guard cannot see the write. That is
a property of the command, not of the machine: a command whose paths are in argv
is already covered by `paths:`, and wrapping it adds cost without adding
assurance.

```yaml
sandbox:
  backend: auto                      # auto | systemd-run | bwrap | sandbox-exec | ...
  default: off                       # off | required | auto (per §5.4)
  modules:                           # confine exactly these, by module id
    - cli.python3
    - cli.node
    - cli.sh
    - cli.sed
    - cli.awk
  external: container                # unauthenticated serve/a2a callers
```

`backend: auto` is platform-aware. It prefers an OS-provided local sandbox over a
container/VM, and never chooses an `advisory` path for a selected module.

Rules:

1. **An empty `modules:` list means nothing is confined.** The default install
   confines nothing; `apexe sandbox recommend` (planned) prints the modules the
   corpus believes compute their own paths, so the operator can start from a
   reviewed list rather than a guess.
2. `modules:` names **module ids**, the same vocabulary `acl.yaml` targets use,
   so a reader does not learn a second addressing scheme.
3. Selection does not change *who may call* — that stays in `acl.yaml`. It
   changes *what the call may touch*.
4. A module selected here pays the wrapper cost on **every** call: `sed -n` is
   confined exactly like `sed -i`, because apexe cannot read the script. That
   trade-off is the operator's to accept, and the audit record (§9) shows which
   calls paid it.
5. `external:` overrides `modules:` for callers that are not `local`: an
   unauthenticated client runs code we did not choose, so it is confined even
   when the module is not listed.

### 6.1 Default backends by platform

The default pair requires no third-party installation; optional backends are
selected only when present and capable.

| Platform | Default | Third-party install | Fallback |
|---|---|---|---|
| macOS | `sandbox-exec` (OS; deprecated; canonical-path probe) | none | `nono` (private `sandbox_init`), Docker, Apple `container`, microVM, Linux VM |
| Linux + systemd | `systemd-run` (OS; needs a user manager/DBus) | none | `bwrap` (the `bubblewrap` package) when installed; Landlock/seccomp in-process; no backend → `required` refuses |
| Linux without systemd | `bwrap` (if installed) | `bubblewrap` | Landlock/seccomp; no backend → `required` refuses |
| external/untrusted | container or VM | Docker/Apple container/microVM | E2B/OpenSandbox/CubeSandbox later |

`bwrap` is a third-party package, not part of the base OS. It is preferred when
installed because its mount namespace expresses `read_only`, `read_write`,
`no_access`, and `DenyWithinGrant` directly; `systemd-run` is the zero-install
Linux default when its user manager is available. A missing backend is never a
silent host fallback: `required` refuses, `auto` refuses, and `off` runs only
because the operator explicitly chose it.

## 7. Backend matrix

| Backend | Platform | Capabilities | Enforcement | Status |
|---|---|---|---|---|
| `systemd-run --property=…` | Linux + systemd | all four + `DenyWithinGrant` | `enforcing` | planned, first Linux zero-install |
| `bwrap` | Linux (unprivileged userns) | all four, via bind mounts | `enforcing` | planned, preferred when installed |
| `podman` / `docker` | Linux, macOS | all four, via mounts | `enforcing`, different semantics (§8) | optional, per-caller only |
| `sandbox-exec` (seatbelt) | macOS | all four, with canonical paths | `enforcing`, **deprecated** | candidate, first macOS local |
| `nono` (Seatbelt) | macOS | all four, via private `sandbox_init` | `enforcing` after a probe | candidate |
| `microsandbox` / Apple `container` | macOS | all four, via a Linux VM | `enforcing` | later |
| — | macOS, no backend above | — | `advisory` | fallback only; `required` refuses |

### macOS: deprecated, but not inert

The original version of this section reported `sandbox-exec` as inert. That was a
**false negative in the test**, not an inert sandbox. On macOS `/tmp` is a
symlink to `/private/tmp`; Seatbelt matches literal paths, so a rule for
`subpath "/tmp/sbtest"` does not cover a write whose resolved path is
`/private/tmp/sbtest`. With `(allow default)` the write succeeds for that
reason, not because the sandbox was bypassed.

The corrected test on macOS 26.3 (arm64):

```text
$ sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath "/private/tmp/sbtest"))' \
    /bin/sh -c 'echo x > /private/tmp/sbtest/f'
/bin/sh: /private/tmp/sbtest/f: Operation not permitted   # exit 1
```

A real home path and a deny-all write are blocked too; `(deny default)` blocks
execution of the child. Details and the full transcript are in
[`docs/sandbox-testing.md`](sandbox-testing.md) §4.

The market tools work because they handle this correctly: Gemini's
`resolveToRealPath` emits both the original and resolved paths, and `nono`
explicitly resolves `/tmp` to `/private/tmp` and emits both. A backend that uses
only the caller's spelling will produce false negatives exactly like our first
test.

`sandbox-exec` is **deprecated**, and that is the real risk. It is not inert on
macOS 26.3. The practical macOS order is:

1. `sandbox-exec` (or `nono`'s private `sandbox_init`) after a canonical-path
   probe, with a plan to migrate;
2. `microsandbox` or Apple `container` for a VM boundary;
3. a Linux VM/devcontainer when the whole host must be contained;
4. Docker Desktop as the already-installed container fallback.

An operator who needs enforcement can therefore use `sandbox-exec` today, but
should not build a long-term product on a deprecated API. `sandbox.required`
refuses when no backend passes its probe; `advisory` is only the diagnostic
level when none does, never a silent downgrade from an enforcing backend.

---

## 8. Container backends: different semantics, narrow scope

A container is not a path sandbox with extra steps. Confining a call in a
container changes what the call *is*:

| Difference | Consequence |
|---|---|
| The child sees the image's filesystem, not the host's | Tool drift: `python3`, `node`, `sed` are whatever the image ships |
| `$HOME`, the repository and tool caches must be bound in | Miss one and the tool breaks; bind too many and the confinement is decorative |
| Files are written as the container UID | Artefacts can end up owned by another user, breaking later `git` operations |
| Start-up costs 100–300 ms per call | Unusable for a shim, where `ls` itself must stay in the tens of milliseconds |
| Requires a daemon or socket | A new trust surface and a new dependency |

Therefore a container backend is **never** the blanket wrapper, and the selection
rules in §6 apply unchanged. It is selected per caller:

```yaml
sandbox:
  default: auto            # systemd-run / bwrap where available
  external: container      # unauthenticated serve/a2a callers — the ones that run code we did not choose
  # local stays auto: shims must not pay container start-up
```

The asymmetry is the point: `@external` runs code written by someone else,
which is exactly the case where "a path in argv" is a meaningless guarantee.

---

## 9. Audit

Two records, both in the existing `audit.jsonl`, so a reader can answer "what was
in force when this ran" without correlating files.

### 8.1 Startup record (`event: "sandbox_probe"`)

```json
{
  "event": "sandbox_probe",
  "backend": "systemd-run",
  "available": true,
  "enforcement": "enforcing",
  "capabilities": ["read_only", "read_write", "no_access", "write_only", "deny_within_grant"],
  "policy_digest": "sha256:9f2c…",
  "reason": null
}
```

### 8.2 Execution record (fields added to the existing `event: "execution"`)

| Field | Meaning |
|---|---|
| `sandbox_backend` | `systemd-run` \| `bwrap` \| `sandbox-exec` \| `nono` \| `container` \| `microvm` \| `remote` \| `none` |
| `sandbox_enforcement` | `enforcing` \| `advisory` |
| `sandbox_outcome` | `applied` \| `unavailable` \| `refused` \| `degraded` |
| `sandbox_policy_digest` | Hash of the translated invocation, so the policy in force is provable without logging the policy per call |
| `sandbox_reason` | Required when the outcome is not `applied`; names the capability or probe fact |

`policy_digest` is over the *translated invocation*, not the YAML: a whitespace
change to `config.yaml` must not look like a policy change in the audit, while a
changed mount or rule surface must.

---

## 10. What apexe will not do

Each item names where the responsibility actually lives, so a future request can
be answered without relitigating this document.

| Not doing | Where it lives instead |
|---|---|
| Kernel sandbox implementation (Landlock/seatbelt calls in-process) | The backend; `landlock`/`bwrap`/`systemd-run` |
| Syscall filtering, eBPF, seccomp profiles | The backend or the OS |
| Container images, registries, runtime management | The container runtime |
| Network isolation, DNS policy, user ID mapping | The container runtime, or a VM |
| Interpreting a program's code to predict what it will touch | Nothing can; that is what `enforcing` backends are for |

---

## 11. Testing

See `docs/sandbox-testing.md` for the five-layer plan, the two probes every
backend must pass (a denied write must fail *and* a permitted write must
succeed), the canonical-path requirement (`/tmp` → `/private/tmp`), the
adapter conformance suite, and which layers a given platform can verify. A
third-party adapter is not accepted until it passes the same suite:
`describe/probe/prepare/execute/wait/kill/cleanup`, `DenyWithinGrant`,
`write_only` refusal, no silent drop, selection, audit, and fail-closed.
