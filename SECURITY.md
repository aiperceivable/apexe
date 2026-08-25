# Security Policy

`apexe` spawns processes on behalf of AI agents. Its whole purpose is to sit on
that boundary, so a flaw here is worth reporting carefully.

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
- The output cap or timeout failing to bound a runaway subprocess
- A binding file or overlay that achieves execution the scan could not produce
- Credential handling on the HTTP transports (`--auth`), including a
  non-loopback bind starting without a credential and without acknowledgment

## What is not in scope

The threat model's **§5** documents these deliberately. They are design
boundaries, not defects — a report about one of them will be closed with a link
back to that section:

- **An agent bypassing `apexe` entirely** by using a raw shell tool its host also
  offers (§5.1). `apexe` governs the tools it exposes, not the host
- **A heuristic misclassifying a command** — e.g. `find` annotated `readonly`
  while `find -delete` is destructive (§5.2). This is an overlay fix; open a
  normal issue or a PR
- **No policy being enforced without `--acl` / `--enable-approval`** (§5.3).
  Both are opt-in by design
- **The audit trail not naming which file was deleted** (§5.5). No argument
  values are recorded, deliberately
- **An incomplete schema from a CLI with poor `--help` output** (§5.6)
- **Anything `apexe` never mediated**: the agent's own file edits, its direct
  HTTP calls, code it runs in a sandbox (§5.7)
- **Prompt injection carried in a wrapped tool's output.** `apexe` bounds output
  size, not content
- **Sandbox escape.** `apexe` is not a sandbox (§1) and does not claim
  containment

If you are unsure which side a finding falls on, report it privately. A
misrouted report is a smaller problem than an unreported bypass.
