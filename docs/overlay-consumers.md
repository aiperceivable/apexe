# Reading Overlays Without apexe

An overlay is a reviewed description of **one variant of one command** — BSD `ls`
as macOS ships it, GNU coreutils `ls`, BusyBox `ls`. The format is defined by
[`schemas/tool-overlay.schema.json`](../schemas/tool-overlay.schema.json) and
describes the command, not apexe: nothing in a `.json` file names a Rust type, a
scan tier, or an apexe concept. This document is what a second implementation
needs, because until now that knowledge lived only in apexe's source.

For **writing** an overlay, see [`overlays.md`](overlays.md). This is the other
half: how to consume one you were handed.

---

## 1. What you are holding

One file, one `(command, variant)` pair. The command name is a name, never a
path — `ls`, not `/bin/ls` — because the same name is several programs and the
variant is what tells them apart.

A consumer's job is three steps: **select** the overlay that applies to the
binary in front of it, **read** the surface it describes, and **respect** the
placement rules when turning a call into `argv`.

## 2. Selecting an overlay

More than one overlay can name the same command. `match` decides which apply and
how strongly.

```json
"match": {
  "platform": ["macos", "freebsd"],
  "probe": { "args": ["--version"], "expect": "failure" },
  "binary_globs": ["/usr/bin/sed", "/bin/sed"],
  "version_range": ">=9.0"
}
```

**Every declared condition must hold.** A condition that is stated and not
satisfied rejects the overlay outright — it never degrades into a weaker match.
This matters most for `probe`: Homebrew puts GNU `sed` on a macOS box, where
every path and platform signal still says BSD, and only running the binary
distinguishes them. An overlay that declares a probe and whose probe fails is
not a candidate, however well its paths match.

**Match strength, strongest first:**

| Strength | Condition |
|---|---|
| Probe | `probe` declared and satisfied |
| Platform + globs | both `platform` and `binary_globs` declared and satisfied |
| Platform | `platform` alone |

Among overlays of equal strength, apexe breaks the tie toward the one that came
from a source closer to the operator, and then toward the one loaded last. A
consumer with its own layering should apply the same principle — the more local
source wins — and should make its order explicit rather than inheriting it from
a data structure.

An absent `match` means "no conditions", which matches anything.

## 3. `mode` — is this the whole surface?

`authoritative` says the flags, positional args and subcommands listed are the
**complete** set for this variant. If you hold a weaker description of the same
command — your own scan, a completion spec, an older overlay — discard it and
use this.

`merge` says only the listed entries are asserted. Everything else stays
whatever you already had, so a gap in the overlay degrades to your weaker source
rather than erasing a real flag.

The distinction is load-bearing in the dangerous direction: an `authoritative`
overlay that is missing a flag **erases** that flag from your model of the
command. Authors are told to claim it only when the option set was closed
against the running tool — a `getopt(3)` string, an enumerated long-option
table — never from a transcribed document.

## 4. `confidence` and `provenance` — how much to trust it

Four levels, describing the **evidence** rather than the author's certainty:

| Level | Means |
|---|---|
| `verified` | Read off the running tool, with `provenance` recording how. The schema requires the block at this level. |
| `high` | From a machine-readable description shipped by the tool or its packagers. |
| `medium` | Two independent sources agree. |
| `low` | One unconfirmed source. |

`provenance` is the part worth reading:

```json
"provenance": {
  "platform": "linux", "tool_version": "4.9", "package": "sed",
  "source": "help", "checked_on": "2026-09-05",
  "command": "docker run --rm debian@sha256:0463… sed --help",
  "environment": "debian@sha256:0463…",
  "notes": "…"
}
```

`command` is meant to be **re-runnable verbatim**, and `environment` pins the
build by digest. `checked_on` is how you decide whether a past check still
holds; there is no expiry in the format, so staleness is your policy to set.
`source` has exactly three values — `man-page`, `help`, `vendor-docs` — and
there is deliberately none meaning "I know this tool".

## 5. Building an `argv`

Four fields exist because a usage line cannot express them, and ignoring any of
them produces a command that runs and does the wrong thing.

**`value_optional`** — the flag has two legal spellings that mean different
things. `sed -i` edits in place with no backup; `sed -i.bak` keeps one. The
value must be **attached**: `--flag=VALUE`, not `--flag VALUE`. Rendering the
separated form for an optional-value flag makes the tool read the value as an
operand — `grep --color never pattern file` looks for `never` in a file called
`pattern`.

**`conflicts_with`** — flags that must not be sent together, covering both the
pair a tool diagnoses and the pair it silently resolves last-one-wins. If your
input is an unordered structure (a JSON object, a map), a last-one-wins pair has
no defined outcome: *which* flag wins depends on the order keys happened to be
written. Send one or the other. No help or man page format states this, so an
overlay is the only place it can come from.

**`before_operands` (flag) / `before_flags` (operand)** — placement. Most tools
are `tool [options] operands`, but `find` is not: its true options must precede
the paths (`find dir -L` is "unknown primary or operator") while its primaries
must follow them (`find -name '*.txt' dir` is "illegal option -- n"). Two
classes of dash-token on opposite sides of the same operand, which is why
placement is a flag-level and operand-level fact rather than one boolean.

**`end_of_options`** — the command's own parser honours `--`. Set it and a value
that legitimately begins with `-` can be passed behind the separator; leave it
unset and such a value should be refused, which is the right answer for a
command nobody has checked. Note `--` protects the operand slot from being read
as options; it does not rescue an operand whose value begins with `-` if the
command re-parses operands in a second pass, as `find` does.

**`repeatable`** means the flag may appear more than once, so the natural input
shape is a list.

## 6. Annotations

Five behavioural assertions, each optional.

**An absent field means "unknown", and a consumer must not read it as `false`.**
The schema requires none of them, so `"annotations": {"readonly": true}` says
nothing whatever about `open_world` — and in most languages the natural
expression of that (`if annotations.get("open_world")`) silently reads the
absence as a denial. That is the unsafe direction, and it is not hypothetical:
the reference consumer shipped with exactly that bug, and `sort` is the command
that shows why it matters. `sort` reads as a pure text utility, and
`--compress-program=PROG` runs PROG. Until it was checked, the corpus left
`open_world` unstated on `sort`, so a consumer that equated absent with false
would have told a model that `sort`'s domain of interaction is closed.

Where a consumer has its own inference, absent means "keep what you inferred".
Where it has none — a consumer reading only the corpus — absent means it does
not know, and the safe reading of a consequential property it does not know is
the conservative one.

| Field | Means |
|---|---|
| `readonly` | Does not modify state on the machine it runs on. Says nothing about what it *sends*. |
| `destructive` | Can destroy data. Not the same as "writes" — creating a file is neither. |
| `idempotent` | Running it twice with the same arguments leaves the same state as running it once. |
| `requires_approval` | A human should decide before this runs. Usually a *ceiling*: it marks the command as gateable, not every call as dangerous. |
| `open_world` | Reaches beyond this machine, or hands its arguments to another program to run. |

Two traps worth stating, both drawn from the shipped corpus:

- **`readonly` is not "safe".** `readonly` plus `open_world` is the exfiltration
  shape — a read that reaches the network. Do not auto-allow on `readonly`
  alone.
- **A command can destroy without a destructive-looking flag.** `sed`'s `w`
  function truncates its target before any input is read, with no `-i` anywhere
  on the command line; `uniq input output` truncates `output`; `find -fprint
  FILE` truncates FILE. These are annotated, but only because someone checked.

## 7. What the format does not give you

- **No enforcement.** These are assertions about a command, not a policy and not
  a sandbox. Whether a call is refused is entirely the consumer's decision.
- **No completeness guarantee below `authoritative`**, and at `authoritative`
  only as good as the author's closure check.
- **No freshness guarantee.** `checked_on` is a date, not a promise; a tool that
  released since then may have changed.
- **No subcommand tree** in the current version: an overlay describes one
  command's own surface. A tool with subcommands needs one overlay per
  invocable command.

## 8. What the format does not settle

Found by writing a second consumer against this document alone — a Python script
that shares no code with apexe. It worked, including probe-based variant
selection, but these had to be decided by the consumer rather than read off the
data. They are listed so the next implementer does not think they missed
something.

**There is no derivation rule from annotations to a permission tier**, and
there deliberately will not be: what to allow depends on the environment, and a
corpus that shipped a policy would be asserting something it cannot check. The
cost is real, though — two consumers reading the same corpus can reach different
policies, so a rule is only as portable as the mapping behind it.

What a mapping must not do is lose the facts that are *not* in the five
annotations. Two are load-bearing and easy to skip:

- a flag marked `long_running` (§5) hangs an agent forever on a command whose
  annotations say `readonly` and `idempotent` — `tail` is exactly that shape;
- an operand or FILE-valued flag that truncates (§6) destroys without any
  destructive-looking flag on the command line.

A consumer is free to decide that a destructive command is allowed in its
environment. It is not free to conclude a command is harmless without having
looked at those two.

**There is no annotation for "creates".** `mkdir` and `touch` are neither
`readonly` nor `destructive`: they make something that was not there and destroy
nothing. Both fields are simply `false`, which reads identically to "nobody
said". A consumer has to invent a third meaning for the pair.

**The tie-break assumes a layered consumer.** §2 says the more local source
wins, which presumes sources to rank. A consumer reading one directory has no
such signal and must fall back to something arbitrary.

**`version_range` has a syntax but no acquisition rule.** The schema defines the
constraint; nothing says how a consumer learns the version to test it against,
or what to do when it cannot — treat the condition as satisfied, or as failed.
Those are opposite answers in the dangerous direction.

**`probe` has no timeout and no side-effect contract.** Running it means
executing an arbitrary binary with arbitrary arguments, which is a real action
taken during what a consumer may think of as reading a file. The guide's
`--version` examples are harmless; nothing in the format says they have to be.

**`mode` is not needed to derive a permission rule.** The reference consumer
never reads it. It matters when you hold your own description of a command and
must decide whether to discard it; a consumer starting from the corpus alone
does not.

## 9. Minimum viable consumer

Read the directory, keep the files whose `command` matches, drop the ones whose
`match` conditions fail, take the strongest remaining, and read `annotations`.
That is enough to derive a permission rule. Everything in §5 becomes necessary
only when you also build the command line.
