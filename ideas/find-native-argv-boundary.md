# Native argv for commands with a trailing mini-language (`find`, `tar`, `awk`)

**Status:** specification. Not implemented. This is the last blocker for running
`find` through a shim rather than only through module JSON.

## 1. The problem

`find`'s grammar is `find [options] path… [expression]`. The overlay declares it
faithfully:

```json
"positional_args": [
  { "name": "path",       "type": "path",   "variadic": true, "before_flags": true },
  { "name": "expression", "type": "string", "variadic": true }
]
```

and the resolver refuses it before anything else runs:

```rust
// src/adapter/argv.rs:373
fn validate_operands(&self) -> Result<(), ModuleError> {
    if ... || self.operands.iter().filter(|p| p.schema["type"] == "array").count() > 1 {
        return Err(invalid("Ambiguous declared positional grammar"));
    }
```

Measured on this machine after the annotation work:

```
$ find /etc -maxdepth 1 -name hosts
Error: Native arguments refused for cli.find: Ambiguous declared positional grammar
```

So the corpus change made `find` readable by the *policy* (ACL allow, read
strength for searches, write strength plus approval for `-delete`), and that is
verified through `apexe exec cli.find --input …` and through MCP/A2A. The shim
still cannot drive it.

## 2. Why the obvious fix is unsafe

`x-apexe-opaque-after` already exists and marks the last operand as an
unvalidated tail. Using it here would pass `find`'s expression through untouched,
and that is a **silent hole**, not a fix: the approval gate reads
`inputs["delete"]` and the path guard reads `x-apexe-writes-with`, so a `-delete`
inside an unvalidated tail is invisible to both. `find . -delete /etc` would be
checked at read strength — the exact failure this project has spent the week
removing.

## 3. The rule

> When an overlay declares a leading `before_flags` variadic operand **and** a
> trailing variadic operand marked `opaque_after`, the boundary between them is
> the first token that matches a declared flag. Tokens before it are the leading
> operand's values, type-checked as today. Tokens from the boundary onward are
> parsed **normally** — every one must be a declared flag, with the arity it
> declares. Any token that cannot be classified refuses the whole call and names
> the token.

`opaque_after` therefore changes meaning: not "stop validating", but "stop
assigning tokens to this operand; the rest is parsed as flags". The schema
description and `docs/overlays.md` must say so, and the release note must say the
meaning changed, because an existing overlay relying on today's pass-through
behaviour would otherwise silently start validating.

### Effects

| Call | Today | After |
|---|---|---|
| `find . -maxdepth 1 -name '*.rs' -print` | refused | runs; `-name` takes one value, `-print` takes none |
| `find . -delete` | refused | runs; `-delete` is seen, so approval fires and the path check is at write strength |
| `find . -exec rm {} \;` | refused | refused, naming `rm` (or `{}`), with "`-exec`'s argument list is a command line, not a flag; use `apexe exec cli.find --input …`" |
| `find . -name` | refused | refused: `-name` declares one value and none was given |

The third row is the honest ceiling: the arguments of `-exec` are *another
program's argv*, which no type system can validate. Fail-closed is the only safe
answer, and the message must say so rather than reporting a grammar defect.

## 4. Code touch points

| File | Change |
|---|---|
| `src/adapter/argv.rs:373` `validate_operands` | allow a second array operand **iff** the last one is `opaque_after` and the earlier ones are `before_flags`; every other combination keeps refusing |
| `src/adapter/argv.rs:357` `locate_opaque_operand` | keep the "last operand, variadic" requirement; add "the operands before it must be `before_flags`" |
| `src/adapter/argv.rs:390` `parse` | at the boundary, hand the remaining tokens back to the flag matcher instead of emitting `OpaqueTail`; propagate an unknown-token refusal that names the token |
| `src/adapter/schema.rs:563` | description of `x-apexe-opaque-after` states the new meaning |
| `docs/overlays.md` | the rule above, plus the semantic change |
| `CHANGELOG.md` | "meaning changed", not "field added" |

`OpaqueTail` stays for the case it was built for — `node <script> …`, where the
tail really is an interpreter's argument vector and no validation is possible.

## 5. Safety invariants the change must preserve

1. A tool that declares neither field behaves byte-for-byte as today.
2. The invariants of §2 hold *by construction*: no token is ever skipped, so
   `x-apexe-escalating-params` and `x-apexe-writes-with` see every flag that was
   sent. A regression here is a security bug, so it needs a test that asserts the
   gate fires for `find . -delete`, not just that the call succeeds.
3. An unknown token is refused with its name and position, never silently passed.

## 6. Test matrix (each row is a test, run against the real binaries)

| Tool | Declared shape | Cases |
|---|---|---|
| `find` (BSD, macOS) | path… + expression | read + `-delete` (gate fires) + `-exec` (refused) + missing value (refused) + `--` separator |
| `tar` (BSD, macOS) | `-f file` + members… | `tar cf out.tar dir` with `cf` bundled; a member named `-x` after `--` |
| `grep` (BSD, macOS) | pattern + files… | `grep -rn pat dir` (no boundary; regression check that nothing changed) |

Plus the existing runtime CLI scenarios in `scripts/validate_cli.py`, so the
compiled interface keeps being exercised.

## 7. Rollout

1. This spec and the `overlay_after` semantic change reviewed together;
2. Resolver change + unit tests, with `grep` as the "nothing moved" control;
3. `find`/`tar` measured on macOS by the operator, and on Linux by the same
   container route used for the GNU `find` verification
   (`rust:slim` build + `debian:stable-slim` run);
4. Only then does the corpus keep `opaque_after` on `find`'s expression — it is
   already there and currently inert, which is itself worth noting: the marker
   must not be shipped in a release whose parser does not honour it.
