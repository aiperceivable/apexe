# F9: Sandbox delegation -- 0.15.0 implementation

| Field | Value |
|---|---|
| **Feature ID** | F9 |
| **Status** | Implemented in 0.15.0: contract, four built-in backends, config, `sandbox check`, dry-run and audit. Optional adapters are later. |
| **Dependencies** | F5 (Governance), F8 (Local Execution) |
| **Depended On By** | None |
| **Owning contract** | [`docs/sandbox.md`](../sandbox.md) |
| **Acceptance plan** | [`docs/sandbox-testing.md`](../sandbox-testing.md) |
| **Source review** | [`docs/ideas/sandbox-source-notes.md`](../ideas/sandbox-source-notes.md) |

---

## 1. Purpose

Give selected CLI calls a real operating-system sandbox **without** writing a
new sandbox runtime. apexe translates its existing `paths:` policy into the
arguments of an existing backend, executes the call through one spawn point,
and records which backend actually ran.

The policy layers stay where they are:

```text
coverage (bindings) -> permission (acl.yaml) -> access (paths/sandbox) -> executor
```

This feature does **not** replace ACL, approval, the path guard, or the
`child_exec` analysis. It adds an opt-in enforcement backend for the modules an
operator selects.

## 2. Scope for 0.15.0

### In scope

| Area | 0.15.0 |
|---|---|
| Backends | `systemd-run`, `bwrap`, `sandbox-exec`, `docker` |
| Defaults | per platform: macOS `sandbox-exec`; Linux `systemd-run`; `bwrap` preferred when installed; Docker is an optional container fallback |
| Configuration | `sandbox.backend`, `sandbox.default`, `sandbox.modules`, `sandbox.external` (reserved), `sandbox.backends.docker` |
| Selection | by **module ID**, never by command name; explicit list only |
| Modes | `off`, `required`, `auto`; **no `advisory` execution mode** |
| Fail-closed | unavailable backend, inexpressible policy, missing Docker image, translation error → refuse before spawn under `required`/`auto` |
| Dry-run | `apexe exec --dry-run` reports the backend, enforcement, launch argv/spec, policy digest and refusal reason |
| Actual execution | `apexe exec` and PATH shims run through the selected backend. MCP/A2A external routing is reserved for 0.16. |
| Audit | `sandbox_backend`, `sandbox_enforcement`, `sandbox_outcome`, `sandbox_policy_digest`, `sandbox_reason`; `sandbox_probe` governance event |
| Diagnostics | `apexe sandbox check` (table/JSON), static probe |
| Contract | `PathPolicy` → `PathSpelling` → `MountPlan` → `LaunchSpec` → `SandboxChild`; one executor spawn |
| Tests | unit + integration; A/B/C/canonical/NoAccess/WriteOnly/DenyWithinGrant/selection/audit/fail-closed |

### Out of scope (later)

- `nono`, Apple `container`, `microsandbox`, `shuru`, `pullrun`, Firecracker,
  gVisor, E2B, OpenSandbox, CubeSandbox;
- third-party adapter protocol (0.16);
- network policy, credentials, agent profiles, Windows sandbox;
- `external` container routing (field reserved, not applied);
- a new sandbox runtime, microVM, container runtime, or remote protocol;
- hardcoded command-name → sandbox maps;
- generic YAML `args:` / profile templates.

## 3. Contract

```text
SandboxRuntime
  select(module_id) -> Selection (Off | Required | Auto)
  probe(backend)    -> Probe { available, enforcement, reason, capabilities }
  plan(module_id, binary, argv, io) -> Option<SandboxPlan> | Refusal

SandboxPlan
  backend, enforcement, program, argv, policy_digest, cleanup
```

The translation is pure and typed:

```text
PathPolicy (config `paths:`)
  -> PathSpelling { original, resolved, is_file }   (normalize once)
  -> MountPlan { read_only, read_write, no_access, write_only, cwd }
  -> LaunchSpec:
       systemd-run command
       bwrap command
       sandbox-exec profile + command
       docker run container spec
```

No backend canonicalizes paths on its own; the runtime normalizes once. The
`PathSpelling` is where `/tmp` → `/private/tmp` and `/etc` → `/private/etc` are
handled.

### 3.1 Backend capabilities

| Backend | ReadOnly | ReadWrite | NoAccess | WriteOnly | DenyWithinGrant |
|---|---|---|---|---|---|
| `systemd-run` | yes | yes | yes | **no** | yes |
| `bwrap` | yes | yes | yes | **no** | yes |
| `sandbox-exec` | yes | yes | yes | yes | yes |
| `docker` | yes | yes | yes | **no** | yes |

A policy that needs a capability the selected backend lacks is a **refusal**,
not a silent drop. In 0.15.0 the shipped `paths.write_only` default is empty;
an operator who sets it with `systemd-run`/`bwrap`/`docker` gets a refusal.

### 3.2 Modes

- `off`: no sandbox; selected modules are not confined. Explicit choice only.
- `required`: selected modules use the configured/default backend; unavailable,
  inexpressible or translation error → refuse before spawn.
- `auto`: choose the first available enforcing, capable backend in the platform
  order; if none → refuse. No `advisory` execution.

`modules: []` means nothing is selected, even under `required`/`auto`.

## 4. Configuration

```yaml
sandbox:
  backend: auto                  # auto | systemd-run | bwrap | sandbox-exec | docker
  default: off                   # off | required | auto
  modules:                       # module IDs only, not command names
    - cli.sed
    - cli.python3
  external: none                 # none | container (reserved in 0.15.0)
  backends:
    docker:
      image: ""                  # required for the docker backend
      network: none
      read_only: true
      user: ""
      memory: ""
      pids: 0
```

- `backend: auto` prefers an OS-provided local sandbox; Docker is a fallback
  only when configured.
- `sandbox check` reports the selection and reason.
- `sandbox.modules` uses the same module IDs as `acl.yaml`.
- `sandbox.external` is parsed and reported but not applied in 0.15.0.

## 5. CLI surface

```bash
apexe sandbox check
apexe sandbox check --format json
apexe exec --dry-run -- sed -i 's/a/b/' file
```

`exec --dry-run` adds:

```json
{
  "sandbox": {
    "backend": "sandbox-exec",
    "enforcement": "enforcing",
    "outcome": "planned",
    "policy_digest": "sha256:...",
    "launch": {"program": "...", "argv": ["..."]},
    "reason": null
  }
}
```

A refusal is reported in `sandbox.reason`, sets `valid: false`, and appears in
`checks`; no process is spawned.

## 6. Failure rules

| Situation | `off` | `required` | `auto` |
|---|---|---|---|
| No backend on this platform | run unconfined (explicit) | refuse | refuse |
| Backend missing | run unconfined (explicit) | refuse | refuse |
| Policy needs unsupported capability | run unconfined (explicit) | refuse | refuse |
| Translation error | run unconfined (explicit) | refuse | refuse |
| Docker selected, no image | run unconfined (explicit) | refuse | refuse |

`off` is an explicit no-sandbox choice. A missing backend never silently
becomes `off`.

## 7. Audit

Every sandboxed execution carries:

```text
sandbox_backend, sandbox_enforcement, sandbox_outcome,
sandbox_policy_digest, sandbox_reason
```

`sandbox check` writes one `sandbox_probe` governance event. `policy_digest`
hashes the translated invocation, not the YAML.

## 8. Acceptance criteria

1. `apexe config --init` includes `sandbox:` with `default: off`.
2. `apexe sandbox check` reports the selected backend, enforcement, capabilities
   and reason in table and JSON.
3. `apexe exec --dry-run` reports the sandbox plan without executing.
4. `apexe exec` runs a selected module through the selected backend and returns
   the child exit status.
5. `systemd-run`, `bwrap`, `sandbox-exec`, `docker` translators produce the
   documented argv/spec for a fixture `PathPolicy`.
6. A/B/C: denied write fails, permitted write succeeds, computed-path write
   fails at the kernel.
7. Canonical paths: `/tmp`/`/private/tmp`, `/etc`/`/private/etc`, symlink and
   dangling cases.
8. `NoAccess` denies content and metadata; `WriteOnly` writes but cannot read
   on `sandbox-exec`; unsupported backends refuse.
9. `DenyWithinGrant`: `read_only /var`, `read_write /var/log`,
   `no_access /var/log/secret`.
10. Selection wraps only listed modules; `cli.ls` is not wrapped when unlisted.
11. `required`/`auto` refuse before spawn when no backend/capability/image.
12. Audit fields and `sandbox_probe` present; one spawn point.
13. `make check` and `mkdocs --strict` pass.

Platform tests:

- macOS: `sandbox-exec` A/B/C (real).
- Linux: `systemd-run`/`bwrap` A/B/C when available; skip with a reason when
  the user manager/userns is unavailable.
- Docker: optional A/B/C; skip if no daemon/image.
- No CI-only code path; skipped backend is reported, not failed.

## 9. File map

| File | Change |
|---|---|
| `src/governance/sandbox.rs` (new) | Config types, PathSpelling, MountPlan, backend translators, runtime, plan, check |
| `src/config.rs` | `SandboxConfig`, `ApexeConfig.sandbox`, template |
| `src/cli/sandbox.rs` (new) | `sandbox check` |
| `src/cli/mod.rs` | command dispatch; `sandbox` |
| `src/cli/exec.rs` | dry-run sandbox field |
| `src/module/executor.rs` | sandbox plan at the one spawn point |
| `src/module/io.rs` | carry the plan through captured/passthrough |
| `src/module/cli_module.rs` | attach/plan sandbox; audit outcome |
| `src/module/registry.rs` | `ExecutorOptions.sandbox`; attach to modules |
| `src/governance/audit.rs` | sandbox fields; probe event |
| `src/governance/path_guard.rs` | `sandbox_policy()` snapshot |
| `docs/config.example.yaml` | sandbox section |
| `docs/user-manual.md` | sandbox check/exec/dry-run |
| `docs/sandbox.md` / `sandbox-testing.md` | implementation notes |
| `docs/features/v2-f9-sandbox.md` | this file |
| `mkdocs.yml` | nav |
| `CHANGELOG.md` / `Cargo.toml` / docs versions | 0.15.0 |

## 10. Non-goals and risks

- Not a sandbox runtime; no kernel sandbox implementation.
- `sandbox-exec` is deprecated; the 0.15.0 implementation must keep the
  deprecation visible and use canonical paths.
- `systemd-run` needs a user manager/DBus; it is not available in every
  container/CI image.
- `bwrap` is a package, not part of the base OS.
- Docker changes the filesystem/tool identity; it is an explicit fallback, not
  the local default.
- `write_only` is unsupported by `systemd-run`/`bwrap`/`docker`; refuse rather
  than approximate.
- `external` container routing and the adapter protocol are 0.16.

## 11. Version

This is the **0.15.0** feature. The release bumps `Cargo.toml`, `Cargo.lock`,
`CHANGELOG.md`, `docs/user-manual.md`, `docs/FEATURE_MANIFEST.md` and
`examples/README.md`. The sandbox implementation is not a normal-execution
sandbox for unselected modules; it is opt-in.
