# Sandbox testing and integration

**Status: 0.15.0 implements the four built-in backends. The conformance
cases below run on a platform when that backend is available; an absent
backend is skipped with its probe reason, not failed.**
This document defines how a backend is accepted, and — just as important — which
claims can be verified on which machine. Every statement below is either a
command run in this repository or is marked *(spec)* / *(to verify)*.

Owner document: `docs/sandbox.md` (contract, assurance levels, selection rules).

---

## 1. What this repository can verify today

The commands are the repository's own entry points (`Makefile:20` defines
`check` as `fmt-check lint check-chars build test test-scripts test-examples`):

| Entry point | Command | Covers |
|---|---|---|
| Rust suite | `cargo test --all-features --locked` | unit + integration, including `tests/cli_runtime_validation.rs` |
| Runtime CLI gate | `cargo test --all-features --locked --test cli_runtime_validation` | drives `scripts/validate_cli.py` against the built binary: every CLI flag must have a passing scenario |
| Script suite | `python3 -m unittest discover -s scripts/tests -v` | the validator's own logic (`scripts/runtime_validation.py`) |
| Whole gate | `make check` | all of the above |

A sandbox backend must not add a fifth entry point. It adds scenarios to the
existing ones: translation is a unit test, backend availability is a unit test
with an injected probe, and execution is an integration test that skips by
platform (§6).

---

## 2. The five layers

Layers 1–3 and 5 run anywhere. Layer 4 needs a kernel that enforces, which is
the whole point of it.

| Layer | Asserts | Runs on | Entry point |
|---|---|---|---|
| 1. Translation | `paths:` + invocation → the backend's argv, byte for byte | any OS | `#[cfg(test)]` next to the translator |
| 2. Selection | the right backend is chosen for a platform, and an unavailable one is reported, not assumed | any OS; probe injected as a value | unit test |
| 3. Fail-closed | unavailable backend under `sandbox.required`, or an inexpressible policy, refuses *without spawning* a child | any OS | unit test asserting the spawn was not reached |
| 4. Enforcement | a denied write fails at the kernel; a permitted write succeeds | Linux (`bwrap`; `systemd-run` system/root only); macOS (`sandbox-exec`/`nono`) with canonical paths; VM/container for untrusted | integration test, `#[ignore]` or platform-skipped |
| 5. Audit | `sandbox_probe` at startup and the five `sandbox_*` fields per execution | any OS | integration test reading `audit.jsonl` |

Layer 4 is *the* acceptance criterion for a backend, and it has two halves.
Testing only the denied write passes an implementation that blocks everything;
testing only the permitted write passes one that blocks nothing.

---

## 3. The two probes

Both are literal commands, so a reviewer can run them by hand on any machine.

`bwrap` is the Linux local backend. `systemd-run --user` is **not** a path
sandbox: on Debian 12 / systemd 252 it accepts `ReadOnlyPaths`/`InaccessiblePaths`
and silently ignores them, so a service wrote into a `ReadOnlyPaths` directory.
Only a **system** `systemd-run` service (root) enforces those properties.

```bash
# Probe A — must fail. /etc is read_only in the shipped default policy.
bwrap --unshare-all --new-session --die-with-parent \
  --bind / / --ro-bind /etc /etc --ro-bind /usr /usr --ro-bind /bin /bin \
  -- /bin/sh -c 'echo x > /etc/sandbox-probe'
# expected: non-zero exit, EACCES / "Read-only file system"

# Probe B — must succeed. A writable directory inside the same policy.
dir="$(mktemp -d)"
bwrap --unshare-all --new-session --die-with-parent \
  --bind / / --ro-bind /etc /etc --bind "$dir" "$dir" \
  -- /bin/sh -c "echo x > '$dir/ok'"
# expected: exit 0, file present
```

A system `systemd-run` probe (root) may be used instead of bwrap:

```bash
sudo systemd-run --quiet --pipe --wait --collect \
  -p ReadOnlyPaths=/etc -p ReadOnlyPaths=/usr -p ReadOnlyPaths=/bin \
  -- /bin/sh -c 'echo x > /etc/sandbox-probe'
```

The probes must also be run **through apexe**, not only against the backend
directly, or the test proves the backend works and says nothing about apexe's
translation:

```bash
apexe exec --dry-run -- /bin/sh -c 'echo x > /etc/sandbox-probe'   # expect sandbox.backend = bwrap
apexe exec --dry-run -- /bin/sh -c 'echo x > /tmp/ok'             # expect sandbox.enforcement = enforcing
```

*(spec)* `exec --dry-run` gains a `sandbox: {backend, enforcement}` field; until
it lands, the equivalent check is `apexe sandbox check`.

The class of command this exists for:

```bash
# Probe C — the reason a sandbox is needed at all.
python3 -c 'open("/etc/sandbox-probe","w").write("x")'
# advisory: succeeds, and apexe cannot prevent it — no path appears in argv
# enforcing: fails at the kernel
```

Probe C is the one that distinguishes `enforcing` from `advisory` in practice. A
backend that passes A and B but not C has not achieved anything the path guard
already did.

---

## 4. macOS: enforcing, but only with canonical paths

The original note here reported `sandbox-exec` as inert. That was a **false
negative in the test**, not an inert sandbox. On macOS, `/tmp` is a symlink to
`/private/tmp`; Seatbelt matches literal paths, so a rule for `subpath
"/tmp/sbtest"` does not cover a write whose resolved path is
`/private/tmp/sbtest`. With `(allow default)` the write therefore succeeds.

The corrected tests on macOS 26.3 (arm64), 2026-11-15:

```text
# non-canonical: allowed (the false negative)
$ sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath "/tmp/sbtest"))' \
    /bin/sh -c 'echo x > /tmp/sbtest/f'
exit=0

# canonical: blocked by the kernel
$ sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath "/private/tmp/sbtest"))' \
    /bin/sh -c 'echo x > /private/tmp/sbtest/f2'
/bin/sh: /private/tmp/sbtest/f2: Operation not permitted
exit=1

# a real home path: blocked
$ sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath "/Users/tercel/sbtest"))' \
    /bin/sh -c 'echo x > /Users/tercel/sbtest/f3'
/bin/sh: /Users/tercel/sbtest/f3: Operation not permitted
exit=1

# deny every write: blocked
$ sandbox-exec -p '(version 1)(allow default)(deny file-write*)' \
    /bin/sh -c 'echo x > /tmp/sbtest/f4'
/bin/sh: /tmp/sbtest/f4: Operation not permitted
exit=1

# deny default: execution itself is refused
exit=71 (execvp: Operation not permitted)

# read deny with canonical path: blocked
$ sandbox-exec -p '(version 1)(allow default)(deny file-read* (subpath "/private/etc"))' \
    /bin/cat /private/etc/hosts
cat: /private/etc/hosts: Operation not permitted
exit=1
```

Consequences:

1. `sandbox-exec` is **deprecated but enforcing**. It is a candidate for the
   macOS local backend, with a deprecation risk and a mandatory canonical-path
   probe. It is not inert.
2. Layer 4 **can** be verified on macOS with `sandbox-exec` (Seatbelt) or the
   `nono` private `sandbox_init` path. The probe must use `realpath` and must
   test both the original and resolved spelling, exactly as the market tools
   do: Gemini's `resolveToRealPath` emits both, and `nono` explicitly resolves
   `/tmp` to `/private/tmp` and emits both.
3. `apexe sandbox check` reports `sandbox-exec` as `enforcing` only after the
   canonical probe passes. It reports `advisory` only when no backend passes or
   the selected backend cannot express the policy. The reason must name the
   failed probe.
4. `sandbox-exec` is deprecated; the long-term macOS options remain
   `nono`/Seatbelt via `sandbox_init`, `microsandbox`, Apple `container`, or a
   Linux VM/devcontainer. Deprecation is the risk, not current inertness.

Tooling present on this machine (checked, not assumed):

```
$ which sandbox-exec bwrap docker
/usr/bin/sandbox-exec
bwrap not found
/usr/local/bin/docker
```

## 5. Backend acceptance checklist

A new backend is accepted when every line below is demonstrated in the pull
request that adds it. "Demonstrated" means a test in this repository, not a
transcript in a description.

| # | Check | Where it is proved |
|---|---|---|
| 1 | `probe()` returns `available: false` with a factual `reason` when the program is absent from `PATH` | unit test, probe injected |
| 2 | `probe()` reports `advisory` only when the backend cannot enforce; a macOS `sandbox-exec`/`nono` probe must canonicalize paths and pass before reporting `enforcing` | unit test + the §4 probe |
| 3 | Declared `capabilities` match what the backend can express; a policy needing an absent capability yields `Refusal`, not a dropped entry | unit test with a policy containing `no_access` inside a granted root |
| 4 | `translate()` is deterministic: identical inputs produce identical argv | unit test, run twice and compared |
| 5 | `sandbox.required` + unavailable backend refuses **before** spawning | unit test asserting the spawn function was not called |
| 6 | Probes A and B both hold through `apexe exec` | integration test, Linux or macOS (`sandbox-exec`/`nono`) with canonical paths |
| 7 | Probe C fails under the backend, and the failure is attributable to the kernel (`EACCES`), not to a wrapper script exiting early | integration test, Linux or macOS (`sandbox-exec`/`nono`) with canonical paths |
| 8 | `audit.jsonl` carries `sandbox_probe` at startup and the five `sandbox_*` fields on every execution, with `sandbox_outcome` never silently `applied` when the backend was not used | integration test reading the file |
| 9 | A call the operator did not select is **not** wrapped, and a call they did select is (selection rules: `docs/sandbox.md` §6) | integration test asserting the backend never runs for `cli.ls` |

Items 1–5 and 8–9 are the ones that catch the realistic failures: a backend that
reports success, a policy entry silently dropped, a selection that wraps
everything, or an audit trail that cannot distinguish "confined" from "not".

### 5.1 Built-in default pair

The macOS default uses the OS-provided `sandbox-exec`; Linux local uses
`bwrap` (the `bubblewrap` package). `systemd-run` is system/root only: its user
manager ignores the path properties. Docker is optional.

| Platform | Default | Third-party install | Fallback |
|---|---|---|---|
| macOS | `sandbox-exec` (OS; deprecated; canonical-path probe) | none | `nono` (private `sandbox_init`), Docker, Apple `container`, microVM, Linux VM |
| Linux local | `bwrap` (the `bubblewrap` package) | `bubblewrap` | `systemd-run` system/root only; `--user` ignores path properties; no backend → `required` refuses |
| Linux without `bwrap` | none | `bubblewrap` needed | `systemd-run` system/root only; no backend → `required` refuses |
| external/untrusted | container or VM | Docker/Apple container/microVM | E2B/OpenSandbox/CubeSandbox later |

`bwrap` is a package, not part of the base OS. It is preferred when installed
because its mount namespace expresses `read_only`, `read_write`, `no_access`, and
`DenyWithinGrant` directly. `systemd-run --user` ignores the path properties;
only a **system** `systemd-run` service (root) is a path backend. `sandbox-exec`
is the zero-install macOS default, but it is deprecated and must use canonical
paths.

### 5.2 Adapter protocol conformance

The built-in backends implement the in-process contract from
[`docs/sandbox.md`](sandbox.md#5-backend-contract). A third-party adapter is not
accepted until it passes the same suite over the versioned out-of-process
protocol:

- `describe` reports `contract_version`, platform and enforcement capability;
- `probe` passes A/B/C and canonical paths (`/tmp` → `/private/tmp`);
- `prepare` returns a `LaunchSpec` or a `Refusal`, never a silently narrowed
  policy;
- `execute/wait/kill/cleanup` preserve stdin/stdout/stderr, exit status, signal
  and timeout;
- `capabilities` include `DenyWithinGrant` and `write_only` when the effective
  policy needs them, otherwise refuse;
- selection wraps only the operator-selected modules and `external` callers;
- audit carries backend, enforcement, outcome, policy digest and reason;
- `sandbox.required`/`auto` refuse before spawn when no backend is available;
- every surface still goes through the executor's one spawn point.

An adapter is a trusted component because it can run commands: installation is
signed/allowlisted and the operator selects it. A probe is evidence, not proof;
`sandbox check` and the audit record say which backend actually ran. Built-ins do
not pay the out-of-process hop; the protocol exists for other languages and
distributions.

### 5.3 Cases derived from the reference implementations

These are source-informed cases for the conformance suite; the source reading is
recorded in [`docs/ideas/sandbox-source-notes.md`](ideas/sandbox-source-notes.md).

- **Canonical paths:** `/tmp`/`/private/tmp`, `/etc`/`/private/etc`,
  `/var`/`/private/var`, a symlink target, a dangling symlink, `..`, `~`, a
  trailing slash, glob vs literal, file vs directory.
- **Dual spelling:** a write through the original spelling is blocked by a deny
  on the resolved spelling, and vice versa where the backend is literal.
- **`NoAccess`:** content and metadata (`stat`, `readdir`, `cat`) are denied.
  "Allow discovery, deny content" is not `NoAccess` and must not be accepted as
  an equivalent.
- **`WriteOnly`:** write succeeds, read fails, for a file and a directory.
- **`DenyWithinGrant`:** `read_only /var` + `read_write /var/log` +
  `no_access /var/log/secret`; a Landlock-only backend must refuse.
- **A/B/C:** denied write fails; permitted write succeeds; a computed path
  (`python3 -c 'open(...)'`) fails at the kernel.
- **bwrap:** `--unshare-all`, `--new-session`, `--die-with-parent`,
  `--ro-bind / /`, `--dev`, `--proc`, `--tmpfs /tmp`, `--cap-drop ALL`,
  `--args`; a missing deny path is refused or cleaned up.
- **Seatbelt:** `(deny default)`, read/write rule order,
  `file-write-unlink`/`file-write-create`, profile size, both spellings.
- **systemd-run:** `ReadOnlyPaths`/`ReadWritePaths`/`InaccessiblePaths` in a
  **system** service (root), `--pipe --wait`; do not use `--user` for paths.
- **selection:** `cli.ls` never wrapped; selected `cli.sed` wrapped; external
  caller gets the container backend.
- **audit:** backend, enforcement, outcome, policy digest, reason, probe record.
- **adapter:** `describe/probe/prepare/execute/wait/kill/cleanup`, signed/
  allowlisted, no silent drop, one spawn.

---

## 6. Skipping, not failing

A backend absent on the current machine is an expected state, not a failure.
The rule:

| Situation | Test behaviour |
|---|---|
| Backend not installed | skip, with the probe's `reason` in the skip message |
| No macOS backend passes its probe | skip layer 4; assert `apexe sandbox check` reports `advisory` **and** names the reason; `sandbox-exec`/`nono` with canonical paths are candidates, not presumed inert |
| Third-party adapter absent or fails conformance | not selected; `required`/`auto` refuse; record the probe reason; never fall back to the host |
| CI image without a user systemd manager | skip layer 4 and record why; do not add a CI-only code path |

A CI that turns "no sandbox available" into a red build invites turning the
sandbox off to get green. Asserting the `advisory` report instead keeps the
information without the pressure.

---

## 7. What this document does not cover

| Out of scope | Where it belongs |
|---|---|
| Container image contents, and whether `python3` inside matches the host | the operator's image, documented in the container backend's own page |
| Network isolation under a container backend | the container runtime |
| Performance benchmarking of each backend | `scripts/benchmark_shims.py` exists for shims; a sandbox benchmark is *(to add)* when a backend lands |
| Whether `paths:` itself is correct | `docs/user-manual.md` §9.7 and the `paths:` tests |
