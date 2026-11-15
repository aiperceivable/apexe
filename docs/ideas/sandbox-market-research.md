# Sandbox market research

**Status:** research note, 2026-11-15. This is not a contract and not a
commitment. The owning design document remains [`docs/sandbox.md`](../sandbox.md);
the acceptance plan remains [`docs/sandbox-testing.md`](../sandbox-testing.md).

## 1. Method and limits

- GitHub repository search across `agent sandbox`, `AI sandbox`, `sandbox
  runtime`, `code execution sandbox`, `sandbox mcp`, `microvm sandbox`,
  `firecracker sandbox`, `bubblewrap sandbox`, `landlock sandbox`, and `sandbox
  cli`.
- Repository metadata (stars, language, license, push date) read from the
  GitHub API on 2026-11-15.
- READMEs read for the projects named below.
- Stars and README claims are evidence of attention, **not** evidence of
  security. No project below was independently penetration-tested.
- The original macOS `sandbox-exec` measurement in this repository was a
  **false negative**: the test used `/tmp/sbtest`, which resolves to
  `/private/tmp/sbtest`, and Seatbelt matched only the literal spelling. The
  corrected test with `/private/tmp/sbtest` or a real home path blocks the
  write; `(deny default)` blocks execution. `sandbox-exec` is **deprecated but
  enforcing**, not inert. Details are in `docs/sandbox-testing.md` §4.

The question is not "can apexe build a sandbox?" It is "which existing sandbox
should apexe delegate to, and what part of the problem is still ours?"

## 2. Executive summary

1. **Local OS sandboxing is a solved, crowded layer.** Anthropic's
   `sandbox-runtime` (7.1k stars, TypeScript, Apache-2.0, beta) wraps
   `sandbox-exec` on macOS and `bubblewrap` on Linux. OpenAI Codex (135k),
   Gemini CLI (108k), and Claude Code all ship local sandboxes. Writing another
   Seatbelt/Landlock/bubblewrap wrapper would mostly re-implement existing code.
2. **`nono` is the closest adjacent product to apexe's desired outcome.**
   It is Rust, Apache-2.0, ~5k stars, supports Pi (the host used here), runs each
   tool in a kernel-enforced sandbox, and ships credentials, network policy,
   audit, and rollback. macOS uses the private `sandbox_init` API, not the
   `sandbox-exec` CLI; whether that still enforces on macOS 26.3 must be
   measured, not assumed. Linux uses Landlock + seccomp + namespaces.
3. **Local microVMs are the 2026 trend, and they solve the macOS problem
   honestly.** `microsandbox` (9k, Rust, Apache-2.0, libkrun, macOS Apple
   Silicon + Linux KVM, OCI images, SDK + MCP), `shuru` (894, Apple
   Virtualization.framework), `pullrun` (134, OCI to Apple Silicon VM /
   Firecracker / container), and Apple's official `container` (51k, Swift,
   Apache-2.0, macOS 26, one Linux VM per container) all run the workload in a
   VM boundary. They are stronger than `sandbox-exec` and do not need Docker.
4. **Docker is a backend, not a probe.** It is a real container runtime and is
   already installed on this machine. On macOS it is a container inside Docker
   Desktop's Linux VM; on Linux it shares the host kernel. It is useful as a
   fallback/container backend, not as "probe and then run on the host".
5. **Remote/self-hosted sandbox platforms exist and are later.**
   E2B (14.8k, Firecracker, SDK, self-hosting), OpenSandbox (17.3k, Docker/K8s,
   Firecracker, API + MCP), CubeSandbox (13.2k, KVM microVM, E2B-compatible,
   x86_64 Linux), and others. They are the right later backend for untrusted
   remote execution, not the first local backend.
6. **There is no direct competitor to apexe's actual differentiator.** None of
   the sandbox projects combines typed CLI coverage (the corpus), ACL,
   approval, audit, and MCP/A2A serving. The market has many sandboxes and
   several agent-specific policy layers; it does not have a governed CLI
   execution layer. That is apexe's niche.
7. **The gap worth building is the translation and evidence layer**, not the
   sandbox: `paths:` → backend capabilities, fail-closed selection, one spawn
   point, and an audit record that says which backend actually confined the
   call.
8. **The macOS `sandbox-exec` result is a deprecation risk, not an inert
   sandbox.** The original test used `/tmp`, which resolves to `/private/tmp`;
   Seatbelt matched the literal spelling and allowed the write. The corrected
   test enforces. The market tools use it because it works, and they
   canonicalize paths: Gemini's `resolveToRealPath` and `nono` emit both the
   original and resolved spelling. The real risk is that Apple has deprecated
   the API; `nono`'s private `sandbox_init` and the VM backends are the
   long-term options.

## 3. Market map

Metadata from the GitHub API on 2026-11-15. Stars are rounded; licenses are the
repository's declared SPDX identifier or `NOASSERTION`.

### 3.1 Local OS sandboxes / agent runtimes

| Project | Stars | Language | License | Platform / backend | Role |
|---|---:|---|---|---|---|
| [anthropics/sandbox-runtime](https://github.com/anthropics/sandbox-runtime) | 7.1k | TypeScript | Apache-2.0 | macOS `sandbox-exec`, Linux `bubblewrap`, Windows WFP, proxy network | Closest local sandbox design; beta; MCP server sandboxing |
| [openai/codex](https://github.com/openai/codex) | 135k | Rust | Apache-2.0 | macOS Seatbelt, Linux Landlock/seccomp, Windows sandbox | Coding agent with a mature sandbox and `execpolicy`; reference, not a backend |
| [google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli) | 108k | TypeScript | Apache-2.0 | macOS Seatbelt, Linux bwrap/Docker/Podman | Agent; sandbox selection/fallback reference |
| [nolabs-ai/nono](https://github.com/nolabs-ai/nono) | 5.0k | Rust | Apache-2.0 | macOS Seatbelt (`sandbox_init`), Linux Landlock/seccomp/namespaces, WSL2 | Closest product: per-tool sandbox, credentials, network, audit, Pi profile; Rust/Python FFI |
| [Warashi/cage](https://github.com/Warashi/cage) | 127 | Go | Apache-2.0 | Cross-platform write-restricted command sandbox | Small CLI; not a path-policy backend |
| [agentic-dev3o/sandbox-shell](https://github.com/agentic-dev3o/sandbox-shell) | 32 | Rust | MIT | macOS Seatbelt | Small CLI; credential protection focus |
| [umago/bubblewrap-ai](https://github.com/umago/bubblewrap-ai) | 26 | Go | MIT | Linux bwrap | Small agent wrapper |
| [isanchez31/opencode-sandbox-plugin](https://github.com/isanchez31/opencode-sandbox-plugin) | 71 | TypeScript | MIT | Uses `@anthropic-ai/sandbox-runtime` | Ecosystem integration example |

### 3.2 Local microVM / container runtimes

| Project | Stars | Language | License | Platform / backend | Role |
|---|---:|---|---|---|---|
| [superradcompany/microsandbox](https://github.com/superradcompany/microsandbox) | 9.1k | Rust | Apache-2.0 | libkrun microVM; macOS Apple Silicon + Linux KVM; OCI; SDK + MCP | Best local microVM candidate; no Docker daemon |
| [apple/container](https://github.com/apple/container) | 51k | Swift | Apache-2.0 | macOS 26; one Linux VM per container; OCI | Official macOS VM backend; strong; new |
| [apple/containerization](https://github.com/apple/containerization) | 9.0k | Swift | Apache-2.0 | Virtualization.framework / Cloud Hypervisor | Swift package behind `container` |
| [superhq-ai/shuru](https://github.com/superhq-ai/shuru) | 894 | Rust | Apache-2.0 | macOS Apple Virtualization.framework; experimental Linux KVM | Ephemeral microVM; mounts/network/secrets/checkpoints |
| [pullrun/pullrun](https://github.com/pullrun/pullrun) | 134 | Rust | Apache-2.0 | Apple Silicon VM, Firecracker, Linux container; OCI; MCP | One binary, multiple execution targets; new |
| [cocoonstack/cocoon](https://github.com/cocoonstack/cocoon) | 266 | Go | MIT | Cloud Hypervisor microVM; OCI | MicroVM engine; Linux |
| [lima-vm/lima](https://github.com/lima-vm/lima) | 22.4k | Go | Apache-2.0 | Linux VMs on macOS | Run bwrap/apexe inside a Linux VM |
| [abiosoft/colima](https://github.com/abiosoft/colima) | 31.7k | Go | MIT | Container runtimes on macOS | Docker/containerd VM wrapper |
| [podman-container-tools/podman](https://github.com/podman-container-tools/podman) | 33.3k | Go | Apache-2.0 | Rootless OCI containers; macOS VM | Docker alternative |
| Docker Desktop / OrbStack | — | — | proprietary | Linux VM + containers on macOS | Already present; fallback |

### 3.3 Remote / self-hosted sandbox platforms

| Project | Stars | Language | License | Isolation / API | Role |
|---|---:|---|---|---|---|
| [e2b-dev/E2B](https://github.com/e2b-dev/E2B) | 14.9k | Python | Apache-2.0 | Firecracker; cloud SDK + self-hosting | De facto remote sandbox API |
| [opensandbox-group/OpenSandbox](https://github.com/opensandbox-group/OpenSandbox) | 17.3k | Go | Apache-2.0 | Docker locally, K8s + Firecracker in cluster; API/MCP | Self-hosted platform; unified lifecycle API |
| [TencentCloud/CubeSandbox](https://github.com/TencentCloud/CubeSandbox) | 13.2k | Go | NOASSERTION | KVM microVM; E2B-compatible; x86_64 Linux | Strong Linux microVM service; not macOS |
| [kubernetes-sigs/agent-sandbox](https://github.com/kubernetes-sigs/agent-sandbox) | 4.6k | Go | Apache-2.0 | Kubernetes controller for singleton workloads | Future cluster backend |
| [deeplethe/sandstorm](https://github.com/deeplethe/sandstorm) | 6.1k | Rust | Apache-2.0 | Thousands of agent sandboxes | Cluster/scale; not local |
| [BitMiracle-AI/Dormice](https://github.com/BitMiracle-AI/Dormice) | 1.9k | TypeScript | Apache-2.0 | Self-hosted, E2B-compatible | Small self-hosted platform |
| [daytonaio/daytona](https://github.com/daytonaio/daytona) | 71k | — | — | Archived 2026-07 | Do not adopt as a new dependency; check successor |
| [OpenHands/OpenHands](https://github.com/OpenHands/OpenHands) | 93k | TypeScript | MIT | Docker runtime | Agent; not a generic backend |
| E2B / Modal / Runloop / Blaxel / Northflank / Cloudflare / Vercel / CodeSandbox | — | — | proprietary | Cloud sandboxes | Remote backends later |

### 3.4 Kernel and process primitives

| Project | Stars | Language | License | What it gives |
|---|---:|---|---|---|
| [containers/bubblewrap](https://github.com/containers/bubblewrap) | 9.5k | C | NOASSERTION (check LGPL) | Unprivileged namespaces; the natural Linux local backend when the package is installed |
| [google/nsjail](https://github.com/google/nsjail) | 4.2k | C++ | Apache-2.0 | Namespaces, cgroups, rlimits, seccomp; Linux |
| [google/minijail](https://github.com/google/minijail) | 396 | C | BSD-3-Clause | ChromeOS/Android containment |
| [netblue30/firejail](https://github.com/netblue30/firejail) | 7.7k | C | GPL-2.0 | setuid Linux sandbox; not a first choice |
| [landlock-lsm/rust-landlock](https://github.com/landlock-lsm/rust-landlock) | 342 | Rust | NOASSERTION | Rust Landlock binding; used by `nono` |
| [google/gvisor](https://github.com/google/gvisor) | 20k | Go | Apache-2.0 | User-space kernel for containers; untrusted workloads |
| [firecracker-microvm/firecracker](https://github.com/firecracker-microvm/firecracker) | 38k | Rust | Apache-2.0 | microVM; used by E2B/CubeSandbox/shuru |
| [kata-containers/kata-containers](https://github.com/kata-containers/kata-containers) | 9.1k | Rust | Apache-2.0 | VM-isolated containers |
| [cloud-hypervisor/cloud-hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor) | 6.5k | Rust | — | VMM; used by Cocoon/Containerization |
| systemd `systemd-run` | — | C | LGPL | Linux service sandbox; OS-provided, zero third-party install |

## 4. Deep dives

### 4.1 Anthropic `sandbox-runtime` (`srt`)

- **What:** lightweight wrapper for arbitrary processes, MCP servers, and shell
  commands; filesystem and network restrictions without a container.
- **Backends:** macOS `sandbox-exec` with generated Seatbelt profiles; Linux
  `bubblewrap`; Windows dedicated local account + WFP egress fence.
- **Network:** allow-only proxy model; HTTP/HTTPS + SOCKS5 proxies; Linux uses
  a Unix socket inside a network namespace; macOS allows only a localhost proxy
  port.
- **Strengths:** clean policy model, network proxy, MCP server support,
  violation attribution, library and CLI.
- **Weaknesses for apexe:** beta research preview; Node/TypeScript; macOS
  `sandbox-exec` is deprecated but works when the profile uses canonical paths
  (the tool already canonicalizes); it is a policy wrapper, not a typed CLI
  governance layer; adopting it would duplicate apexe's path policy.
- **Verdict:** best **reference** for network policy and MCP sandboxing; not the
  first backend.

### 4.2 Codex / Gemini CLI / Claude Code

- All three ship local agent sandboxes:
  - Codex: macOS Seatbelt, Linux Landlock/seccomp, Windows sandbox; approval
    policy; `execpolicy`.
  - Gemini CLI: macOS Seatbelt profiles, Linux bwrap/Docker/Podman, policy
    manager and folder trust.
  - Claude Code: `sandbox-runtime`.
- **Strengths:** battle-tested UX, approval modes, platform detection,
  fallback matrices, Windows coverage.
- **Weaknesses for apexe:** they are agents, not reusable backends; their
  policies are tied to their own tool calls; extracting the sandbox code would
  be a fork, not an integration.
- **Verdict:** read their selection/approval/fallback code; do not depend on
  the projects.

### 4.3 `nono`

- **What:** complete agent runtime security: per-tool ephemeral sandboxes,
  filesystem/network/env/credential capabilities, profiles, audit, rollback,
  supply-chain attestation.
- **Backends:** macOS Seatbelt via the private `sandbox_init` API (not the
  `sandbox-exec` CLI); Linux Landlock + seccomp + optional namespaces.
- **Pi:** first-class profile (`nono search pi`, `nono run --profile
  nolabs-ai/pi -- pi`). This is the host used here.
- **Rust:** `crates/nono` exposes `CapabilitySet`/`Sandbox`; FFI for Rust and
  Python; single unprivileged binary.
- **Strengths:** closest product to "sandbox + policy + audit + agent"; Rust;
  Apache-2.0; no root; credentials never enter the sandbox; Pi support; active.
- **Risks:** macOS uses a private API whose macOS 26.3 behavior is unverified;
  its profile model is agent/tool-centric, not apexe's typed CLI corpus; its
  policy/audit overlaps with apexe.
- **Verdict:** **evaluate first**. If its macOS probe enforces and its profile
  can express `paths:`, it is a better sandbox backend than anything apexe
  would write. If not, use its Linux/Landlock path or the microVM backends.

### 4.4 `microsandbox`

- **What:** local-first microVM runtime and library for untrusted workloads.
- **Platform:** macOS Apple Silicon, Linux KVM; OCI images; Docker-like
  workflows; SDKs; MCP server.
- **Strengths:** hardware isolation; no Docker daemon; local; Rust; Apache-2.0;
  9k stars; active; OCI; network/volume/secret features.
- **Risks:** VM startup cost (order of 1s, not a shim); image/tool drift; new
  compared with Docker; a VM per selected module.
- **Verdict:** strongest local microVM candidate for macOS and Linux; a good
  backend for untrusted selected modules.

### 4.5 Apple `container` / `containerization`

- **What:** official Swift tooling to run OCI Linux containers as lightweight
  VMs on macOS 26, one VM per container.
- **Backends:** Virtualization.framework on Apple silicon; Cloud Hypervisor +
  KVM on Linux.
- **Strengths:** Apple-maintained; per-container VM; no Docker daemon;
  sub-second start claims; OCI; macOS 26 (this machine is 26.3).
- **Risks:** macOS 26 only; new; CLI/API still evolving; need to verify the
  security flags and mount/network semantics.
- **Verdict:** strongest **official** macOS backend to evaluate alongside
  `microsandbox`; Docker Desktop remains the installed fallback.

### 4.6 `shuru`, `pullrun`, `Dory`, `cocoon`

- `shuru`: macOS Apple Virtualization.framework, ephemeral microVM, VirtioFS
  mounts, network allowlist, secret proxy, checkpoints; experimental Linux;
  new (894 stars).
- `pullrun`: one OCI image to Apple Silicon VM, Firecracker, Linux container,
  or Kubernetes; no daemon; MCP; policy engine; new (134 stars).
- `Dory`: Swift, Apple Silicon local development with policy-bound agent
  sandboxes; GPL-3.0; new.
- `cocoon`: Cloud Hypervisor microVM engine; OCI; Linux; 266 stars.
- **Verdict:** interesting second-wave backends; too new for the first
  production backend; keep on the radar.

### 4.7 Docker / Podman / Colima / Lima

- Docker is a real container runtime; on macOS it is a container in Docker
  Desktop's Linux VM; on Linux it shares the host kernel.
- It is the **only sandbox runtime already installed** on this machine.
- Container semantics differ from a path sandbox: image filesystem, bind
  mounts, UID ownership, startup cost, daemon/socket trust surface, tool drift.
- **Verdict:** first fallback/container backend; not a path sandbox with extra
  steps; never a "probe then host" fallback.

### 4.8 `bubblewrap` / Landlock / nsjail

- `bwrap` is the natural Linux local backend **when installed**: unprivileged
  namespaces, precise ro/rw/tmpfs mounts, no daemon, millisecond startup.
- `systemd-run` is the **zero third-party install** option on systemd Linux; it
  needs a user manager/DBus, which containers and some CI images lack.
- Landlock (Rust crate) is the natural no-userns, no-external-binary option;
  `nono` already wraps it with seccomp and audit.
- `nsjail`/`minijail`/`firejail` are older/setuid or ChromeOS-specific; not the
  first choice.
- **Verdict:** Linux default is `systemd-run` for zero-install, `bwrap` when the
  package is present (preferred for path precision), `nono`/Landlock where user
  namespaces are unavailable.

### 4.9 E2B / OpenSandbox / CubeSandbox

- E2B: cloud Firecracker sandboxes, SDKs, self-hosting; de facto remote API.
- OpenSandbox: self-hosted Docker/K8s, Firecracker, unified lifecycle API,
  SDKs, MCP.
- CubeSandbox: KVM microVMs, E2B-compatible, x86_64 Linux, millisecond boot.
- **Verdict:** remote/self-hosted backend for untrusted external callers later;
  E2B-compatible API is a useful contract reference; not the first local
  backend and not macOS.

## 5. Findings against apexe's design

| apexe design point | Market evidence | Consequence |
|---|---|---|
| `sandbox.md` delegates, does not implement a kernel sandbox | Every mature project delegates to bwrap/Seatbelt/Landlock/Firecracker/VM | Keep the delegation design |
| `paths:` → backend policy | No project maps this CLI path vocabulary to a backend | This is the build-worthy layer |
| opt-in per module, no blanket | bwrap/container startup costs; shims cannot pay | Keep selection explicit |
| `enforcing` / `advisory` | The market uses Seatbelt and canonicalizes paths; our original test was a false negative | Drop `advisory` as an execution mode; use `off` / `required` / `auto` enforcing |
| one spawn point | Codex/Gemini/srt all have a single execution path | Keep the executor wrapper |
| audit with backend/outcome | Most projects log denials, few prove which backend ran | Keep the five audit fields; this is a differentiator |
| container is not a path sandbox | Docker/Apple container semantics differ (image, UID, mounts) | Treat container as a distinct backend with explicit capabilities |
| macOS native | `sandbox-exec` is deprecated but enforcing with canonical paths; the market canonicalizes; `nono`/VM are long-term | Candidate with a canonical-path probe; do not treat it as inert, and do not omit the deprecation risk |
| network and credentials | `srt`, `nono`, E2B, OpenSandbox, CubeSandbox all have them | Add network/credentials after filesystem; do not claim them now |
| remote | E2B/OpenSandbox/CubeSandbox have lifecycle APIs | Add a remote backend later; do not invent a protocol |

## 6. Build vs buy

**Do not build:**

- another Seatbelt/Landlock/bwrap wrapper;
- another microVM or container runtime;
- another agent profile system;
- another remote sandbox protocol;
- a `sandbox-exec` backend without canonical-path handling and a passing probe; it is deprecated but not inert, and a spelling-only profile is a false negative.

**Build:**

- the backend contract: `probe`, `capabilities`, `translate`, `execute`,
  `wait`, `kill`, `cleanup`;
- `LaunchSpec` variants for command, container, microVM, and remote;
- the path-policy translation and capability refusal (`DenyWithinGrant`,
  `write_only`, unexpressible policy);
- selection and fail-closed (`off` / `required` / `auto`, no `advisory`
  execution, no host fallback);
- the audit fields and policy digest;
- the `sandbox check` and `exec --dry-run` surface;
- the CLI governance/ACL/approval/corpus (apexe's actual differentiator).

**Integrate / evaluate:**

- `nono` for process sandbox + credentials/network/audit, especially with Pi;
- `microsandbox` or Apple `container` for macOS VM isolation;
- `systemd-run` / `bwrap` for Linux local;
- Docker for the already-installed fallback/container path;
- E2B/OpenSandbox/CubeSandbox for remote untrusted execution later;
- `sandbox-runtime`/Codex/Gemini as design references, not dependencies.

### 6.1 Contract and adapter boundary

The implementation is a **backend contract + adapters**, not a new sandbox
runtime. The built-in backends implement an in-process contract
(`probe`/`capabilities`/`translate`/`execute`/`wait`/`kill`/`cleanup`); a
versioned out-of-process adapter protocol is published only after the built-in
pair passes the conformance suite. The protocol is a driver contract, not a
policy language: the adapter receives the effective `PathPolicy` and
`Invocation`, returns a `LaunchSpec` or a `Refusal`, and declares its
capabilities. An adapter that cannot express a policy entry refuses; it never
drops it. Adapters are trusted components (they run commands), so installation
is signed/allowlisted and the operator selects them. All surfaces still use the
executor's one spawn point; the adapter does not add a second execution surface.

The default pair requires no third-party adapter or install: macOS
`sandbox-exec` (OS, deprecated, canonical paths) and Linux `systemd-run` (OS),
with `bwrap` (the `bubblewrap` package) preferred when installed. Docker, `nono`,
`microsandbox`, Apple `container`, and E2B/OpenSandbox are optional adapters.

## 7. Recommended backend order

1. **Linux local:** `systemd-run` (OS-provided, zero third-party install; requires
   a systemd user manager/DBus) first; `bwrap` (the `bubblewrap` package;
   preferred when installed because its mount namespace is more precise) second;
   `nono`/Landlock as the no-userns option; another backend as fallback.
2. **macOS local:** `sandbox-exec` (OS-provided, deprecated, but enforcing with canonical
   paths) or `nono` (private `sandbox_init`) after its probe; `microsandbox` or
   Apple `container` for a VM boundary; Docker Desktop as the installed
   fallback. Do not claim `sandbox-exec` enforcement without canonicalizing
   paths; do not build a long-term product on the deprecated API.
3. **External/untrusted:** Docker now; `microsandbox`/Firecracker/gVisor later;
   E2B/OpenSandbox/CubeSandbox as a remote backend when needed.
4. **Remote:** E2B-compatible lifecycle API later; not the first release.

## 8. Hands-on spike before writing code

Run the three probes in the candidate backend, through its own CLI, not just
against the backend directly:

```bash
# A: denied write must fail
# B: permitted write must succeed
# C: computed path must fail at the kernel
python3 -c 'open("/etc/apexe-sandbox-probe","w").write("x")'
```

Candidates to test on this macOS 26.3 arm64 machine:

- `sandbox-exec` with **canonical** paths (`realpath`); `/tmp` -> `/private/tmp`;
- `nono` (macOS private `sandbox_init`; Pi profile available);
- Docker Desktop (already installed);
- `microsandbox` (Apple Silicon; no Docker);
- Apple `container` (macOS 26; per-container VM).

On Linux, also test `systemd-run` (zero third-party install) and `bwrap`
(`bubblewrap` package) with the same A/B/C and canonical paths.

Record: A/B/C exit status and stderr; startup time; whether the probe is
distinguishable from a wrapper script failure; whether the path policy can be
expressed; whether the audit says which backend ran.

Only after that spike should the first backend be chosen.

## 9. Decision points

1. Is `nono` a **buy** for the sandbox layer, an **outer sandbox** around
   apexe, or only a design reference?
2. macOS first enforcing backend: `sandbox-exec` (deprecated, canonical paths)
   or `nono` (if probe passes); `microsandbox` or Apple `container` for a VM
   boundary; Docker remains the installed fallback.
3. Linux first backend: `systemd-run` (zero-install, OS) or `bwrap` (package;
   preferred when installed)?
4. Is `write_only` supported or explicitly refused per backend?
5. Does the first release include `external: container`, or only local
   selected modules?
6. Do we relax `child_exec` `computes_paths` only after an enforcing backend
   passes the probes? (Recommended: yes.)
7. Do we fix the existing `verification` `sandbox-exec` claim in the same
   change? (Recommended: yes.)
