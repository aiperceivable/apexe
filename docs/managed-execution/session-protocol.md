# Managed execution session protocol

This is the proposed v1 protocol. The [isolation domain](isolation-domain.md) defines who can reach the protocol. The [policy](policy.md) defines which typed operations and resources are admissible. This document specifies the session channel, call lifecycle, approval and audit.

## Requirements

- **SES-01** A trusted launcher shall bind each channel to one session and a fixed caller context. Request data, environment variables, filesystem paths and `argv[0]` cannot assert identity or select policy.
- **SES-02** The launcher shall validate peer provenance and each request's sender, reject descriptor transfers, and prevent any control-channel descriptor from reaching a tool worker.
- **SES-03** A single launcher-created call ID shall correlate preflight, approval, execution and terminal audit records. A prepared plan can launch at most one execution. No error triggers an automatic rerun.
- **SES-04** Human approval shall arrive through a separate trusted channel and be bound to the exact plan, execution environment, session, expiry and one-time nonce. Approval shall never override a resource denial or backend capability requirement.
- **SES-05** Agent-visible results and durable audit records shall distinguish admission, denial, tool failure, partial failure and uncertain outcomes. Default audit shall avoid operand values, raw output and approval material.

## Channel and identity

The launcher is the sole policy and execution authority. It keeps the policy snapshot, capability report, approval state, audit sink and launcher-side handles outside the agent domain. A dedicated trusted relay in the session domain carries requests and responses. The relay shall run under the separate UID and process restrictions in the [isolation domain](isolation-domain.md#requirements). Session labels identify records. The authority derives from the launcher-owned channel and verified process identity.

1. The launcher creates an anonymous `AF_UNIX` `SOCK_SEQPACKET | SOCK_CLOEXEC` socketpair. It checks `SO_DOMAIN`, `SO_TYPE`, connectedness and `SO_PEERCRED` on the actual endpoints before handing one to the relay. No request supplies an FD number or endpoint. For a socketpair, `SO_PEERCRED` records creation-time credentials. Use that check for bootstrap provenance.
2. The launcher fixes the relay's host-view PID, UID and GID at bootstrap, holds a pidfd for it, and enables `SO_PASSCRED` on the launcher endpoint. For each received packet, verify its kernel-supplied `SCM_CREDENTIALS` against the expected sender in the launcher's namespace and verify that the pinned relay is alive. A `SO_PEERCRED` snapshot alone cannot identify a later holder of the descriptor. Namespace-translated IDs are interpreted in the receiver's namespace. The trusted relay bootstrap checks the original launcher's `SO_PEERCRED` in the host view before namespace entry and retains that verified endpoint. After entry it checks the expected translated snapshot before accepting traffic. A host PID may appear as zero and a host UID or GID may be unmapped inside the relay. The translated snapshot is corroborating evidence. The trusted endpoint handoff establishes provenance.
3. The relay serves a separate private pathname `AF_UNIX` `SOCK_SEQPACKET` endpoint visible to the agent role and absent from every worker view. The relay verifies each client's `SO_PEERCRED` against the dedicated session agent credentials and binds the connection to its peer PID and process lifetime using `SO_PEERPIDFD` or an equally race-free verified mechanism. A PID lookup by number alone is insufficient. It checks `SCM_CREDENTIALS` on each packet against that connection. The agent client verifies the relay endpoint and its expected UID/GID from trusted bootstrap configuration. Forward only validated protocol fields. The launcher derives the session from its own endpoint.
4. Use close-on-exec on accepted and created descriptors. The trusted bootstrap may explicitly retain one relay endpoint across a relay exec, then shall restore close-on-exec before receiving requests. The launcher and relay close unused endpoints before starting untrusted code. Before any tool exec, install sanitized stdin, stdout and stderr, close all other descriptors with `close_range(3, UINT_MAX, 0)`, and fail before exec if cleanup fails. A trusted builtin worker shall start without any control or approval descriptor and open only plan-scoped data handles in its worker view. Keep supervisor channels and endpoints out of worker mounts.
5. Accept exactly one credential control record where credentials are expected. Reject unexpected ancillary data, `SCM_RIGHTS`, `MSG_TRUNC`, `MSG_CTRUNC`, missing credentials and credential mismatches. Receive with `MSG_CMSG_CLOEXEC`, close any delivered descriptors before closing the connection, and record a bounded protocol refusal. Relay loss or channel-integrity failure invalidates unstarted plans and approval grants. No legacy fallback is permitted.

The implementation shall verify these rules when kernel ptrace policy is permissive. PID visibility, mapped UIDs, non-dumpability and endpoint isolation prevent a sibling process from acquiring launcher or relay descriptors. Peer checks provide a separate control. Linux documents the credential snapshot and per-message credential mechanisms in [unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html).

## Wire messages

One packet carries one UTF-8 JSON object with exactly these envelope fields. Reject duplicate or unknown keys, invalid UTF-8, invalid JSON or unsupported versions. Maximum packet size is 64 KiB, including envelope and encoded output. Request IDs are connection-unique opaque 128-bit values encoded as 32 lowercase hex digits. `id` matches the request on a direct reply. Call IDs use the same encoding and are assigned by the launcher. A session allows at most four concurrent agent connections and 1,024 requests per connection. The relay closes connections that exceed their request budget and rejects new connections at capacity. **The session admits one unfinished request in total** (isolation-domain.md ISO-5); the other connections exist so a running call can be queried or cancelled, and an attempt to `prepare` or `run` while the slot is held is refused with `RESOURCE_LIMIT`. Each connection carries at most one active output stream.

| Field | Type | Meaning |
| --- | --- | --- |
| `v` | integer | Exactly `1` |
| `type` | string | Message name below |
| `id` | string | Request ID for reply correlation |
| `body` | object | Exactly the fields defined for that message |

| Agent request | Body | Reply |
| --- | --- | --- |
| `prepare` | `form` string and `args` object | `prepared` with `call_id`, `plan_digest`, `effects`, `approval_required` and `expires_in_ms` |
| `run` | `call_id` and `plan_digest` | `started` with `call_id`, zero or more `output` packets, then one `result`, or an `error` before start |
| `status` | `call_id` | `state` with `call_id`, `state`, and `result` when terminal |
| `cancel` | `call_id` | `state` after acknowledging cancellation. Active execution may still be stopping. |

`args` contains `path` for list or cat, `path` and `lines` for head, and `source` and `destination` for copy. A path is `{"root":"workspace","path":"output/notes.txt"}`. The selected copy form determines whether `destination` is a new path or an existing directory. The [policy](policy.md#managed-forms) owns validation and effect derivation. `effects` is an ordered array of the plan's visible operations and resource-relative locators, including the derived destination. For v1 each effect is `{op, object, range}`. A file or directory object uses `{kind, root, path}`, with `kind` equal to `file` or `directory`. An entry object uses `{kind, root, parent, basename}`, with `kind` equal to `dir_entry` and `parent` a root-relative path. The range is `exact` or `immediate_entries`. Agent-facing previews shall not reveal protected host paths or launcher-private data. Only resource IDs and paths the caller is permitted to see appear.

Each `output` packet contains `call_id`, a zero-based increasing `seq` across streams, `stream` of `stdout` or `stderr`, and standard padded Base64 `data` containing at most 16 KiB before encoding. Supervise both streams from process start under the [isolation budget](isolation-domain.md#requirements). Streaming changes delivery timing. It does not alter limits. On limit or bounded queue exhaustion, stop and reap the worker and report the reason. Disconnect shall not cause the launcher to rerun a call or block indefinitely on a pipe.

An `error` body contains `code`, `phase` and `call_id` when allocated. `phase` is `channel`, `planning`, `approval` or `execution`. Safe optional fields are `field` and `rule_id`. Before authentication, close without reflecting request values. A `result` contains `call_id`, `status`, `mutation` and, when available, `exit_code` or `signal`. Status values are `succeeded`, `tool_failed`, `denied`, `cancelled`, `resource_limit`, `partial_failure` and `outcome_unknown`. Mutation values are `none`, `complete`, `partial` and `unknown`. An optional `code` supplies a more specific failure. Publish partial or complete mutation only when supported by trusted evidence. An arbitrary child failure alone is insufficient evidence of a backend denial.

## Canonical representation

Use [RFC 8785 JSON canonicalization](https://www.rfc-editor.org/rfc/rfc8785.html) and lower-case hexadecimal SHA-256. Reject duplicate keys before canonicalizing. All protocol integers are bounded to the JSON safe integer range. Canonicalization preserves Unicode strings exactly. The launcher shall construct canonical plans from validated typed data and resolved resource locators, never from the caller's original JSON or shell text. Normalize grant operations as lexically sorted sets and sort grants by root, prefix and operation list before computing the policy digest. Sort form rules by form ID.

The plan digest commits to the protocol version, session ID, call ID, policy digest, isolation manifest digest, operation form, ordered normalized effects, backend identity, runtime image and executable or builtin identity, plus effective resource budget, fixed argv for native forms and immutable resource identities. Worker-specific temporary identifiers do not participate. Keep resolved resource identities and sensitive details in launcher-owned state. A matching digest is necessary for execution and does not confer authority.

## Execution and approval

A plan expires 300 seconds after preparation using the launcher's monotonic clock. Expiry is terminal. A plan starts in `ready` or `pending_approval`. The launcher retains the immutable plan and governs every state transition. A successful human decision moves `pending_approval` to `approved`. A denial moves it to `denied`. An admitted `run` atomically transitions `ready` or `approved` to `starting` before any spawn. It then transitions to `running` or an explicit terminal result. A call reaches at most one terminal state. Keep it queryable for the rest of the session. The launcher retains at most 128 call IDs per session. Once that budget is exhausted, reject further preparation with `RESOURCE_LIMIT` and preserve existing call state.

The launcher sends `approval.request` to the trusted host approver over a distinct endpoint absent from the domain. Its body contains `session_id`, `call_id`, `plan_digest`, `policy_digest`, a 32-byte CSPRNG `nonce` encoded in hex, and expiry. The operator UI also receives a safe rendered summary of the plan. It returns `approval.decision` with the same identifiers, nonce, and `decision` of `approve` or `deny`. Validate the host peer on a launcher-created channel under a distinct operator identity. Agent frames cannot carry decisions. Do not send the nonce to the agent or persist it in audit. An approval grant expires at the earlier of plan expiry or 60 seconds after approval.

On `run`, check that the plan belongs to the calling session, the digest matches and the plan is unexpired. Revalidate backend availability, policy identity, executable identity, root and parent handles, actual source and destination assumptions, and current resource budget. An incorrect request digest returns `PLAN_STALE` without changing call state. If actual execution assumptions changed, make the plan terminal with `PLAN_STALE` or the more specific `DESTINATION_EXISTS`. In one launcher state transition, consume any required approval nonce and claim the call before launch. No nonce becomes reusable after spawn failure, cancellation or crash. A launcher restart creates a new session and invalidates all outstanding grants.

For interruption after a known mutation, report the corresponding mutation state. If the launcher cannot determine whether copy publication occurred, report `OUTCOME_UNKNOWN`. A client that loses its reply may query the original call ID. It shall reconcile the workspace before submitting a new mutating request when the outcome remains unknown. Server-side retries are forbidden.

| Code | Condition |
| --- | --- |
| `PROTOCOL_INVALID` | Malformed packet, version or field violation, or reused request ID |
| `PEER_DENIED` | Endpoint provenance, peer identity or credential violation |
| `CALL_NOT_FOUND` | Call absent or owned by another session |
| `PLAN_EXPIRED` | Plan or approval grant expired before execution |
| `PLAN_STALE` | Execution-time assumptions changed or supplied plan digest differs |
| `APPROVAL_REQUIRED` | Plan is pending and an execution request arrived before approval |
| `APPROVAL_DENIED` | Trusted human denied the call, or a required trusted channel is unavailable |
| `ALREADY_STARTED` | A replay attempts to run a claimed call |
| `RESOURCE_LIMIT` | A finite execution, session, streaming or workspace budget was reached |
| `ISOLATION_FAILED` | Required containment, descriptor cleanup or supervision failed |
| `OUTCOME_UNKNOWN` | Mutation may have committed and its outcome could not be verified |

Use [policy decision codes](policy.md#decision-errors) for authorization, invalid arguments and publication collisions.

## Audit

The launcher shall append records for planning verdict, approval decision, claim, refusal and terminal outcome. Use the same stable `trace_id` across the call lifecycle, with `call_id` and `session_id` as separate fields. Record policy version and digest, rule ID, form ID, effect kinds, opaque resource root IDs, verdict, phase, exit status when known, resource-limit class and mutation state. Mark planned effects as planned. Attribute runtime denial to the backend only when supported by trusted evidence. Keep the append sink and policy snapshot outside the agent domain. If the mandatory pre-execution audit event cannot be written, refuse to launch.

Do not write raw argv, path values, hashes of unkeyed operand values, approval nonce, credentials or tool output to the default audit. Deployment may opt into a separate restricted sink for resource-relative locators or a keyed digest with a separate key and defined retention. Keep the approval and audit records joinable without recording approval secrets. See the current [audit privacy baseline](../threat-model.md#47-audit-trail).

## Acceptance

| ID | Requirements | Release check |
| --- | --- | --- |
| SES-A1 | SES-01, SES-02 | Change `argv[0]`, environment and caller ID fields. Attempt a cross-session request, an injected FD and impersonation by a sibling process. Deny them without granting authority. Confirm the real client succeeds on the same test setup. |
| SES-A2 | SES-02 | Assert valid relay credentials across UID and PID translation. Reject a transferred or mismatched sender, malformed ancillary records and truncation. Inspect the worker from its supervisor and verify only intended I/O survives exec. |
| SES-A3 | SES-03, SES-04 | Approve one bound plan, race two run requests, and observe one launch. Replay a nonce, change the plan, and advance the clock beyond each expiry. None launches a worker. |
| SES-A4 | SES-03, SES-05 | Kill a worker before publication, then in a second test drop its result after possible publication. Verify different terminal mutation states and no automatic retry. Query each terminal result by original call ID. |
| SES-A5 | SES-02, SES-05 | Stream a fixture past the shared output cap, assert explicit termination and bounded delivery, and verify audit correlation across plan, approval and result. Check that paths, argv, stdout, stderr and nonce are absent from default audit records. |
