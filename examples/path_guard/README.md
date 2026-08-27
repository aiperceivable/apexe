# apexe path guard demo

Shows what a wrapped tool may be **pointed at** — the boundary that sits below
the ACL and applies to every call on every surface.

This is deliberately separate from [`acl_demo`](../acl_demo/), because the two
answer different questions:

| | [`acl_demo`](../acl_demo/) | this demo |
| --- | --- | --- |
| Decides | *who* may call *which module* | what *one call* may touch |
| Reads | the caller's `Identity.roles` | the argument values |
| Configured with | `--acl acl.yaml` | nothing — it is on by default |
| Reachable over `apexe serve` | not yet (see that README's caveat) | yes, always |

That last row is the reason they are not one page. `acl_demo`'s `admin`/`user`
distinction is currently only reachable from a library call, because the served
transports do not populate a per-caller `Identity`. The path guard has no such
gap: it needs no flag, has no off switch, and behaves the same over MCP, A2A
and direct `Executor::call`.

## What it shows

Two modules with an identical schema — one variadic operand typed as a path —
differing only in their `readonly` annotation. That annotation selects which of
the guard's two lists binds the call.

| Call | Path | Result |
| --- | --- | --- |
| `demo.read` (readonly) | `/etc/hosts` | **allowed** — system paths stay legible |
| `demo.write` (writer) | `/etc/hosts` | **denied** |
| `demo.read` | `~/.ssh/id_rsa` | **denied** — credentials bind readers too |
| `demo.write` | `/` | **denied** — it *contains* protected locations |
| `demo.read` | `/` | **allowed** — `ls /` is not `rm -rf /` |
| `demo.write` | `…/../../../../../../etc/passwd` | **denied** — resolved before comparison |
| `demo.write` | `/etcetera/notes` | **allowed** — `/etcetera` is not `/etc` |
| `demo.write` | `<workspace>/build/out.txt` | **allowed** |

Then the same four `/etc/nginx/**` paths with and without an `allowed_paths`
carve-out configured, which is the one setting that relaxes the guard.

## Why the split is read/write rather than by directory

A single list forced one answer onto two unrelated risks. Refusing
`cat /etc/hosts` bought nothing — the file is world-readable, and an agent's own
file-reading tool reaches it regardless — while refusing `cat ~/.ssh/id_rsa` is
the whole point. So:

- **System paths** bind writers only. Destruction is the risk; legibility is not.
- **Credential paths** bind both. A deleted private key announces itself the
  next time the key is used; one copied into a model context leaves no trace, so
  the stricter treatment goes to the risk that is harder to notice.

Ancestry follows the same logic: a writer may not target a directory that
*contains* a protected one (`rm -rf /` never names `/etc` and destroys it
anyway), while a reader may, because holding readers to that rule would refuse
`ls ~` — most of what a reader legitimately does.

## Run it

```bash
cargo run --example path_guard
```

Or run the same contract as an assertion:

```bash
cargo test --example path_guard
```

## What this demo does *not* show

The guard acts on values the input schema **types** as paths. Three limits
follow, all documented in [`docs/threat-model.md`](../../docs/threat-model.md)
§5.8 and §5.9 rather than implied here:

- A value the schema does not mark as a path is not inspected — including a
  command string handed to `env -S` or `xargs`, which is why those tools are
  classified `destructive` and are better left out of the registry entirely.
- The mode is per module, not per argument: `cp /etc/hosts ~/backup` is refused
  even though the system path is only being read.
- Nothing bounds what the tool does after it starts. `find . -delete` is past
  the boundary.

The guard raises the floor for the case that actually recurs — an agent pointing
a legitimate destructive command at a system directory. It is not a sandbox.
