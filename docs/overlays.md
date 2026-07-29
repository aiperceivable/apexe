# Authoring Tool Overlays

An overlay is a hand-written, reviewed description of **one variant of one command** — BSD `ls` as shipped by macOS, GNU coreutils `ls`, BusyBox `ls`. When one matches, it can replace the scanner's heuristic output entirely.

That power is the reason this document exists. A wrong overlay is worse than no overlay: `mode: authoritative` discards the scan result, so a flag you got wrong becomes authoritative fact that an agent will act on, carrying the highest confidence label in the system.

There is deliberately **no `apexe overlay verify` command**. See [What cannot be automated](#what-cannot-be-automated) for why. The verification described here is a procedure a human follows, and its reliability comes from the human.

---

## When to write one

Write an overlay when heuristic scanning structurally cannot produce the truth:

- **The tool's `--help` is a bundled usage line.** Most BSD/macOS built-ins reject `--help` and print `usage: ls [-@ABCFGHILOPRSTUWXabcdefghiklmnopqrstuvwxy1%,] ...`. There is no description for any flag, anywhere in that output.
- **Mutual exclusion matters.** No help or man page format states machine-readably that `ls -l` and `ls -1` conflict. An overlay is the only place `conflicts_with` can come from.
- **The command is standardized and slow-moving.** POSIX core utilities are worth curating because the answer stays true for years.

Do **not** write an overlay for a tool with rich, parseable `--help` (most Go/Rust/Python CLIs). The scanner already handles those, and an overlay would just be a second copy that drifts.

## When not to claim `verified`

`confidence: verified` requires a `provenance` block — the schema enforces this, so an unverifiable claim fails validation rather than shipping. Beyond the schema check:

- **You did not run the real tool.** Writing from knowledge of a tool produces plausible, mostly-correct output that systematically misses recent additions. This is not hypothetical; see the `--zero` case below.
- **You checked a different build than the one you are describing.** A check against coreutils 8.x cannot speak for 9.x.
- **You only read vendor documentation.** That is `source: vendor-docs`, and the schema then requires a `reference` citation, because there is no command anyone can re-run to reproduce your check.

When you cannot verify, use `mode: merge` and a lower confidence. A merge overlay refines the scan instead of replacing it, so a gap in your knowledge degrades to the scanner's answer rather than erasing it.

---

## The verification procedure

The goal is a flag list read off **the running tool itself**, not off memory.

### BSD / macOS variants

The host is the reference installation. `man -P cat` emits nroff overstrike (`D\bD`), so strip it:

```bash
man -P cat ls | col -b
```

BSD man pages have no `OPTIONS` section — options are listed inside `DESCRIPTION` after a line like `The following options are available:`. Read that whole block.

BSD tools are generally not versioned independently of the OS, so record the OS release as `tool_version`:

```bash
sw_vers -productVersion
```

### GNU variants

The reference must be a real GNU installation. Use a container:

```bash
docker run --rm debian:stable-slim ls --version    # exact version for provenance
docker run --rm debian:stable-slim ls --help       # the flag list
```

Debian slim images ship **no man pages**, so use `--help`. This is fine for GNU: its `--help` is complete by convention, unlike BSD's.

Record the image digest so the reference is reproducible:

```bash
docker image inspect debian:stable-slim --format '{{index .RepoDigests 0}}'
```

### BusyBox variants

```bash
docker run --rm alpine ls --help
```

BusyBox implements a deliberate subset. Expect far fewer flags than either BSD or GNU, and do not assume a flag behaves the same way just because the letter exists.

### Cross-checking a draft

To compare a draft against the reference, diff the two flag lists directly. Extract from the reference by taking the **option column** and pulling every dash token out of it — not by matching one short plus one long option, which truncates multi-alias forms (see the trap below):

```bash
docker run --rm debian:stable-slim ls --help \
  | grep -E '^\s{2,8}-' \
  | cut -c1-32 \
  | grep -oE '\--?[A-Za-z0-9][A-Za-z0-9-]*' | sort -u
```

and from the overlay:

```bash
python3 -c "import json;print('\n'.join(sorted(f['long'] for f in json.load(open('overlays/ls@gnu.json'))['flags'] if f.get('long'))))"
```

Then `diff` them. Two directions, two different meanings:

- **In the overlay, not in the reference** — a flag that does not exist. This is the dangerous direction: an agent may try to use it.
- **In the reference, not in the overlay** — a gap. Under `authoritative` this erases a real flag.

**Verify the extraction itself before trusting a difference.** Text-comparison checkers have now produced false accusations three times, and every time the overlay was right:

- A regex that did not handle `--color[=WHEN]` reported three real flags as nonexistent.
- A "one short option, optionally one long option" regex silently truncated GNU's multi-alias form `-R, -r, --recursive` at the second short option, so `--recursive` and `-r` looked like inventions.
- A checker that only matched `-x` short options flagged BSD `head`/`tail`'s long options as invented. They are real: **BSD's lack of long options is not a rule**. `head` and `tail` accept `--lines`, `--bytes`, `--quiet` and `--verbose`; `ln`, `mkdir` and `touch` accept none.

A fourth near-miss came from the shell, not a regex: in zsh an unquoted `$var` is **not** word-split, so `tail $form file` with `form="--lines 2"` passes one argument containing a space and the tool rejects it. That looks exactly like an unsupported flag.

### Ask the binary, not the document

The reliable check is not "parse the docs and diff" — it is interrogating the tool itself:

```bash
strings /usr/bin/touch | grep -E '^[A-Za-z:]{6,}$'   # the getopt(3) option string
/usr/bin/touch -f "$tmp/x" && echo accepted           # does it actually take the flag?
```

The getopt string closes the option set in a way no man page can, and direct invocation settles acceptance. This is how BSD `touch -f` was found: it works, and appears **nowhere** in the man page — not in the option block, not in `COMPATIBILITY`, not in `STANDARDS`. Under `authoritative`, a doc-only check would have erased it.

When a diff surprises you, check the extractor, then check the shell quoting, then ask the binary — and only then change the overlay.

---

## Filling in provenance

```json
"provenance": {
  "platform": "linux",
  "tool_version": "9.7",
  "package": "coreutils",
  "source": "help",
  "checked_on": "2026-07-27",
  "command": "docker run --rm debian:stable-slim ls --help",
  "environment": "debian@sha256:328d16499860ae6cb9b345e2e4cebca08c2a36e4f7278482c7bd1f39d71e5bfd",
  "notes": "Debian slim images ship no man pages, so the flag list was read from GNU --help."
}
```

`command` should be re-runnable verbatim by someone else. `environment` should identify the exact reference build — an image digest, not a tag, since tags move. When `command` names the image itself, it takes the digest too: a `command` reading `debian:stable-slim` beside an `environment` pinned by digest contradicts itself, and re-running it a year later reads a different build than the one that was checked. `package` is optional but expected on any `gnu` overlay, because the `gnu` variant spans several projects and `9.7` alone does not say which one it versions.

`source` has exactly three values: `man-page`, `help`, `vendor-docs`. There is intentionally no variant meaning "I know this tool", because that is not evidence.

---

## What cannot be automated

A tool can compare flag *names*. It cannot check any of the things that make an overlay worth writing:

| Machine-checkable | Human-only |
|---|---|
| Is a flag missing? | Is the description accurate? |
| Is a flag invented? | Is `conflicts_with` correct? |
| Does it satisfy the schema? | Is the type / enum right? |
| Is `provenance` present? | Is `version_range` appropriate? |

`conflicts_with` is the sharpest case: it is the single biggest reason overlays exist, and no man page states it machine-readably. It can only come from reading prose and understanding the tool.

This is why there is no `verify` subcommand. A green check that only covers the left column would be read as "this overlay is correct", and the right column is where the damage lives.

### What `conflicts_with` means

**Two flags that must not be sent together** — whether the binary rejects the
pair outright or silently resolves it last-one-wins.

Both belong there, and the reason is specific to how apexe is called. Its input
is a JSON object, and an object has no ordering. For a last-one-wins group,
*which* flag wins is decided by the order the caller happened to write the keys
in — so `{"f": true, "i": true}` on `rm` is not "`-i` wins", it is
**unpredictable**. A caller has exactly one correct move in both cases: send one
or the other.

The tempting narrower reading — record only what the binary diagnoses — is worse
here than it looks. It marks an override pair as combinable, which tells an agent
it may send both, and the outcome then depends on a JSON key order nobody
intended as an interface.

So: read the prose for the group, run the pair to learn *which* kind it is, and
record it either way. Say which kind it is in `provenance.notes` — "diagnosed as
`conflicting output style options`" and "accepted, last-one-wins" are both worth
writing down, and the second is what stops a later reader from deleting the entry
as a mistake.

### What `source` does and does not cover

`source` names the document the flag list and its descriptions were **read
from**. It is not a record of how the overlay was *checked*.

Running the binary is often the better check — it is how `ls@gnu`'s `-f` was
caught describing behaviour coreutils dropped years ago, while `--help` had been
right all along. But a probe establishes that a flag exists, what the binary
prints when it rejects something, and whether a value must be attached; it does
not establish what a flag **means**, which is exactly what a description asserts.
A probe that contradicts the document is a reason to go re-read the document.

That is why there is no `binary-probe` value, alongside there being none meaning
"from knowledge". Put probe findings in `provenance.notes`, with the command that
produced them, so the next reader can re-run it.

---

## Traps we have actually hit

**Same name, different meaning.** Of the 38 flags `ls` shares between its BSD and GNU variants, **zero** have identical descriptions. Some differ in real behavior, not just wording:

```
-h  bsd: With -l, use unit suffixes (Byte, Kilobyte, ... Petabyte).
    gnu: With -l and -s, print sizes like 1K 234M 2G.
```

GNU's `-h` also affects `-s`; BSD's does not. Never copy a description across variants.

**This is also why overlays are one file per variant.** A shared base with per-variant overrides would invite exactly this mistake — the author assumes differences are confined to what they explicitly override, and a difference like `-h` above is only visible if you read both man pages line by line.

**Knowledge-written overlays miss new flags.** An earlier revision of `ls@gnu.json` was written without a GNU reference. Checked against coreutils 9.7 it scored 100% precision but missed `--zero`, added in 8.25. Everything it did contain was right, which is precisely what makes this failure mode hard to notice.

**BSD does not always reject `--version`.** macOS `grep` accepts it and prints `grep (BSD grep, GNU compatible) 2.6.0-FreeBSD` with exit 0. A "BSD rejects `--version`" rule classifies it as unknown. Variant detection therefore also matches positive banners, checked *after* the GNU test so `GNU compatible` in that string does not win.

**A declared probe that fails rejects the overlay outright.** It does not fall back to a path match. Falling back would reintroduce the bug probes exist to prevent: Homebrew coreutils puts a GNU `ls` on a macOS box, where every path and platform signal still says "BSD".

---

## Checklist

1. Confirm heuristic scanning genuinely cannot do the job.
2. Obtain the reference installation — the host for BSD, a container for GNU/BusyBox.
3. Read the real flag list from `man`/`--help`. Not from memory.
4. Transcribe flags, then add `conflicts_with` by reading the prose.
5. Diff your list against the reference in both directions; sanity-check your extractor before believing a difference.
6. Record `provenance` with a re-runnable `command` and a digest-pinned `environment`.
7. Choose `authoritative` only if the overlay enumerates the option set completely; otherwise `merge`.
8. Run `cargo test` — the built-in overlays are parsed and validated by the suite.

---

## How a variant is decided

An overlay only applies if its `variant` equals the one the scanner detected, so
the classification rule decides which overlays are *reachable at all*. It lives
in `src/scanner/variant.rs` and runs six tests, **in this order**:

| # | Test | Verdict |
|---|---|---|
| 1 | `busybox` anywhere in the probe output | `busybox` |
| 2 | a BSD marker in a successful banner | `bsd` |
| 3 | a `GNU <package>` pair in a successful banner | `gnu` |
| 4 | an Apple marker in a successful banner, *after* target triples are stripped | `apple` |
| 5 | a rejected `--version` on a BSD-family platform | `bsd` |
| 6 | otherwise | `unknown` |

**Do not reorder rules 2 and 3.** macOS `grep` answers the probe with
`grep (BSD grep, GNU compatible) 2.6.0-FreeBSD`, which names both families. It
is a BSD tool advertising GNU compatibility, not a GNU tool, so BSD is the
truthful reading — and testing GNU first hands `grep` the GNU overlay. This used
to work by accident, because rule 3 matched only the literal string
`GNU coreutils`; now that it matches the whole GNU family, the ordering is the
only thing keeping it correct.

**Rule 4 strips `<arch>-apple-<os>` triples before looking for Apple.** The
token appears in the banner of tools that are not Apple ports at all —
`curl 8.7.1 (x86_64-apple-darwin25.0)` and
`GNU bash, version 3.2.57(1)-release (arm64-apple-darwin25)` — and position is
no help, since "Apple" is the third token of `sort`'s banner and the sixth of
`curl`'s. Both are kept as regression tests.

**Rule 2 matches token prefixes, not whole tokens.** libarchive announces itself
as `bsdtar 3.5.3 - libarchive 3.7.4`, fusing the marker into the program name; a
whole-token test calls that `unknown`. The cost is that a hypothetical token
like `bsdish` would also match, which is judged the better trade: `bsdtar`,
`bsdcpio` and `bsdcat` are real shipping programs.

**Rule 3 matches a pair, not the bare word `GNU`.** Every GNU tool prints
`License GPLv3+: GNU GPL version 3 or later`, and so does any unrelated GPL tool
that quotes the licence — so a bare-word test would promote half of `/usr/bin`.
A short list of boilerplate words (`general`, `public`, `gpl`, `org`, ...) is
excluded from the package position.

### The `gnu` variant is the whole GNU family

`coreutils`, `diffutils`, `tar`, `sed`, `grep` and `bash` are separate projects
with separate release lines, but they are all "the GNU one" as far as an overlay
author is concerned, and a variant per package scales badly. The package is
recorded in two places instead:

- `provenance.package` — `"coreutils"`, `"diffutils"`, ... so `tool_version:
  "9.7"` says what it is a version *of*.
- `match.probe.output_contains` — where it actually has to be enforced. Each of
  the 21 built-in GNU overlays names its own package: 17 keep `"output_contains":
  "GNU coreutils"`, while `diff`, `grep`, `find` and `xargs` declare `GNU
  diffutils`, `GNU grep` and `GNU findutils` respectively. So a GNU `tar` cannot
  pick up a coreutils overlay even though both classify `gnu`.

Overlays before this change were named `<tool>@gnu-coreutils.json` and declared
`variant: gnu-coreutils`. Both are now `gnu`; a user overlay still using the old
token fails to parse.

### `apple` is for Apple's own ports

macOS ships builds whose banner names Apple rather than BSD: `sort` reports
`2.3-Apple (197)` and `git` reports `git version 2.50.1 (Apple Git-155)`. Both
were `unknown` before, so no overlay could ever match them — declaring
`variant: bsd` would have been a lie the probe never confirms.

Note that a banner naming **both** vendors resolves to BSD, by rule order:
`Apple diff (based on FreeBSD diff)` is `bsd`, and `diff@bsd` depends on that.

### There is no Linux distribution dimension, and there should not be

A recurring proposal is to key overlays on the distribution — `ls@debian`,
`ls@fedora`. The measurements say no. GNU coreutils `ls`, compared across three
distributions running three different releases:

| Reference | coreutils | Flags |
|---|---|---|
| Debian stable-slim | 9.7 | 83 |
| Ubuntu 24.04 | 9.4 | 83 |
| Fedora 41 | 9.5 | 83 |

Not merely the same count — the flag sets are **identical**, and `sort` matches
across all three too. Distributions package the same upstream source; changing
the option set would make it something other than coreutils.

Compare that with a different *implementation* of the same command:

| Reference | Flags |
|---|---|
| GNU coreutils `ls` | 83 |
| BusyBox `ls` | 21 |

Divergence tracks the implementation, which `variant` already captures. A distro
dimension would multiply the overlay count by a factor whose every copy is
identical — the same arithmetic that ruled out a shared base layer, run in
reverse.

What genuinely does vary is covered already: **version** by `version_range`
(a compile-time option such as SELinux `-Z` shows up here), **kernel/userland**
by `platform`, and anything else by `probe.output_contains`, which is decisive.
When a real difference cannot be pinned down, the answer is `mode: merge`, not a
new axis — the gap then degrades to what the scanner actually observed on the
machine in front of it.

---

## A single-dash form may be longer than one character

`find`'s option set is not `-a -b -c`. Its entire expression language is
single-dash multi-character tokens — `-name`, `-delete`, `-maxdepth`,
`-files0-from`, `-newerat` — and they are what a user types, so `short` has to
carry them:

```json
{ "short": "-maxdepth", "type": "integer", "value_name": "n", "description": "..." }
```

The JSON Schema's `^-[^-]` pattern always allowed this; the Rust validator was
stricter and required exactly two characters, which made `find` inexpressible.
The validator now matches the schema. What is still rejected is a bare `-` and
a name with no leading dash, which is the typo the check exists for.

Two consequences worth knowing:

- **`-files0-from` is not a long option.** GNU `find` spells it with one dash;
  `--files0-from=FILE` is rejected as an unknown predicate. Getting this
  backwards produces an overlay that looks right and never works.
- **A single-dash multi-character name is not automatically an option.** BSD
  `chmod` accepts `-w`, `-r` and `-x` because they are *mode* syntax that
  `getopt(3)` is deliberately allowed to swallow, not because they are flags.
  Ask the binary what a token does before listing it.

---

## Long-running flags

`tail -f` may never terminate. An agent invoking it through apexe blocks until
the harness timeout kills the subprocess — this happened for a full two minutes
during overlay verification. The flag type therefore carries a boolean:

```json
{ "short": "-f", "type": "boolean", "long_running": true, "description": "..." }
```

It reaches `ScannedFlag.long_running` and then the emitted contract, as the JSON
Schema extension keyword `x-apexe-long-running` on that flag's property. An
extension keyword rather than a constraint, because `follow: true` is a
perfectly valid *value*; what it tells an executor is to bound the timeout or
refuse, not to reject the input.

Three rules for setting it:

- **It belongs to a flag, not a command.** `tail` is fine; `tail -f` is not.
- **The claim is "may not terminate on its own", never "always blocks".** The
  BSD man page states `tail -f` returns immediately when its input is a pipe, so
  a field asserting certainty would be false. Write the description the same
  way.
- **Flags that change *how long* a follow lasts are out of scope.** GNU `tail
  --retry` and `--sleep-interval` extend a follow; `--pid` and
  `--max-unchanged-stats` bound one. None of them decides termination on their
  own, and marking them would dilute the signal.

Set today on exactly four flags, all verified against the real binaries: BSD
`tail -f` and `-F`, GNU `tail -f/--follow` and `-F`. `less`, `top`, `watch`,
`ping` and `yes` have the same property but no overlay yet — do not guess at a
tool you have not checked.

---

## Open design questions

Deferred deliberately — recorded here so they are decided once, on evidence,
rather than improvised inside an unrelated change.

### BSD `tail`'s runtime usage line contradicts its own man page

The binary prints `[-q]` where its SYNOPSIS prints `[-qv]`. `-v` is real and
documented, so `tail@bsd` follows the man page. Noted in that overlay's
`provenance.notes`. If more cases like this appear, the scanner may need to
prefer one source explicitly rather than leaving it to whoever writes the overlay.
