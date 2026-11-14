# Proposal: make `find` readable by default, with flag-scoped escalation

**Target repository:** `cli-permissions` (the corpus), not `apexe`.
**Status:** proposal, not applied.

## 1. Decision requested

Change the curated overlay for `find` from a module-wide refusal to a
flag-scoped one:

| | Today (corpus) | Proposed |
|---|---|---|
| `readonly` | `false` | **`true`** |
| `destructive` | `true` | `false` |
| `open_world` | `true` | `false` |
| `requires_approval` | `true` (blanket) | ceiling, reached only by the primaries below |
| `approval_flags` | — | `[delete, exec, execdir, ok, okdir]` (BSD) |

Effect: `find` searches anywhere a read is allowed; `-delete` and the
`-exec`/`-ok` family ask a human first.

## 2. Why: what the current entry costs

The current entry is not a guess — it is a deliberate stance, because `-exec`
runs arbitrary programs. But it is stated at module granularity, and `find` is
the most common *read* command that happens to have a write mode:

```
find /etc -name '*.conf'    # a pure read; refused today because the module is destructive
find . -maxdepth 1 -type d  # same
```

Every search pays for `-delete` and `-exec`. Operators notice this as "`find`
does not work here", not as "`find` is gated", and the workaround they reach for
is to allow `cli.find` in their own ACL — which grants the write forms too,
without an approval prompt. The coarse entry therefore pushes users toward a
*less* safe configuration than the flag-scoped one.

## 3. Evidence

`find@bsd.json` in the corpus, as installed here:

```json
{
  "mode": "authoritative",
  "annotations": {
    "readonly": false,
    "destructive": true,
    "idempotent": false,
    "requires_approval": true,
    "open_world": true
  },
  "flags": [ /* 97 declared, including -delete, -exec, -execdir, -ok, -okdir */ ]
}
```

The five primaries this proposal names are the ones that write or execute, read
off this machine's BSD `find` man page (`man find`, PRIMARIES section,
2026-11-14):

```
-delete              Delete found files and/or directories
-exec utility … ;    Run utility for each match
-exec utility … {} + Same, batched
-execdir utility …   Run from the directory holding the match
-ok / -okdir …       Same as -exec / -execdir, asking for confirmation first
```

`find --help` is not evidence on BSD: it is an illegal option and prints a
four-line usage string with no primaries. The man page is the source, and
`provenance.source` should say `man-page`.

**The GNU variant needs its own entry and its own check.** GNU `find` also ships
the `-fprint` / `-fprint0` / `-fprintf` / `-fls` family, which this BSD build
does not; copying the BSD list to GNU would leave those primaries in the
"writes" category with no trigger, i.e. writing calls checked at read strength.

## 4. Why apexe can express this now

`AnnotationOverrides` gained `approval_flags` and `writes_with`
(apexe `9666904`, `07a11f0`):

- `approval_flags` marks the module as an approval *ceiling* and records the
  property names in `x-apexe-escalating-params`, so the gate stands down for a
  call that sends none of them;
- `writes_with` is what the path guard reads to choose the access mode per call,
  and defaults to the approval list — one list to maintain, not two;
- a flag list naming a property the module does not declare is dropped, with an
  error naming the real properties. Dropping keeps write strength, which is the
  conservative direction; applying a trigger that can never fire would not be.

Property names follow the scanner's rule, so they are derivable rather than
guessed: a short-only flag keeps its letters (`-delete` → `delete`).

## 5. Proposed change

```json
"annotations": {
  "readonly": true,
  "destructive": false,
  "idempotent": false,
  "requires_approval": true,
  "open_world": false,
  "approval_flags": ["delete", "exec", "execdir", "ok", "okdir"]
}
```

`writes_with` is omitted on purpose: it inherits `approval_flags`, because the
two answer one question — which arguments are the dangerous ones.

Also update `provenance` in the same edit: `checked_on` to the date the man page
was read, `source: man-page`, `command: man find`, and a `notes` line saying the
annotation change came from that reading. Under `docs/overlays.md`, a claim
nobody can re-run is indistinguishable from a guess.

## 6. Expected behaviour after the change

| Call | ACL | Approval | Path strength | Result |
|---|---|---|---|---|
| `find /etc -name '*.conf'` | readonly allow | none | read | runs |
| `find . -type f` | readonly allow | none | read | runs |
| `find /tmp -delete` | readonly allow | **prompt** | write | runs after approval |
| `find /etc -delete` | readonly allow | prompt | write | refused (`/etc` is `read_only`) |
| `find . -exec rm {} \;` | readonly allow | **prompt** | write | runs after approval — see §7 |

## 6.1 Verified against the real binary

Run in an isolated `HOME` with the corpus entry's 97 declared flags kept and only
the `annotations` block changed, then `scan find --write-acl`:

```
acl explain cli.find            -> allow / rule 0 "Auto-allow readonly CLI commands"
list --verbose (cli.find)       -> readonly: true, destructive: false,
                                   requires_approval: true, open_world: false
                                   extra: x-apexe-writes-with ["delete","exec","execdir","ok","okdir"]
                                          x-apexe-approval-basis "flags"
                                          x-apexe-escalating-params [same five]
```

With `writes_with` set to `["delete"]` alone, to isolate the path guard from the
approval gate:

```
exec cli.find --input '{"f":["/etc/hosts"]}'          -> valid: true
                                                         path_guard allowed: true   (read)
exec cli.find --input '{"delete":true,"f":["/etc/hosts"]}'
                                                      -> valid: false
                                                         path_guard allowed: false  (write)
                                                         checks: argv -> ACL_DENIED,
                                                         '/private/etc' is protected
```

That second line is the whole point of the proposal: the write form is refused by
the path policy *after* the module was allowed, so a flag-scoped grant does not
open protected paths.

This run also found an apexe defect that unit tests had encoded as correct —
`access_mode` consulted `readonly` before `x-apexe-writes-with`, so the write
form was still checked at read strength. Fixed in `11e5148`; the entry below is
only safe to ship on top of it.

## 7. Residual risk (do not ship the proposal without this paragraph)

`-exec` runs the named program itself. The program never passes through apexe,
so neither the ACL nor the path policy applies to it: only the approval prompt
stands between a caller and `find . -exec rm -rf /important \;`. That is a real
ceiling, not enforcement, and it is the reason `open_world` was set in the first
place.

Two honest options, and this proposal recommends the second:

1. Leave `-exec` out of `approval_flags` and keep it a module-wide refusal —
   but then the entry has to stay unreadable, which is what we are fixing.
2. Accept approval as the ceiling and record it. Enforcement for `-exec` needs a
   confinement backend (`docs/sandbox.md`); on macOS that backend is `advisory`
   today, measured.

## 8. Verification (run in this order)

```bash
# 1. The corpus entry loads and the surface keeps its 97 flags.
apexe scan find --format json | python3 -m json.tool | grep -c '"short"'

# 2. The annotation reached the module, not just the file.
apexe list --verbose | grep -A6 'cli.find$'      # readonly: true, approval basis flags

# 3. The ACL follows the annotation.
apexe scan find --write-acl                       # refreshes readonly allow + destructive deny
apexe acl explain cli.find                        # allow, by the readonly rule

# 4. Both directions of the escalation.
apexe exec --dry-run -- find /etc -name '*.conf'  # valid, acl allow, no approval
apexe exec --dry-run -- find /tmp -delete         # approval_required: true
apexe exec --dry-run -- find /etc -delete         # approval, and the path check refuses
```

Step 4's last case is the one that matters: approval alone must not make a
protected path writable.

## 9. Rollback

Revert the single `annotations` block and the `provenance.checked_on`. No apexe
release is involved: the corpus is data, which is the reason this change belongs
there.
