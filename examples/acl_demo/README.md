# apexe ACL demo

Shows how apcore **Access Control Lists (ACL)** gate calls to apexe's
`CliModule`-wrapped commands — the same `orders.delete` (admins only) /
`orders.list` (public read) contract used across the apcore framework
integrations (see [`axum-apcore/examples/acl_demo`](../../../axum-apcore/examples/acl_demo)).

## What it shows

| Call | Roles | Result |
| --- | --- | --- |
| `orders.delete` | *(none — anonymous)* | **denied** |
| `orders.delete` | `user` | **denied** |
| `orders.delete` | `admin` | **allowed** |
| `orders.list` | *(any)* | **allowed** (read is public) |

## How it works

1. `build_demo_executor()` registers two apexe `CliModule`s with explicit IDs
   — `orders.delete` (`destructive: true, requires_approval: true`) and
   `orders.list` (`readonly: true`) — via the exact pipeline `apexe scan`
   produces: build a `ScannedModule`, write it as a `.binding.yaml` with
   [`YamlOutput`](../../src/output/yaml.rs), then load it back through
   [`apexe::module::build_executor`](../../src/module/registry.rs) with
   [`acl.yaml`](./acl.yaml) attached — the same function `apexe serve --acl`
   and `apexe a2a --acl` call under the hood.
2. Each call is made with an explicit `apcore::Context` built from an
   `Identity` carrying zero, one, or the `admin` role (`ctx_with_roles`),
   directly against `Executor::call(...)`.
3. A denied call surfaces as an `ACLDenied` `ModuleError`.

`acl.yaml` (first-match-wins, `default_effect: deny`):

- **admins** (`roles: [admin]`) may call any module;
- **anyone** (including anonymous) may call `orders.list`;
- everything else falls through to `deny`.

`orders.delete` is also marked `requires_approval: true`. ACL and approval
are independent gates: this demo builds the executor with
`enable_approval: false`, so an admin's call clears the ACL check and then
skips the (unconfigured) approval gate — apcore logs a warning about that at
`RUST_LOG=warn`. Pass `enable_approval: true` in `ExecutorOptions` (what
`apexe serve --enable-approval` / `apexe a2a --enable-approval` do) to
additionally require human approval even for admins.

## Run it

```bash
cargo run --example acl_demo
```

```
=== orders.delete (admins only) ===
  admin     -> ALLOWED
  user      -> DENIED (ACLDenied)
  anonymous -> DENIED (ACLDenied)

=== orders.list (public read) ===
  admin     -> ALLOWED
  user      -> ALLOWED
  anonymous -> ALLOWED
```

Or run the equivalent assertion as a test:

```bash
cargo test --example acl_demo
```

## This is not the only boundary

The ACL decides *who may call which module*. It cannot decide *what one call
may touch* — apcore rules match on `callers` and `targets` (module ids), and
every registered condition key is a fact about the principal, never about an
argument. So an ACL can say "`cli.rm` is allowed" or "denied", and nothing in
between.

The **path guard** covers that second axis, and unlike this demo's rules it is
on by default on every surface with no flag to enable it. See
[`examples/path_guard`](../path_guard/) for the same treatment of that layer.

## Why this calls `Executor::call` directly instead of curling a server

apcore's ACL engine is transport-agnostic — it evaluates whatever
`Context::identity.roles` the caller passes in, regardless of whether that
call came from a library, an MCP tool invocation, or an A2A task. This demo
exercises that engine directly (like `axum-apcore/examples/acl_demo`'s own
test does), which is also exactly what embedding apexe as a library looks
like (see [`examples/programmatic.rs`](../programmatic.rs)).

**Caveat:** unlike `axum-apcore`'s demo — which injects `Identity.roles` from
an `X-Roles` header via Axum middleware — apexe's `apexe serve` / `apexe a2a`
transports do not yet populate per-caller `Identity` from an HTTP header or
JWT claim. Every module call served over MCP/A2A today runs as a single
implicit anonymous caller (no roles). That means a role-gated rule like the
`admin`-only one above would currently always deny when served over the
wire — `--acl` today is most useful for apexe's own readonly-allow /
destructive-deny default policy (see
[`AclManager::generate_default`](../../src/governance/acl.rs)) and the
`--enable-approval` human-in-the-loop gate, not per-caller RBAC. Wiring a
JWT/session identity into `McpServerBuilder`/`A2aServerBuilder` (mirroring
`apcore-mcp`'s and `apcore-a2a`'s own auth support) would be a natural
extension to make this demo's `admin`/`user` distinction reachable over
`apexe serve --acl acl.yaml` too.
