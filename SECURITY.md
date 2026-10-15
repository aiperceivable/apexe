# Security Policy

`apexe` mediates CLI execution and imported HTTP operations for humans and AI
agents. Its schema validation, configured policy, transport credentials and
audit records form an execution boundary, so flaws in those controls are worth
reporting carefully. Normal execution does not provide OS sandbox containment.

## Reporting a vulnerability

**Do not open a public issue.**

Use GitHub's private vulnerability reporting: the **Security** tab of this
repository → **Report a vulnerability**. If that is unavailable, email
**team@aiperceivable.org**.

Coordinated disclosure, supported versions, and crediting follow the
[organization-wide policy](https://github.com/aiperceivable/apcore/blob/main/SECURITY.md).
Only the latest release is supported; older versions get fixes on a best-effort
basis.

### Response times

`apexe` is maintained by a very small team. Expect acknowledgment within about a
week, and an initial assessment within two. A critical, reproducible bypass will
move faster than that — but the honest floor is what is written here, not the
one a larger project can promise.

## What is in scope

The [threat model](docs/threat-model.md) is the reference for what `apexe`
claims to enforce. Anything that defeats a claim in its **§4** is in scope:

- Reaching `execve` with argv the module contract did not authorise — a value
  read as an option despite `validate_argument_value`, an escape from the
  `-`-prefix rejection or its `numeric` / `--`-separator exemptions
- A shell being spawned anywhere on the execution path
- A call that bypasses schema validation, or a loaded ACL that fails to deny
  what its rules say it denies
- Environment variables outside the allowlist reaching a wrapped subprocess
- Writing to `audit.jsonl` in a way that forges, suppresses, or corrupts the
  framing of a record — including through a value the caller controls
- Argument values appearing in `audit.jsonl`, which is documented to hold none
- The captured-output cap or timeout failing to bound a wrapped subprocess
- A binding file or overlay that achieves execution the scan could not produce
- Credential handling on the HTTP transports (`--auth`), including a
  non-loopback bind starting without a credential and without acknowledgment
- Imported HTTP operations bypassing validation or loaded ACLs, or specification,
  upstream API and incoming client credentials reaching the wrong destination
- Explicit verification cases escaping their required isolation, or stale
  binding/binary/case evidence retaining earned runtime confidence

## What is not in scope

The threat model's **§5** documents these deliberately. They are design
boundaries, not defects — a report about one of them will be closed with a link
back to that section:

- **An agent bypassing `apexe` entirely** by using a raw shell tool its host also
  offers (§5.1). `apexe` governs the tools it exposes, not the host
- **A heuristic misclassifying a command** — e.g. `find` annotated `readonly`
  while `find -delete` is destructive (§5.2). This is an overlay fix; open a
  normal issue or a PR
- **Server policy not enabled by the operator** (§5.3). Server ACLs require
  `--acl`, and MCP interactive approval requires `--enable-approval`. Local
  exec/shims load the default ACL when present and install an approval gate;
  an absent ACL still means no access control
- **The audit trail not naming which file was deleted** (§5.5). No argument
  values are recorded, deliberately
- **An incomplete schema from a CLI with poor `--help` output** (§5.6)
- **Anything `apexe` never mediated**: the agent's own file edits, its direct
  HTTP calls, code it runs in a sandbox (§5.7)
- **Prompt injection carried in a wrapped tool's output.** `apexe` bounds output
  size, not content
- **Normal execution exceeding a host sandbox boundary.** apexe does not
  implement that sandbox (§1). Required isolation for explicit verification
  cases has its own enforced boundary and remains in scope above

If you are unsure which side a finding falls on, report it privately. A
misrouted report is a smaller problem than an unreported bypass.
