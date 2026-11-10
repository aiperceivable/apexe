# Managed execution isolation domain

This is the proposed v1 contract for managed sessions. Current
local execution is described in the [threat model](../threat-model.md).
The [policy specification](policy.md) owns operation and resource authorization.
The [session protocol](session-protocol.md) owns request authentication,
approval and call state.

## Requirements

**ISO-1 — Trust boundary.** A trusted launcher owns the policy snapshot, the
runtime image, workspace provisioning, session identities and the audit sink.
The launcher remains outside the session. The untrusted agent runs inside it.
The trusted relay and tool workers use host-mapped identities distinct from
the agent's and from each other's. Workers inherit the session restrictions
and receive a narrower filesystem view. The agent reaches the launcher only
through its authenticated session endpoint. The v1 runtime exposes no external
network, host tools, host runtime socket or unrestricted apexe entry point.
This profile supports offline or fixture-driven agents. A mediated model
connection requires its own deployment specification.

**ISO-2 — Filesystem view.** Provision a new session-owned workspace from
operator-approved regular files and directories. Use a bounded, private
filesystem and copy bytes into newly created objects. Apply the policy's
path validation to materialized names. Resolve each input relative to an
approved root without following symlinks or crossing mounts.
Reject multiply linked files, special files, protected descendants and any
policy that requests a protected subtree inside an exposed writable root.
Approval of imported data is the operator's trust decision. Provisioning uses
the approved manifest and protection rules, never a guess about which files
contain secrets. Do not bind-mount a live host workspace, copy host credentials
or write changes back automatically. The agent sees the projected workspace
through a read-only mount. Managed read-only workers see only what the call
authorizes. Only an authorized worker receives the writable view needed
for its operation. Mount data and scratch `nosuid,nodev,noexec`.
Expose a curated `/etc/hosts` fixture through an immutable reference if the
profile needs it. Give each role private scratch space. Keep policy, audit
and the relay endpoint outside the tool view.

**ISO-3 — Linux startup order.** The trusted bootstrap creates private
`user`, `mount`, `pid`, `ipc` and `net` namespaces, installs dedicated UID
and GID mappings, makes mounts private and sets up the filesystem view.
Workspace readers and the copy worker use a session-private primary GID
for data sharing without sharing UIDs. The bootstrap
pivots into that view and detaches the old root. A trusted PID 1 reaps
descendants. There is no procfs or sysfs mount in v1. Use a bounded
`/dev/shm` private to each role, a minimal private `/dev`, and a network
namespace with no configured interfaces or routes, including no active
loopback. After privileged setup,
drop bootstrap capabilities, supplementary groups and elevated credentials.
Set `PR_SET_NO_NEW_PRIVS` **before** calling `landlock_restrict_self` on the
unprivileged path. Apply the role's Landlock rules, process-control filters
and descriptor allowlist before any untrusted code or child thread runs.
The filters deny namespace and mount changes, ptrace, cross-process memory
access, descriptor acquisition and System V IPC. Allow only the socket
families required by each role. Exclude external socket paths from the view. Workers have no
socket creation permission. A trusted supervisor creates workers as siblings
of the agent under the session ceiling, then narrows each worker's access
for its call. Any missing capability, failed setup step or failed readiness
probe aborts startup. [Landlock setup and inheritance](https://docs.kernel.org/userspace-api/landlock.html#enforcing-a-ruleset)

**ISO-4 — Process and descriptor boundary.** Use distinct host identities
across sessions and between untrusted code and the relay or launcher.
Prevent cross-role inspection, signals and descriptor acquisition even on
hosts with permissive ptrace settings. Keep control processes non-dumpable
where applicable. Descriptors are created close-on-exec. The trusted bootstrap
explicitly maps the allowed descriptors and closes all others in the child
with `close_range` before exec. A normal tool receives only stdin, stdout and
stderr, with stdin closed or attached to `/dev/null` in v1 and both outputs
piped to the supervisor.
The trusted relay's explicit bootstrap descriptor is governed by the
[session protocol](session-protocol.md). No caller-supplied or inherited host
directory descriptor, terminal, credential socket or control connection may
reach an untrusted process. The legacy client and direct execution endpoints
are absent from the managed runtime. A managed client with a missing or
invalid channel fails closed.

**ISO-5 — Resource limits.** Default each call to 10 seconds elapsed and
1 MiB combined stdout and stderr. The output budget applies to streamed and
captured results. The launcher enforces a 30-minute session limit, a 32 MiB
aggregate tool output budget and one in-flight call. The session cgroup v2
ceiling is 64 tasks, 512 MiB memory with swap disabled and a quota of one CPU.
Workspace plus scratch uses at most 256 MiB and 16,384 inodes. The operator may
set other finite positive limits. The agent cannot change them. Reject a
deployment unable to enforce them. Drain streams with bounded buffers and
enforce wall time even under output backpressure. On a call limit or caller
disconnect, terminate and reap the entire call subtree. On session teardown,
terminate and reap all session processes, close channels and release mounts.
Use a dedicated cgroup for each call and keep cgroup controls outside
untrusted views so descendants cannot move out of their limit or cleanup
scope.
Return an explicit limit status, with a possible-partial-effects indicator
when a writer had started. [Control group v2](https://docs.kernel.org/admin-guide/cgroup-v2.html)

## Acceptance

| ID | Requirements | Linux integration assertion |
| --- | --- | --- |
| ISO-A1 | ISO-2, ISO-3, ISO-4 | A test worker reads an allowed fixture and writes within its granted root. The same worker attempts an outside write after successful preflight. The operation reports failure, the protected object is unchanged, and the authorized effect occurred. |
| ISO-A2 | ISO-1, ISO-4 | The agent and worker roles find no procfs or sysfs in their view, so `/proc/<sibling>/root` is unreachable by construction; assert the absence rather than probing a path that cannot exist. From an outside sibling, assert that the launcher's and worker's descriptors cannot be inspected or acquired under the supported host configuration. |
| ISO-A3 | ISO-3 | The same `/dev/shm` name in two roles or sessions resolves to different storage. A session cannot connect to an abstract Unix listener outside its network namespace or to an outside IP listener. Each listener has a positive control. |
| ISO-A4 | ISO-1, ISO-4 | A probe enumerates descriptors and attempts to use the control channel from a worker. Only the permitted descriptors are present and the worker cannot invoke the launcher. Direct legacy invocation and an invalid or missing managed channel fail. |
| ISO-A5 | ISO-5 | Output flood, blocked output consumer, hanging child and process-tree fixtures hit the corresponding limits. Assert bounded output and elapsed time, an explicit reason, and no surviving descendants. |
| ISO-A6 | ISO-2 | A requested live workspace, protected descendant, symlink, hard link, mount crossing or unsupported backend fails before session start. Imported mutations never modify the input tree. |
| ISO-A7 | ISO-3 | The readiness probe asserts `PR_GET_NO_NEW_PRIVS == 1` and that a denied syscall probe actually fails. A bootstrap variant that skips `PR_SET_NO_NEW_PRIVS`, or applies Landlock before it, must abort startup rather than launch untrusted code; a variant that leaves procfs or sysfs mounted must fail readiness. |

## Release boundary

Linux namespace and Landlock integration tests are release gates, including
positive controls for the attack fixtures and negative controls for incomplete
isolation. SBPL is a macOS prototype backend. The macOS probe from this design
review exercised selected file operations on 26.3, build 25D125. It did not
validate the Linux deployment or the complete managed-session profile. Run the
Linux backend in a VM on macOS for the v1 managed profile. Failure to start
the approved backend ends the session.

Shadow mounts and live host workspaces are deferred. Before supporting them,
test name visibility, ancestor rename and removal from both the host and
namespace, symlink and mount aliases, and partial recursive deletion on the
target Linux backend. A protected subtree surviving a partial deletion is
an isolation result. An all-or-nothing preflight refusal must be tested
separately before spawn.

Before admitting rename, test both directions as a non-root worker with no
DAC override. A `0444` source with a writable parent can be renamed under
an otherwise valid grant. A `0666` source with a `0555` parent cannot.
Assert the returned status and the resulting directory entries in both cases.
