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
python3 -c "import json;print('\n'.join(sorted(f['long'] for f in json.load(open('overlays/ls@gnu-coreutils.json'))['flags'] if f.get('long'))))"
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
  "source": "help",
  "checked_on": "2026-07-27",
  "command": "docker run --rm debian:stable-slim ls --help",
  "environment": "debian@sha256:328d16499860ae6cb9b345e2e4cebca08c2a36e4f7278482c7bd1f39d71e5bfd",
  "notes": "Debian slim images ship no man pages, so the flag list was read from GNU --help."
}
```

`command` should be re-runnable verbatim by someone else. `environment` should identify the exact reference build — an image digest, not a tag, since tags move.

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

---

## Traps we have actually hit

**Same name, different meaning.** Of the 38 flags `ls` shares between its BSD and GNU variants, **zero** have identical descriptions. Some differ in real behavior, not just wording:

```
-h  bsd: With -l, use unit suffixes (Byte, Kilobyte, ... Petabyte).
    gnu: With -l and -s, print sizes like 1K 234M 2G.
```

GNU's `-h` also affects `-s`; BSD's does not. Never copy a description across variants.

**This is also why overlays are one file per variant.** A shared base with per-variant overrides would invite exactly this mistake — the author assumes differences are confined to what they explicitly override, and a difference like `-h` above is only visible if you read both man pages line by line.

**Knowledge-written overlays miss new flags.** An earlier revision of `ls@gnu-coreutils.json` was written without a GNU reference. Checked against coreutils 9.7 it scored 100% precision but missed `--zero`, added in 8.25. Everything it did contain was right, which is precisely what makes this failure mode hard to notice.

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

## Open design questions

Deferred deliberately — recorded here so they are decided once, on evidence, rather than improvised inside an unrelated change.

### Long-running flags have no machine-readable representation

`tail -f` / `--follow` never terminates on its own. An agent invoking it through
apexe blocks until the harness timeout kills the subprocess; this happened for a
full two minutes during overlay verification. The same applies to `tail -F`, and
to `less`, `top`, `watch`, `ping` and `yes` — so it is not a `tail` patch.

Today this is expressible only as prose in a flag's `description`, which no
executor can act on. A boolean such as `long_running` on `flag` would let the
adapter refuse the invocation, or apply a bounded timeout, instead of letting an
agent hang.

Two constraints on any design:

- The property belongs to a **flag**, not to the command. `tail` is fine; `tail -f` is not.
- "Always blocks" would be a false claim. The BSD man page states `tail -f` returns
  immediately when the input is a pipe, so the semantics need room for
  "blocks depending on its operand".

Adjacent flags modify duration rather than termination — GNU `tail --retry` and
`--sleep-interval` extend a follow, `--pid` and `--max-unchanged-stats` bound
one. A design should say whether those are in scope.

### BSD `tail`'s runtime usage line contradicts its own man page

The binary prints `[-q]` where its SYNOPSIS prints `[-qv]`. `-v` is real and
documented, so `tail@bsd` follows the man page. Noted in that overlay's
`provenance.notes`. If more cases like this appear, the scanner may need to
prefer one source explicitly rather than leaving it to whoever writes the overlay.

### Apple-ported tools are not classified

macOS `sort` answers `--version` with `2.3-Apple (197)` and is classified
`unknown`: the banner carries no BSD token, and it is not GNU. `diff` escapes
this only by luck — `Apple diff (based on FreeBSD diff)` happens to name FreeBSD.

The obvious fix, adding `apple` to the BSD banner tokens, is **wrong**: `curl`
reports `curl 8.1.2 (x86_64-apple-darwin)`, so the token also appears in target
triples of tools that are not BSD at all. Position is no help either — "Apple"
is the third token in `sort`'s banner and the sixth in `curl`'s.

Consequence: an overlay for `sort` cannot declare `variant: bsd`, because the
probe will never produce that verdict and the overlay would never match. Until
this is decided, such tools need either `variant: unknown` or a `match` block
resting on `platform` + `binary_globs` + `probe.output_contains`.

Worth deciding together with whether `ToolVariant` should gain an `apple`
member, or whether "the vendor's own port of a BSD userland" is better expressed
as BSD with a separate provenance note.

### `gnu-coreutils` is narrower than "GNU"

The GNU probe matches the literal banner `GNU coreutils`, but plenty of GNU
tools ship in other packages: `diff --version` reports `diff (GNU diffutils)
3.10`, and `grep`, `sed`, `awk`, `tar` and `find` are each their own project.
All of them classify as `unknown` today.

So `ToolVariant::GnuCoreutils` is really "GNU, and specifically coreutils",
while overlay authors will reasonably read it as "the GNU one". Options:

- Add a broader `Gnu` variant and keep `GnuCoreutils` for cases where the
  package genuinely matters.
- Match on `GNU ` as a prefix and record the package in provenance instead.
- Add one variant per GNU package, which scales badly.

This blocks GNU-side overlays for every non-coreutils GNU tool, so it is worth
settling before the overlay set grows much further.
