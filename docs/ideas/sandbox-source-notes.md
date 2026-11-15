# Sandbox source notes

**Status:** source-reading notes, 2026-11-15. This is not a contract and not a
commitment. The owning design document remains [`docs/sandbox.md`](../sandbox.md);
the acceptance plan remains [`docs/sandbox-testing.md`](../sandbox-testing.md).

Reference checkouts are in the sibling directory
`../../../sandbox-references/`. They are read-only references, not dependencies
and not vendored. No code was copied. Licenses: Apache-2.0 for
`sandbox-runtime`, `nono`, Codex and Gemini; LGPL-2.1 for bubblewrap.

| Source | Commit | Reference path |
|---|---|---|
| `anthropics/sandbox-runtime` | `0bbebed0aa9a8e628dc4df8bc30293550a1988eb` | `sandbox-references/anthropic-sandbox-runtime` |
| `nolabs-ai/nono` | `165e2f7a4cc44552577c7ac649e069b0233c62b2` | `sandbox-references/nono` |
| `openai/codex` | `ee344ca9458351a6ea102795fa73110ad3ede5dc` | `sandbox-references/codex` (sparse) |
| `google-gemini/gemini-cli` | `4ed06ed0ddc523a6d356a4ad35ddfea331041888` | `sandbox-references/gemini-cli` (sparse) |
| `containers/bubblewrap` | `1fb0aa26d3eb66254f7742e704add7f82f41a582` | `sandbox-references/bubblewrap` |

## 1. Anthropic `sandbox-runtime`

### 1.1 Canonical paths are a first-class model

- `src/sandbox/sandbox-utils.ts:92-100` — `pathSpellings()` returns the caller's
  spelling and, when it differs, the realpath. A path that cannot be resolved
  is named by its spelling alone.
- `src/sandbox/sandbox-utils.ts:435` — `normalizePathForSandbox()` handles `~`,
  relative paths, `//`, `/./`, trailing slash, and symlink resolution for
  non-glob patterns. `opts.literal` distinguishes a name from a pattern.
- `src/sandbox/path-entries.ts` — a path entry is read as a pattern, as the
  literal name on disk, or both; denies take every reading, allows take only
  the exact spelling.
- `src/sandbox/macos-sandbox-utils.ts:1083` — the generated profile starts
  `(deny default)`; `file-read*`, `file-write*`, `file-write-unlink` and
  `file-write-create` are added explicitly. The read rules are emitted in a
  deliberate order: allow all reads, deny the deny-only set, re-allow the
  carve-outs, then re-emit denies that an allow would otherwise clobber
  (`:869-:960`). The write rules are ordered as allow-all-writes, then the
  denies, then re-allow `file-write-unlink`/`file-write-create` for allowed
  write roots (`:986-:1035`).
- `src/sandbox/macos-sandbox-utils.ts:518, 656, 808` — the profile is split and
  size-bounded because `sandbox-exec` rejects a string literal over 1025 bytes
  and pays compile time per path filter; `file-write-unlink`/`create` protects
  paths that a plain `file-write*` deny can be bypassed around by rename.
- `src/sandbox/macos-*.test.ts` — deny grouping, literal deny brackets, glob
  deny re-emit, profile size, PTY. These are good conformance cases.

**Adopt:** a `PathPolicy` normalization stage that carries
`{ original, resolved, is_file }`; a literal backend emits both spellings; a
deny is resolved to its end, an allow is resolved from its starting point; a
deny covers every reading, an allow only the exact spelling; profile-size
guard; `file-write-unlink`/`file-write-create` for the move bypass.

**Reject:** the credential masking, MCP sandbox, Windows/UNC and network-proxy
machinery in the first filesystem release. They are real features, but a
different layer from apexe's `paths:` contract.

### 1.2 Linux bwrap details worth copying at the argv level

- `src/sandbox/linux-sandbox-utils.ts:157` — `resolveSymlinkedDenyPath()`
  canonicalizes a deny path before a mask/bind; binding `/dev/null` over a
  symlink makes bwrap abort.
- `:288` — non-existent deny paths become a mount point, and the created mount
  point must be cleaned up after bwrap exits.
- `:769` — always `--cap-drop ALL`; `CAP_SETFCAP` only for the specific
  root/userns case, with a reasoned error when it is missing.
- `:882-:916` — the `--args` profile is held on a file descriptor and opened
  through `/proc/<pid>/fd/<n>`; this avoids argv size limits and keeps the
  profile read-only to the sandboxed process.
- `:99` — bwrap is resolved from PATH by default; the bwrap binary is
  configurable.
- `:661-:675` — bwrap mount points created for absent deny paths are tracked and
  removed; the source is never removed while a live bind uses it.

**Adopt:** `--ro-bind / /`, `--bind`, `--tmpfs`, `--dev`, `--proc`,
`--unshare-all`, `--new-session`, `--die-with-parent`, `--cap-drop ALL`,
`--args`; cleanup of synthetic mount points; the "bind `/dev/null` over a
canonicalized path" idea for absent deny paths, but only if our policy requires
it. The simpler first version can refuse an absent deny path instead of creating
a mount point; that is more fail-closed.

## 2. `nono`

### 2.1 Capability model

- `crates/nono/src/capability.rs:89` — `AccessMode` is `Read`, `Write`,
  `ReadWrite`. `Write` is **write-only**; `contains()` makes `ReadWrite` the
  superset. This confirms that `WriteOnly` is not a theoretical capability.
- `:121` — `FsCapability` carries `original` and `resolved` paths, `access`,
  `is_file`, and a `source` (`User`, `Profile`, `Group`, `System`). It
  **canonicalizes first**, then checks metadata on the resolved path, closing a
  TOCTOU window (`:155`).
- `:889` — `NetworkMode` is `Blocked`, `AllowAll`, `ProxyOnly`. `IpcMode` and
  `SignalMode` exist as separate scopes. These are later capabilities; the
  first apexe release can keep `paths:` filesystem-only.
- `:1489, 1552, 1566` — directory capabilities are preserved so platform deny
  rules can still be generated; `Write`/`ReadWrite` are treated as write. This
  is the source of the "a grant plus a deny" model.
- `crates/nono/src/sandbox/macos.rs:522` — `(deny default)` baseline;
  `:253-:276` — emit the resolved and original spelling for a path; `:209-:245`
  — `collect_symlink_hops` includes the symlink itself and both parents;
  `:24`/`:51` — private `sandbox_init` and `sandbox_extension_issue_file` FFI.
- `crates/nono/src/sandbox/macos.rs:379-:413` — network is denied by the base
  profile unless a specific network rule is added; the `tool-sandbox` layer can
  add `ProxyOnly` ports.
- `crates/nono/src/sandbox/linux.rs:9` — Landlock `AccessFs`, `AccessNet`,
  `PathBeneath`, `Scope`; `:47` — `NoNewPrivs`; `:257` — `restrict_self`.
- `docs/cli/internals/seatbelt.mdx` — the profile structure, "allow discovery,
  deny content" for credentials, `process-exec*` and `file-map-executable` per
  readable path, and the note that `sandbox-init()` is private but same
  underlying mechanism as `sandbox-exec`.

**Adopt:** `WriteOnly` as a real capability; `original` + `resolved` +
`is_file` in the normalized path; canonicalize before metadata; `deny default`;
per-path `process-exec*`/`file-map-executable` on macOS; dual spelling;
`DenyWithinGrant` only where the backend can express a deny under a grant.

**Reject / do not assume:** `nono`'s `Profile`/`Group`/`Source` policy model,
agent tool profiles, keychain/Mach exceptions, and credentials/proxy layers are
its product, not apexe's. Do not depend on the private `sandbox_init` API as the
only macOS path; keep the VM/container fallback and the deprecation risk visible.

### 2.2 `systemd-run --user` is not a path backend

Actual probe on Debian 12 / systemd 252: a `systemd-run --user` service with
`-p ReadOnlyPaths=$HOME/denied` wrote into `$HOME/denied`; `-p InaccessiblePaths`
was likewise ignored. The same `systemd-run` **system** service (root) refused
both writes with `Read-only file system`. The user manager accepts the
properties but does not enforce them. Linux local path enforcement therefore
uses `bwrap`; `systemd-run` is system/root only.

### 2.3 `no_access` is stricter than "allow discovery, deny content"

`nono`'s sensitive-path policy allows `file-read-metadata` (stat/readdir) and
denies `file-read-data` (content). That is a reasonable UX policy, but it is
**not** `NoAccess`: apexe's `no_access` means neither read nor write, and a
caller that can `stat`/list a directory has read metadata. The contract must
keep the two distinct:

- `NoAccess`: deny content **and** metadata (`file-read-data` and
  `file-read-metadata`), plus writes.
- "Allow discovery, deny content": a separate policy, not expressible as
  `NoAccess`; only add it if operators ask for it, under its own name.

**0.15.0 amendment.** The strict version above is only expressible by Seatbelt.
`bwrap --tmpfs`, systemd `InaccessiblePaths`, and Docker's empty mount hide the
protected content and entries but leave a stat-able placeholder at the path.
Those backends therefore declare `no_access` (content/entries) but not
`no_access_metadata`; the contract and tests must not claim otherwise. A true
metadata-denying mount backend remains future work.

### 2.3 `DenyWithinGrant` is backend-specific

`nono`'s Linux backend is Landlock (an allow-list). It can grant rights, but a
`no_access` subtree inside a granted `read_write` root is not expressible with
Landlock alone; `nono` handles credentials via its platform rules and profile
layer, not by pretending Landlock has a deny-within-grant. The `sandbox.md`
capability must be declared per backend:

- `bwrap`, `systemd-run` (system/root), `sandbox-exec`: can express `DenyWithinGrant` via
  mounts/rules/Seatbelt.
- Landlock-only backend: **cannot**; it must refuse a policy that needs
  `no_access` inside a granted root rather than silently dropping the entry.

### 2.4 Execution and audit shape

- `crates/nono-cli/src/tool-sandbox/launch.rs` — a `LaunchSpec` is written to a
  `0600` file and passed to a re-exec'ed launcher via an env var; the child is
  executed with a filtered env; the launcher's `HOME` never reaches the child.
  `exit_status_code()` maps `128 + signal`.
- `crates/nono-cli/src/tool-sandbox/protocol.rs` — a typed launch spec, not a
  shell string.
- `crates/nono/src/audit.rs` — audit is part of the library, not a side log.
- `crates/nono-cli/src/tool-sandbox/shim.rs` — per-tool shim mediation.

**Adopt:** `LaunchSpec` as typed data; a `0600` handoff if a backend needs one;
filtered child env; `128 + signal`; audit as a first-class output. Do not copy
the whole `tool-sandbox`/profile/credential layer.

## 3. OpenAI Codex

The sparse checkout contains the sandbox plumbing, not the whole
`codex-sandboxing` crate that builds the profile. Findings are therefore
partial and should not be treated as a complete implementation review.

- `codex-rs/core/src/sandboxing/mod.rs` — `SandboxType`, `SandboxExecRequest`,
  `PermissionProfile`, `ExecRequest`; the core-owned adapter translates a
  transformed sandbox request back into an `ExecRequest`; the actual policy
  selection and command transformation live in `codex-sandboxing`.
- `codex-rs/cli/src/debug_sandbox/seatbelt.rs` — `DenialLogger` streams macOS
  sandbox violation events, attributes them by PID, and maps them to
  `SandboxDenial { name, capability }`. This is a useful **audit/diagnostic**
  pattern, not a policy translator.
- `codex-rs/execpolicy/src/` — command policy / sandbox migration. It is
  agent-facing policy, not a reuse candidate for `paths:`.
- `codex-rs/core/src/windows_sandbox*.rs` — Windows-specific; out of scope for
  apexe (Unix-only).
- `codex-rs/cli/src/sandbox_setup.rs` / `sandbox_uninstall.rs` — setup and
  uninstall are explicit, not hidden in the wrapper.

**Adopt:** the denial logger/attribution idea, explicit setup/uninstall, and
the separation of `PermissionProfile` from the low-level `SandboxType`. Do not
adopt the agent-specific policy and approval model.

## 4. Gemini CLI

- `packages/core/src/sandbox/macos/seatbeltArgsBuilder.ts:67` — every denied
  path is `resolveToRealPath()`'d; `:73-:75` emits a deny for the original and,
  when different, the real path. `:96` resolves `os.tmpdir()`; `:110` resolves
  the node root; `:123` resolves PATH entries; `:160` resolves governance files.
- `:89-:93` — workspace read/write rules are emitted for both original and
  resolved spellings; `:96` tmp is resolved; `:143-:149` policy paths are
  read/write.
- `packages/core/src/sandbox/linux/bwrapArgsBuilder.ts:28` — `--unshare-all`,
  `--new-session`, `--die-with-parent`; `:34` `--share-net` when network is
  allowed; `:38-:46` `--ro-bind / /`, `--dev /dev`, `--proc /proc`,
  `--tmpfs /tmp`; `:66-:75` workspace `--bind-try`/`--ro-bind-try` for both
  spellings; `:88` policy read-only binds; `:93` policy write binds.
- `packages/core/src/policy/sandboxPolicyManager.ts` and
  `packages/cli/src/utils/sandbox.ts` — selection and policy management.
- `docs/cli/sandbox.md` — methods are `sandbox-exec`, `bwrap`, Docker, Podman,
  `runsc`, LXC; profiles are named (`permissive-open`, `restrictive-open`,
  `strict-open`, etc.); selection precedence is flag, env, settings.

**Adopt:** the dual-spelling emission, bwrap flag set, and the explicit
selection/fallback surface. **Do not** copy the hardcoded governance file list
(`.git`, `.gitignore`, ...) into apexe: that is a policy decision, not the path
policy. It belongs in `config.yaml`/an overlay, not in the backend translator.

## 5. `bubblewrap`

- `README.md:111-:116` — the minimal invocation: `--ro-bind /usr /usr`,
  `--proc /proc`, `--dev /dev`, `--new-session`.
- `README.md:145` — PID namespace; `:148` — network namespace;
  `:152` — seccomp; `:158-:169` — `--new-session`/TIOCSTI and bind hazards;
  `:193` — unprivileged userns.
- `bwrap.c`, `bind-mount.c`, `namespace.c`, `tests/` — argv parsing, mount
  ordering, user namespace, and the `--args` file path.

**Adopt:** `--unshare-all`, `--new-session`, `--die-with-parent`,
`--ro-bind / /`, `--bind`/`--ro-bind`, `--tmpfs`, `--dev`, `--proc`,
`--cap-drop ALL`, `--args`, `--seccomp`. Prefer `--dev` over `--dev-bind /dev`
for the minimal device set. Keep the bwrap path configurable and resolve it from
the clean PATH.

## 6. Contract amendments

These are concrete changes to [`docs/sandbox.md`](../sandbox.md); none are
implemented yet.

1. **Normalized path type.** Add a `PathSpelling { original, resolved, is_file }`
   produced by a pure normalization step before `translate`. A literal backend
   emits both spellings; a mount backend uses the resolved path and the
   original spelling for the mount. The probe must test both. This is the
   `/tmp` → `/private/tmp` lesson from `sandbox-runtime`, `nono` and Gemini.
2. **`NoAccess` denies metadata too.** State that `NoAccess` means no content
   and no metadata; the "allow discovery, deny content" policy in `nono` is a
   different policy and must not be labelled `NoAccess`.
3. **`WriteOnly` is real.** Keep `WriteOnly` in `Capability`; `nono`'s
   `AccessMode::Write` is a working model. A backend that cannot express it
   refuses.
4. **`DenyWithinGrant` is per backend.** Landlock-only backends cannot express a
   deny under a granted root; they must refuse, not silently drop. `bwrap`,
   `systemd-run` (system/root) and Seatbelt can express it.
5. **`bwrap` translation.** Use `--unshare-all`, `--new-session`,
   `--die-with-parent`, `--ro-bind / /`, `--dev`, `--proc`, `--tmpfs /tmp`,
   `--cap-drop ALL`, `--args`; bind the workspace and policy paths; resolve
   symlinked deny paths; clean synthetic mount points or refuse absent deny
   paths.
6. **`systemd-run` translation.** Use `ReadOnlyPaths`, `ReadWritePaths`,
   `InaccessiblePaths`, `BindPaths`, `NoNewPrivileges`, `PrivateTmp`, and
   `--pipe --wait` **in a system service (root)**. The user manager accepted the
   path properties and silently ignored them in the 0.15.0 probe (Debian 12 /
   systemd 252); `systemd-run --user` is not a path backend.
7. **Seatbelt translation.** Generate `(deny default)`; emit both spellings;
   add `file-write-unlink`/`file-write-create` for the move bypass; bound the
   profile; use `sandbox-exec` (deprecated) or the `nono` private
   `sandbox_init` as a candidate, not as a permanent API.
8. **Execution shape.** `LaunchSpec` and `SandboxChild` are confirmed; the
   adapter protocol needs `execute/wait/kill/cleanup`, not only `translate`.
   The `nono` typed launch spec and `0600` handoff are useful references.
9. **Selection.** Use module IDs in `sandbox.modules`, not command names. The
   Codex/Gemini selection/approval layers are references; do not duplicate
   `acl.yaml` with a second policy.
10. **Network/credentials are later.** `sandbox-runtime` and `nono` show the
    shape, but the first filesystem release should refuse/deny network rather
    than claim it. Containers default to `--network none`.

## 7. Conformance cases derived from the sources

Add these to [`docs/sandbox-testing.md`](../sandbox-testing.md):

- **Canonical paths:** `/tmp`, `/private/tmp`, `/etc`, `/private/etc`, `/var`,
  `/private/var`, symlink target, dangling symlink, `..`, `~`, trailing slash,
  glob vs literal, file vs directory.
- **Dual spelling:** a write through the original spelling is blocked by a deny
  on the resolved spelling, and vice versa where the backend is literal.
- **`NoAccess`:** content and directory entries are denied. Metadata (`stat`)
  is denied by Seatbelt; mount backends expose an empty placeholder (see the
  0.15.0 amendment above).
- **`WriteOnly`:** write succeeds; read fails; a directory and a file.
- **`DenyWithinGrant`:** `read_only /var` + `read_write /var/log` +
  `no_access /var/log/secret`; a backend without the capability refuses.
- **A/B/C:** denied write fails; permitted write succeeds; computed path write
  fails at the kernel.
- **bwrap:** `--unshare-all`, `--new-session`, `--die-with-parent`,
  `--ro-bind / /`, `--dev`, `--proc`, `--tmpfs /tmp`, `--cap-drop ALL`,
  `--args`; a missing deny path refuses or is cleaned.
- **Seatbelt:** `(deny default)`; read order; write order; `file-write-unlink`/
  `file-write-create`; profile size; both spellings.
- **systemd-run:** `ReadOnlyPaths`/`ReadWritePaths`/`InaccessiblePaths` in a
  system service (root); `--pipe --wait`. `--user` ignores path properties.
- **selection:** `cli.ls` never wrapped; selected `cli.sed` wrapped; external
  caller gets the container backend.
- **audit:** backend, enforcement, outcome, policy digest, reason; probe record.
- **adapter:** `describe/probe/prepare/execute/wait/kill/cleanup`; signed/
  allowlisted; no silent drop; one spawn.

## 8. What not to copy

- `sandbox-runtime`'s Node implementation, credential masking, MCP/network
  proxies, Windows/UNC handling; keep them as later features.
- `nono`'s profile/group/source model, agent tool profiles, keychain/Mach
  exceptions, credentials and proxy layers; use the capability model and
  enforcement primitives as reference.
- Codex's agent policy/approval and Windows sandbox; use the denial logger and
  setup/uninstall separation.
- Gemini's hardcoded governance file list; that is policy, not translation.
- Any backend's source without an explicit license/attribution decision.

## 9. Open decisions after the source reading

1. Is `nono` a **buy/integrate** for the sandbox layer, a **library** for the
   primitives, or only a reference? The macOS private API and the Linux
   Landlock `DenyWithinGrant` gap matter.
2. Do we add `Network`/`Ipc`/`Signal` capabilities in the first contract, or keep
   the first release filesystem-only and default containers to `--network none`?
3. Does the first macOS backend use `sandbox-exec` (deprecated, OS, no install)
   or `nono` (`sandbox_init`, private) or a VM/container? The source does not
   remove the deprecation risk.
4. Does `bwrap` or `systemd-run` get the first Linux translator? `bwrap` is the
   local path backend; `systemd-run` is system/root only.
5. Does `NoAccess` deny metadata (recommended, strict) or allow discovery
   (reject as the default)?
