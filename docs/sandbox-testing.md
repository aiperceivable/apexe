# Sandbox testing and integration

**Status: specification. No sandbox backend is implemented yet.**
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
| 4. Enforcement | a denied write fails at the kernel; a permitted write succeeds | Linux (`systemd-run`/`bwrap`); **not macOS**, see §4 | integration test, `#[ignore]` or platform-skipped |
| 5. Audit | `sandbox_probe` at startup and the five `sandbox_*` fields per execution | any OS | integration test reading `audit.jsonl` |

Layer 4 is *the* acceptance criterion for a backend, and it has two halves.
Testing only the denied write passes an implementation that blocks everything;
testing only the permitted write passes one that blocks nothing.

---

## 3. The two probes

Both are literal commands, so a reviewer can run them by hand on any machine.

```bash
# Probe A — must fail. /etc is read_only in the shipped default policy.
systemd-run --user --quiet --pipe --wait \
  -p ReadOnlyPaths=/etc -p ReadOnlyPaths=/usr -p ReadOnlyPaths=/bin \
  -- /bin/sh -c 'echo x > /etc/sandbox-probe'
# expected: non-zero exit, EACCES / "Read-only file system"

# Probe B — must succeed. A writable directory inside the same policy.
dir="$(mktemp -d)"
systemd-run --user --quiet --pipe --wait \
  -p ReadOnlyPaths=/etc -p ReadWritePaths="$dir" \
  -- /bin/sh -c "echo x > '$dir/ok'"
# expected: exit 0, file present
```

The probes must also be run **through apexe**, not only against the backend
directly, or the test proves the backend works and says nothing about apexe's
translation:

```bash
apexe exec --dry-run -- /bin/sh -c 'echo x > /etc/sandbox-probe'   # expect sandbox.backend = systemd-run
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

## 4. macOS: measured, and the reason it cannot be verified there

Measured in this repository on macOS 26.3 (arm64), 2026-11-14:

```
$ sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath "/tmp/sbtest"))' \
    /bin/sh -c 'echo x > /tmp/sbtest/f'
exit=0
$ ls -l /tmp/sbtest/f
-rw-r--r--@ 1 tercel wheel 2 Nov 14 12:06 /tmp/sbtest/f
```

The profile was accepted and the write was not blocked. Reproduce with the two
commands above; the only prerequisite is `/tmp/sbtest` existing.

Consequences, all of which are *conclusions from that measurement*, not
assumptions:

1. `sandbox-exec` is not a candidate backend (`docs/sandbox.md` §7).
2. Layer 4 cannot be verified on macOS. A developer on macOS can verify layers
   1, 2, 3 and 5 and nothing else; the documentation must say so, or the first
   person to run layer 4 will read its skip as a bug.
3. On macOS the honest level is `advisory`, and `apexe sandbox check` must print
   that together with the reason (`sandbox-exec present but not enforcing`),
   so no one concludes the policy is being enforced because the binary exists.

Tooling present on this machine (checked, not assumed):

```
$ which sandbox-exec bwrap docker
/usr/bin/sandbox-exec
bwrap not found
/usr/local/bin/docker
```

---

## 5. Backend acceptance checklist

A new backend is accepted when every line below is demonstrated in the pull
request that adds it. "Demonstrated" means a test in this repository, not a
transcript in a description.

| # | Check | Where it is proved |
|---|---|---|
| 1 | `probe()` returns `available: false` with a factual `reason` when the program is absent from `PATH` | unit test, probe injected |
| 2 | `probe()` reports `advisory` (never `enforcing`) on a platform where enforcement is not effective | unit test + the §4 measurement |
| 3 | Declared `capabilities` match what the backend can express; a policy needing an absent capability yields `Refusal`, not a dropped entry | unit test with a policy containing `no_access` inside a granted root |
| 4 | `translate()` is deterministic: identical inputs produce identical argv | unit test, run twice and compared |
| 5 | `sandbox.required` + unavailable backend refuses **before** spawning | unit test asserting the spawn function was not called |
| 6 | Probes A and B both hold through `apexe exec` | integration test, Linux only |
| 7 | Probe C fails under the backend, and the failure is attributable to the kernel (`EACCES`), not to a wrapper script exiting early | integration test, Linux only |
| 8 | `audit.jsonl` carries `sandbox_probe` at startup and the five `sandbox_*` fields on every execution, with `sandbox_outcome` never silently `applied` when the backend was not used | integration test reading the file |
| 9 | A call the operator did not select is **not** wrapped, and a call they did select is (selection rules: `docs/sandbox.md` §6) | integration test asserting the backend never runs for `cli.ls` |

Items 1–5 and 8–9 are the ones that catch the realistic failures: a backend that
reports success, a policy entry silently dropped, a selection that wraps
everything, or an audit trail that cannot distinguish "confined" from "not".

---

## 6. Skipping, not failing

A backend absent on the current machine is an expected state, not a failure.
The rule:

| Situation | Test behaviour |
|---|---|
| Backend not installed | skip, with the probe's `reason` in the skip message |
| Platform cannot enforce (macOS) | skip layer 4; assert `apexe sandbox check` reports `advisory` **and** names the reason |
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
