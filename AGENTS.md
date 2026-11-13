# AGENTS.md — apexe Development & Code Quality Specification

## Project Overview

`apexe` is a governed execution layer for CLI tools and HTTP APIs. It scans CLI commands into typed apcore modules, executes them locally through `exec` and eligible PATH shims, verifies CLI contracts, and serves CLI and imported OpenAPI operations through MCP/A2A with configurable access control, approval and auditing.

---

## Change Discipline

Understand the existing architecture and business design before adding a
feature or changing code. Read the code and the document that owns the area,
and be able to name the mechanism that is supposed to handle the problem. If
one already can, the change belongs to that mechanism's data or configuration —
not to a second path around it. An extension that works by duplicating an
existing layer is a design regression, even when it passes the tests.

### Decide in this order, and stop at the first answer

1. **Is this a bug in a layer that already covers the case?** Fix that layer.
   A wrong annotation, a misleading refusal message or a parser defect is a
   defect in the existing design, not a missing feature.
2. **Does an existing configuration or policy expression solve it?** ACL rules,
   `env --scanned` / `--also` / `--exclude`, the `agent` host integration,
   approval ceilings and curated overlays are the designed extension points.
   Use them before considering code.
3. **Is the answer new data rather than new code?** An overlay, an ACL rule or
   a provenance record is a reviewed assertion an operator can audit and
   change without a rebuild. Prefer it to a switch nobody can explain.
4. **Only then extend the design.** State which existing mechanism cannot
   express the need and why. Keep the extension in the layer that owns the
   concept, and update the document that describes it in the same change.

### May, should, must not

- **May**: fix the layer that owns the decision; add a curated overlay with
  provenance; add a model field the contract genuinely needs (and bump
  `SCAN_FORMAT_VERSION`); add or reorder ACL rules; correct an annotation that
  is demonstrably too broad for the real installation.
- **Should**: prefer the narrowest mechanism that expresses the intent; keep
  the coverage gate and the policy gate answering their own questions; leave
  every refusal saying which layer actually decided; link to the owning
  document instead of restating it.
- **Must not**: widen the surface to make one call succeed (`--unmanaged`,
  a catch-all ACL rule, `--scanned all` as a reflex); write an overlay from
  memory or to make `exec` accept a scanned tool; change an annotation to clear
  a prompt; treat `requires_approval` as a verdict instead of a ceiling; add a
  configuration knob for behaviour an ACL rule, overlay or policy flag already
  expresses; tell a shim-governed host to wrap its commands in `exec`.

### The layer that owns the decision

Two gates are evaluated in order — coverage (is this route expressible?) then
policy (is this caller allowed?) — and three tiers label what the grammar is
(reviewed, scanned, legacy). A change belongs to exactly one layer and must not
move a decision into a layer that does not own it.

| Layer | Owns | Must not |
|---|---|---|
| Scanner / overlay | the declared surface and its grammar | widen the surface to make one call succeed; write an overlay from memory |
| Executor | how an admitted route runs (native argv, scanned argv passthrough, module JSON, MCP/A2A) | admit scanned grammar through the native path; add a third execution surface for a case an existing one covers |
| ACL / approval | who may call what, and where a human must confirm | use a catch-all to make a policy question disappear; change an annotation to clear a prompt |
| Host integration | how a client's commands enter apexe | wrap a call that is already governed, or route a shim-governed host through `exec` |

The operator-facing rules live in [`docs/overlays.md`](docs/overlays.md) and
[the user manual](docs/user-manual.md#coverage-and-policy-are-two-gates). Keep
this section short and link to them rather than restating the procedures.

---

## Rust Code Quality

### Readability

- Use precise, full-word names; standard abbreviations only when idiomatic (`buf`, `cfg`, `ctx`).
- Functions ≤50 lines, single responsibility, verb-named (`parse_request`, `build_schema`).
- Avoid obscure tricks, overly chained iterators, unnecessary macros, or excessive generics.
- Break complex logic into small, well-named helper functions.

### Types (Mandatory)

- Provide explicit types on all public items; do not rely on inference for public API surfaces.
- Prefer `struct` over raw tuples for anything with more than 2 fields.
- Use **`newtype`** wrappers (`struct TaskId(Uuid);`) to encode domain semantics.
- Use **`enum`** for exhaustive variants; avoid stringly-typed logic.
- Implement `serde::Serialize` / `serde::Deserialize` on all public data types.

### Design

- Favor **composition over inheritance**; use `trait` only for true behavioral interfaces.
- Prefer plain functions + data structs; minimize trait object (`dyn Trait`) indirection.
- No circular module dependencies.
- Use **dependency injection** (constructor arguments) for config, logging, DB connections, etc.
- Keep `pub` surface minimal — default to module-private, expose only what consumers need.

### Errors & Resources

- Define domain errors with **`thiserror`**; no bare `Box<dyn Error>` in library code.
- Propagate errors with `?`; no `unwrap()` / `expect()` in library paths (tests excepted).
- If `unwrap()` / `expect()` is truly unreachable, document with a `// SAFETY:` or `// INVARIANT:` comment.
- Validate and sanitize all public inputs at crate boundaries.
- Use RAII / `Drop` for resource cleanup; avoid manual teardown.

### Async

- Runtime: **Tokio** (`features = ["full"]`).
- Never block the async executor — use `tokio::task::spawn_blocking` for CPU-heavy work.
- Use `tokio::time::sleep` / `tokio::time::timeout`, not `std::thread::sleep`.

### Logging

- Use **`tracing`** — no `println!` / `eprintln!` in production code.
- Level guide:
  - `tracing::error!` — unrecoverable failures
  - `tracing::warn!`  — recoverable anomalies
  - `tracing::info!`  — key business events
  - `tracing::debug!` — internal state for debugging
- Always include structured fields: `tracing::info!(task_id = %id, "task started")`.

### Testing

- Run with: `cargo test --all-features`
- **Unit tests**: in the same file under `#[cfg(test)] mod tests { ... }`.
- **Integration tests**: in `tests/` directory.
- Test names: `test_<unit>_<behavior>` (e.g., `test_parse_request_returns_error_on_empty_body`).
- Never change production code without adding or updating corresponding tests.
- Use **`tokio-test`** for async test helpers.

### Serialization

- JSON: `serde_json`. YAML: `serde_yaml`.
- Avoid manual `Serialize` / `Deserialize` impls unless schema shape requires it.

---

## Mandatory Quality Gates (CI — all must pass)

Run `make check` before every commit (mirrors CI exactly):

| Command | Purpose |
|---------|---------|
| `cargo fmt --all -- --check` | Formatting |
| `cargo clippy --all-targets --all-features -- -D warnings` | Lint (warnings = errors) |
| `apdev-rs check-chars src/` | Character validation |
| `cargo build --all-features --examples --locked` | Full build and example binaries |
| `cargo test --all-features --locked` | Full suite, including the compiled-interface CLI runtime gate |
| `cargo test --all-features --examples --locked` | Rust example tests |
| `python3 -m unittest discover -s scripts/tests -v` | Runtime-validator regression tests |
| `make test-examples` | All runnable examples and live documented calls |

```bash
make setup   # One-time: installs apdev-rs + pre-commit hook
make fmt     # Auto-format
make check   # Run all gates (do this before git commit)
```

### Clippy & Formatting Rules

- `#[allow(...)]` to silence Clippy requires an inline comment explaining why.
- `#[rustfmt::skip]` is forbidden without documented justification.
- `Cargo.lock` **must be committed** (reproducible CI builds).

### CI Security

- All `uses:` actions must be pinned to a **full commit SHA** (not just a tag):
  ```yaml
  uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
  ```
- Workflow default permissions: `contents: read` (minimum privilege).

---

## Dependency Management

- Evaluate necessity before adding a new dependency.
- Specify minimum compatible versions; avoid over-pinning patch versions.
- Dev-only crates go in `[dev-dependencies]`, never `[dependencies]`.

---

## Tool Overlays

Overlays in `overlays/` are curated human assertions that can replace the scan
result entirely (`mode: authoritative`) and carry the highest trust label in the
system (`confidence: verified`). They are plain data files, which makes them
deceptively easy to add — and a wrong one is worse than none, because an agent
will act on it.

- **Never mark an overlay `verified` without reading the flag list off a real
  installation.** Writing from model knowledge yields plausible, mostly-correct
  output that silently omits recently added flags. The schema requires a
  `provenance` block; do not satisfy it by inventing one.
- **Never copy a flag description from one variant to another.** Of the 38 flags
  `ls` shares between its BSD and GNU variants, zero have identical
  descriptions, and some differ in behavior rather than wording.
- **When you cannot verify against a real installation**, use `mode: merge` with
  a lower confidence, so a gap degrades to the scanner's answer instead of
  erasing a real flag.

Read [`docs/overlays.md`](docs/overlays.md) before adding or editing an overlay.
It is the single source for the verification procedure — keep summaries
elsewhere short and link to it rather than duplicating the steps.

---

## General Guidelines

- **English only** for all code, comments, doc comments, error messages, and commit messages.
- Fully understand surrounding code before making changes.
- Do not generate unnecessary documentation stubs, example files, or boilerplate unless explicitly requested.
- No secrets hardcoded — use environment variables or configuration files.
