# Managed execution policy and backend capabilities

Status: proposed v1 specification. The current [local execution](../features/v2-f8-local-execution.md) and [governance](../features/v2-f5-governance.md) contracts remain separate. This document defines managed authorization. The [isolation domain](isolation-domain.md) defines enforcement and budgets, and the [session protocol](session-protocol.md) defines how clients submit and execute an authorized plan.

## Requirements

- **POL-01** The launcher shall pin a validated policy, approved resource map, command catalog and backend capability report before accepting requests. Unknown, malformed or ambiguous configuration shall prevent activation.
- **POL-02** The launcher shall authorize the exact operation form and every declared effect within the intersection of the session and form grants. Existing apcore ACLs may impose additional module admission requirements. An ACL allow shall never widen a resource grant.
- **POL-03** The selected backend and controlled worker shall together enforce every promised restriction. The compiler shall reject unsupported policies before execution. Approval cannot expand the compiled authority.
- **POL-04** The launcher shall reject non-declared and unclassified forms in v1. It shall bind declared effects to concrete resource IDs and resolved paths before accepting a plan.
- **POL-05** Publication by `builtin.copy_create` shall never replace an existing directory entry, including one created after planning. The operation shall either publish a complete file or report failure. A lost result after publication may leave a complete file with an unknown outcome.

## Resource and effect model

A resource is a trusted, launcher-defined root with a stable opaque ID and a role. V1 defines a session-private `workspace` and zero or more immutable `reference` resources. The launcher materializes them as described in the [isolation domain](isolation-domain.md#requirements). References may represent individually selected files such as a copy of `/etc/hosts`. Policy and request paths use a root ID plus a relative path. They never select a live host path. The runtime image is a separate system input and cannot be named as an operand. The agent can read the workspace projection directly. Session and form grants constrain managed calls and do not hide data already exposed to the agent by its view.

Path components are UTF-8, 1–255 bytes, with no NUL, C0 controls, DEL, separators, empty components, `.` or `..`. Paths have at most 64 components and at most 4,095 bytes in their slash-joined representation, leaving room for the syscall NUL terminator. The empty string names the root. Resolve beneath the pinned root using `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV` or an equivalent verified mechanism. Check object types on opened handles. For a future entry, resolve and pin its existing parent and validate the final component separately. Unsupported object types, missing parents, and symlink traversal fail planning. Execution revalidates resource identities and assumptions before use. Publication shall also enforce no replacement at the filesystem operation.

A normalized effect has an operation, a resource-relative object locator and an object kind. A directory entry locator consists of a parent locator plus a basename. A move or exchange carries both entry locators in one correlated operation. The effect's range is separate from object kind. An exact object, immediate directory listing, or subtree describes which objects may be affected. Moving a directory can affect a subtree without a recursive option.

| Effect | Object and range | Linux view and Landlock | SBPL prototype | V1 exposure |
| --- | --- | --- | --- | --- |
| `inspect_metadata` | File or directory, exact object | Show the object in the call view. Landlock alone does not gate general `stat`. | `file-read-metadata` candidate | Required by reviewed read forms |
| `list_dir` | Directory, immediate entries | Mount a read-only directory and grant `READ_DIR`. Command form forbids recursion. | `file-read-data` and metadata operations, subject to probe | `cli.ls` |
| `read_file` | Regular file, exact object | Expose approved object read-only and grant `READ_FILE`. | `file-read-data` and required metadata | `cli.cat`, `cli.head`, `builtin.copy_create` |
| `create_entry` | Parent plus new regular-file name | Writable destination parent, `MAKE_REG`, and a typed publication operation that enforces the exact name and no replacement. | `file-write-create` candidate, paired with a no-replace primitive | `builtin.copy_create` |
| `write_file` | Existing file content | `WRITE_FILE`, separately from truncate. | `file-write-data` candidate | Private copy staging only |
| `truncate_file` | Existing file content | `TRUNCATE` on ABI 3 or later. | `file-write-data` candidate. Exact write versus truncate enforcement is unverified. | Reserved |
| `remove_entry` | Parent plus child name and type | `REMOVE_FILE` or `REMOVE_DIR` on parent. Exact per-name control requires a typed operation or a view that supports that boundary. | `file-write-unlink` candidate | Private copy staging only |
| `rename_entry` | Source and destination entries, with affected range | Source removal and destination creation rights, plus `REFER` on both parents for cross-directory rename. Replacement separately requires rights over the old destination entry. | Create/unlink combination is a candidate. Probe each supported form. | Private no-replace publication only |
| `modify_metadata` | File or directory attributes | View and worker restrictions are required. Landlock alone cannot gate general `chmod`, `chown` or `setxattr`. | Relevant `file-write-*` suboperation must be validated | Reserved |
| Recursive effects | Explicit affected subtree | Projection and worker enforce the boundary. Landlock allows are additive within a layer and cannot subtract a child rule from an allowed parent. | Emit validated ancestor grants before child denials for supported overlaps | Reserved |

The Linux backend shall verify required kernel rights, mount operations, syscall restrictions and the publication filesystem before activation. For the v1 copy profile, require Landlock ABI 3 or later, an effective `TRUNCATE` denial on exposed data, and the namespace and isolation checks in the [isolation domain](isolation-domain.md#requirements). The readiness probe shall exercise the complete descriptor-relative `renameat2(RENAME_NOREPLACE)` flow from private staging to a destination on the chosen filesystem under the final mount topology, role identities and Landlock rules. This includes `REFER` on both parents and Landlock's no-rights-gain constraint on reparenting. Probe successful publication and refusal to replace an existing target. Mapping a policy effect can require several controls. Directory-granularity Landlock rights do not enforce per-name authorization on their own. See [Landlock access rights](https://docs.kernel.org/userspace-api/landlock.html#access-rights) and [Landlock policy layers](https://docs.kernel.org/userspace-api/landlock.html#layers-of-file-path-access-rights).

The SBPL backend is a prototype. The recorded macOS 26.3 probe verified that a child `file-write*` denial applied when emitted after a matching parent grant, and the tested operations succeeded with the order reversed. The generator shall emit those supported child denials after applicable grants and pin that combination with create, truncate, unlink and rename probes. No universal rule-ordering guarantee is inferred. Unsupported combinations and unverified operations shall be reported as unsupported. SBPL shall not activate a v1 managed session.

## Policy input and compilation

The policy is a single UTF-8 JSON object of at most 64 KiB. Accept exactly these fields, reject duplicate keys, and require all listed fields. Policy inputs are supplied by the operator to the launcher. Clients cannot submit a policy, choose a backend or add roots.

| Field | Shape | Validation |
| --- | --- | --- |
| `version` | integer | Exactly `1` |
| `session_grants` | array of grants | 1–64 entries. No duplicates. |
| `forms` | array of form rules | 1–5 rules. Exactly one rule per listed form. Omitted forms are denied. |
| `forms[].id` | string | Unique stable rule ID matching `[a-z][a-z0-9_-]{0,63}` |
| `forms[].form` | string | One of the v1 form IDs below |
| `forms[].grants` | array of grants | 1–64 entries. No duplicates. |
| `forms[].approval` | string | `none` or `required` |

A grant is `{"root":"workspace","prefix":"output","ops":["create_entry"]}`. Accept exactly those three fields. Root IDs match `[a-z][a-z0-9_-]{0,63}` and must appear in the launcher's resource map. `prefix` follows the path rules above. `ops` is a non-empty set of v1 operations `inspect_metadata`, `list_dir`, `read_file`, and `create_entry`. Reject duplicate operations. Match whole path components. For `create_entry`, match the final child path while deriving backend permissions from its parent. Never grant creation on an immutable reference. For each operation in a form grant, require a session grant with the same root and an equal or ancestor prefix that grants that operation. Required effects may match separate grants.

A root's contents are entirely within its approved projection. V1 rejects requested exclusions or nested protected descendants of that projection. To authorize a smaller data set, the operator supplies a separately materialized root. Deny-by-default applies at both levels. No rule ordering, wildcard strings, mutable policy includes, or host baseline carve-outs participate in managed compilation.

The proposed `apexe managed policy-check --backend linux|sbpl --file FILE [--root ID[:reference]]` performs validation and reports the backend, detected capabilities, and each unsatisfied restriction. It lives under `managed` rather than `policy` because `apexe policy` already names the path-guard boundary, and two unrelated policies behind one verb is how an operator checks the wrong one. A successful capability probe is required for launch, even after a passing check. The policy identity is the SHA-256 digest of the canonical policy representation as defined in the [session protocol](session-protocol.md#canonical-representation).

## Managed forms

The [cli-permissions scope](https://github.com/aiperceivable/cli-permissions/blob/main/SCOPE.md) owns verifiable command facts. The launcher pins the reviewed runtime image, binary identity, variant and grammar revision. The GNU forms below use the [ls](https://github.com/aiperceivable/cli-permissions/blob/main/overlays/ls%40gnu.json), [cat](https://github.com/aiperceivable/cli-permissions/blob/main/overlays/cat%40gnu.json), and [head](https://github.com/aiperceivable/cli-permissions/blob/main/overlays/head%40gnu.json) evidence plus apexe tests for the constrained forms. Overlay annotations and old local bindings cannot confer managed authority.

| Form ID | Typed operands | Declared effects |
| --- | --- | --- |
| `cli.ls.list` | One directory. No caller flags, recursion, inherited environment or stdin. | Inspect that directory and list its immediate entries |
| `cli.cat.file` | One regular file. No caller flags or stdin. | Inspect and read that file |
| `cli.head.lines` | One regular file and integer `lines` in 1–1000. No stdin. | Inspect and read that file |
| `builtin.copy_create.to_path` | One regular source and one new destination entry | Inspect and read source, inspect destination parent, create the destination entry |
| `builtin.copy_create.into_dir` | One regular source and one existing destination directory | Same effects, with destination entry derived as directory plus the source basename |

For native read workers, construct fixed argv from the typed form and worker view. Use GNU `ls -1 -- PATH`, `cat -- PATH`, or `head -n N -- PATH`. `PATH` is a launcher-assigned absolute path in the worker view, so an operand named `-` cannot switch a read to stdin. No raw argv, flags from input, recursive reads or shell fragments reach workers. Each worker starts with the selected object pinned in its read-only call view and has no control-channel access. `list_dir` declares a known directory and bounded enumeration operation. Granting it permits disclosure of immediate entry names independently of any grant to read their contents. The individual returned names are runtime data and do not enlarge the grant.

`builtin.copy_create` is an apexe primitive with its own source and test provenance. It shall stage bytes in an exclusively created `0600` regular file in launcher-owned private staging on the destination filesystem. The staging area shall be absent from the agent view. Use the session-private primary GID shared with workspace readers. Set the completed staged file to `0640` using the open staging handle and preserve no source metadata. Publish only after the bounded copy completes using descriptor-relative `renameat2(RENAME_NOREPLACE)`. The worker shall hold the pinned staging and destination parents, and the trusted primitive shall use only the planned basename. Stage, permission setting and cleanup authority shall be limited to that call's private staging and omitted from the agent-visible grants. `EEXIST` is `DESTINATION_EXISTS`, with the prior object untouched. If a process dies after the publication point and its result is unavailable, report `OUTCOME_UNKNOWN` per the [session protocol](session-protocol.md#execution-and-approval). Apply the [isolation budget](isolation-domain.md#requirements) to streaming output and copy staging.

`declared` means the authorized resource objects and bounded operation shapes are known at planning. `scope_only` means the verifier can bind an upper scope but runtime determines object-level effects, such as a `find -delete` predicate. `unclassified` means there is no reviewed effect contract. **`opaque` is the `scope_only` case the corpus already names**: an overlay's `x-apexe-opaque-after` marks the token from which a tool stops interpreting its own argv, so `node tsc.js --noEmit` and `python3 -m <module> <args>` forward a tail whose effects cannot be derived at planning. V1 permits only `declared` forms and rejects `scope_only` and `opaque` even if isolated, which is why a shimmed interpreter admitted by `apexe env --enable --also node,python3` has no managed form. Legacy local bindings stay on the existing unmanaged surface. Directory-entry commands, recursive copying, overwrite, exchange, cross-filesystem moves and generic shell execution are outside the v1 catalog.

**V1 has no form that writes caller-supplied bytes.** `builtin.copy_create` reads an existing resource file; the workspace projection is read-only to the agent. A managed agent can therefore read and copy, but cannot create a file from content it generated. That is a scope decision, not an omission: adding `builtin.write_new` means a bounded blob message on the session channel, staged and published through the same `RENAME_NOREPLACE` path, and it should be specified before a deployment expects an agent to produce code.

## Decision errors

| Error | Trigger |
| --- | --- |
| `POLICY_INVALID` | Invalid schema, duplicate rule, unknown root, illegal path, or form grant exceeding session grants |
| `BACKEND_UNSUPPORTED` | Selected backend or filesystem lacks a required capability, or the policy requests an unimplemented protected descendant or carve-out |
| `FORM_UNSUPPORTED` | Unknown, unclassified, `scope_only`, or unavailable reviewed form |
| `INVALID_ARGUMENT` | Bad operand shape, invalid relative path, unsafe resolution, wrong object type, missing object or parent |
| `RESOURCE_DENIED` | One or more effects fall outside the session grant, form grant, or additional ACL admission |
| `DESTINATION_EXISTS` | The target exists at planning or the no-replace publication reports a collision |

Report the code and phase and identify the rule or operand field when safe. Avoid reflecting a raw protected host path, requested content or environment values. Use the [session protocol](session-protocol.md#execution-and-approval) for approval, timeout and uncertain outcomes.

## Acceptance

| ID | Requirements | Release check |
| --- | --- | --- |
| POL-A1 | POL-01, POL-03 | Reject duplicate fields, unknown roots, a form grant wider than the session, a protected-descendant request, an unavailable kernel right and an unsupported publication filesystem. Run no child. |
| POL-A2 | POL-02, POL-04 | Allow the planned read of one explicitly projected reference. Deny writing to it, selecting an ungranted sibling, using a symlink or special file, and executing a recursive or opaque form. |
| POL-A3 | POL-02, POL-04 | Verify exact binary and variant pinning. Substitution after planning fails before spawn. An allow-all apcore ACL cannot make a missing effect grant pass. |
| POL-A4 | POL-03, POL-05 | Allow each copy form and verify complete output. Create a regular file or symlink at the destination after planning and before publication. Both runs return `DESTINATION_EXISTS` and preserve existing content and links. Interrupt the worker before publication and verify no destination is exposed. |
| POL-A5 | POL-03 | Start a test-only worker after a successful plan and induce an attempted out-of-scope syscall. Verify an explicit failure, a positive in-scope operation and an unchanged protected fixture. |
| POL-A6 | POL-03 | On the SBPL prototype, verify allow-then-deny against both a normal and a protected child. Reverse the pair as a negative control. An SBPL managed launch remains unsupported. |
