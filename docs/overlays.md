# Authoring Tool Overlays

> **Where a new entry goes.** The corpus is maintained in
> [cli-permissions](https://github.com/aiperceivable/cli-permissions), which is
> upstream; `overlays/` in this repository is a vendored snapshot of the set
> apexe ships built in. Write and verify an entry there. This document is the
> procedure for doing so, and it applies wherever the file ends up — including
> `~/.apexe/overlays/`, which needs no repository at all.

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

> Writing one is this document. **Reading** one — selecting which overlay
> applies, what `mode` and `confidence` oblige you to, and the four placement
> fields that decide an `argv` — is
> [Reading Overlays Without apexe](overlay-consumers.md).

## Where the file goes

An overlay has three possible destinations, and they cost different amounts.

**`~/.apexe/overlays/*.{json,yaml}` — no code change, no rebuild.** The directory
is read at scan time, so dropping a file there is enough: the next
`apexe scan <tool>` picks it up. It also *outranks* the built-ins at equal match
strength, so this is where a local correction or a distributed corpus belongs.
`--overlay <PATH>` is the same thing for one invocation, and outranks both.

**A directory listed in `overlay_dirs` — no code change, and not yours to
maintain.** Config (`overlay_dirs:` in `config.yaml`, or `APEXE_OVERLAY_DIRS`)
names extra directories read *before* `~/.apexe/overlays/`. This is where a
corpus someone else publishes belongs — a team policy repository, a checked-out
data set, a plugin that ships overlays — so that consuming it does not mean
copying files into a directory apexe also treats as your own scratch space. A
listed directory that does not exist is warned about, not ignored silently.

**A packaged corpus in a well-known location — nothing to configure.** apexe
also reads `$XDG_DATA_HOME/cli-permissions/overlays`,
`/usr/local/share/cli-permissions/overlays`, `/usr/share/…`, and Homebrew's
prefix on macOS. That is where a package manager should install the corpus, so
that installing it is enough. It ranks below both directories above, so a local
correction always wins.

**apexe itself carries none.** There is no built-in set to add to: the corpus is
the `cli-permissions` repository, and apexe reads it like any other consumer.

## Checklist

1. Confirm heuristic scanning genuinely cannot do the job.
2. Obtain the reference installation — the host for BSD, a container for GNU/BusyBox.
3. Read the real flag list from `man`/`--help`. Not from memory.
4. Transcribe flags, then add `conflicts_with` by reading the prose.
5. Diff your list against the reference in both directions; sanity-check your extractor before believing a difference.
6. Record `provenance` with a re-runnable `command` and a digest-pinned `environment`.
7. Choose `authoritative` only if the overlay enumerates the option set completely; otherwise `merge`.
8. Test it with `apexe scan <tool> --overlay <file> --no-cache`. **`--no-cache` is not optional here** — see below.
9. Open a pull request against [cli-permissions](https://github.com/aiperceivable/cli-permissions). Nothing is needed to use it locally — `~/.apexe/overlays/` picks it up as soon as it is on disk.
10. Run `cargo test` in apexe with `APEXE_TEST_CORPUS` pointing at your corpus, so the tests that assert against real entries actually run.

### `--no-cache` when testing an overlay

Overlays are applied at scan tier 4. A cached scan result is replayed from tier
2, so the overlay is *loaded* and then never *applied* — and the only difference
in the output is a `Using cached scan result` line that looks like good news:

```bash
apexe scan tar --overlay ./tar@bsd.json --no-cache --output-dir ./out
```

Confirm it actually ran by looking for `Applying curated overlay` in the log and
`Scan tier: 4` in the summary. If you see tier 2, the overlay did nothing and
whatever you concluded from that binding is about the scanner, not your file.

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

## Flag risk

Two things a scan cannot infer, and the reason it cannot: help text does not
distinguish `--exec-path=<path>`, which sets a search path, from
`--upload-pack=<cmd>`, which runs one. Both read as "takes a value" to every
parser apexe has. Guessing produces exactly the failure this document exists to
prevent — a plausible, mostly-correct assertion that omits the flag that
mattered — so the field defaults to absent and only a human raises it.

```json
{ "long": "--upload-pack", "type": "string", "risk": "executes", "description": "..." }
{ "long": "--privileged",  "type": "boolean", "risk": "escalates", "description": "..." }
```

| `risk` | Meaning | What apexe does |
|--------|---------|-----------------|
| absent / `"none"` | Nobody has said. | The name-based floor decides. |
| `"benign"` | A human checked; the floor is wrong about this flag. | The floor is overruled for this flag only. |
| `"escalates"` | Sending it changes the operation's blast radius. | The call is put to the approval gate — but only when the flag is actually sent. |
| `"executes"` | The value **is** a command the tool will run. | The call is refused outright. |

`escalates` is for the flag that changes what an ordinary command does to the
world: `git push --force`, `docker run --privileged`, `rm --no-preserve-root`.
Without it the command is unremarkable, which is why marking the *command*
would be wrong — it would prompt on every `git push`, and a gate that prompts
constantly gets switched off. It reaches the contract as `x-apexe-escalates`.

A short list of common names (`--force`, `-f`, `--all`, `--recursive`, …) is
applied to every tool without an overlay saying anything. `escalates` exists for
the ones no name list generalizes: nothing about the string `--privileged`
suggests danger.

`benign` is the other direction, and it exists because that name list matches on
spelling — the only thing available without per-tool knowledge — and spelling is
sometimes a coincidence:

```json
{ "short": "-y", "type": "boolean", "risk": "benign", "description": "Compress the archive with bzip2." }
```

`bsdtar -y` is bzip2, `sort -f` folds case, `cut -f` selects fields. None of them
mean "yes" or "force". Before this existed, the only way to stop the prompt was
to assert `requires_approval: false` for the whole command — which would also
say `tar` may overwrite files unattended. `benign` suppresses one flag and
leaves the rest of the command's gating intact.

It reaches the contract as an explicit `x-apexe-escalates: false`, so the
keyword is three-state: absent means the floor decides, `true` and `false` are
both human assertions. It overrides the name list only — it cannot cancel a
`destructive` command-level annotation, and writing it means you read the flag's
documentation, the same standard every other field here holds.

`executes` is stronger and is not a severity notch above `escalates` — it is a
different claim. apexe's central guarantee is that argv reaches `execve` with no
shell on the path, so a metacharacter is an inert byte rather than something to
blacklist. A flag whose value becomes a command hands that guarantee back: the
wrapped tool spawns a shell of its own with a string the caller chose, and no
JSON Schema constrains what that string becomes. Such a parameter is therefore
**refused, not gated** — there is nothing for a human to approve, because
approving would mean predicting what an arbitrary string will do. It reaches the
contract as `x-apexe-exec`.

Known members of this class, none of which look dangerous in a help listing:

| Tool | Flags |
|------|-------|
| `git` | `--upload-pack`, `--receive-pack`, `rebase --exec`, `-c core.pager=`, `-c core.sshCommand=` |
| `tar` | `--use-compress-program`, `-I`, GNU's `--to-command`, `--checkpoint-action=exec=` |
| `rsync` | `-e`, `--rsync-path` |
| `find` | `-exec`, `-ok` |

Verify each against a real installation before asserting it — the same rule as
every other field here. Marking a flag `executes` that is not stops a legitimate
call; failing to mark one that is leaves the hole open.

## Optional-value flags

`git --exec-path` prints the path to git's core programs; `git --exec-path=<p>`
sets it. One flag, two legal spellings, two different meanings — and the only
thing in the help text that says so is the bracket in `--exec-path[=<path>]`.
A flag therefore carries a boolean:

```json
{ "long": "--exec-path", "type": "path", "value_name": "path", "value_optional": true, "description": "..." }
```

It reaches `ScannedFlag.value_optional` and then the emitted contract as
`x-apexe-value-optional`, alongside a union type — `"type": ["string", "boolean"]`
— so both spellings are expressible: `true` selects the bare form and a string
supplies a value. Without the union one of the two is unreachable, which is what
makes this a correctness field rather than a documentation one.

Omitting it is not the safe default. The marker is what makes the executor
render `--flag=value`; without it the value is emitted as a separate argv entry,
and an optional-value option reads that as *"no value, and here is an operand"*.
The result is a wrong answer rather than a refusal, in two directions at once:

```
ls --color never .     # lists a file called `never`, AND leaves colour ON
ls --color=never .     # what the caller asked for
mkdir --context ctx d  # exit 0, having created a spurious directory `ctx`
```

Four rules for setting it:

- **Probe both spellings against the binary.** Run `<tool> --flag VALUE
  <operands>` and `<tool> --flag=VALUE <operands>`. The value is optional when
  the separated form loses `VALUE` into the operand list; it is required when
  both forms behave the same. Bare-form-works is a weaker signal — probe the
  separated form, because that is the one apexe would otherwise emit.
- **An enum is not evidence.** The temptation is to infer the marker from
  `enum_values`, and it goes the wrong way on real flags: GNU `ls --sort=WORD`
  and `sort --sort=WORD` are both enum-valued with a *required* value, while
  `--color` is enum-valued and optional. The distinction is documented per flag
  and is not derivable from the schema.
- **Never carry it across variants.** BSD `grep --context` takes an optional
  value; GNU `grep --context=NUM` requires one. Same flag, same name, opposite
  argv shape.
- **It is about arity, not about the value's type.** `value_name` and `type`
  still describe the value when one is given.

On GNU tools `--help` states the answer outright — `--opt[=VAL]` is optional and
`--opt=VAL` is required — which is why the GNU overlays record `source: help`.
BSD man pages have no such notation, so there the probe is the only evidence.
Either way, run it: the notation is a claim, not a result.

Note that the marker travels with the *long* form. apexe emits the long literal
when a flag has both (`x-apexe-flag`), and the short form's arity is frequently
different: `diff`'s help line reads `-c, -C NUM, --context[=NUM]`, so `-C` takes
a required, separated value while `--context` does not.

A change to any of this is a change to what apexe understands about an unchanged
binary, so it also needs `SCAN_FORMAT_VERSION` bumped in `src/scanner/cache.rs`
— otherwise an existing install keeps serving the cached, pre-fix contract.

---

## Operand placement

Almost every tool's grammar is `tool [options] operands`, and the renderer
emits operands after the flags. `find` inverts it: its grammar is `find path
... [expression]`, and its predicates are spelled like options (`-name`,
`-type`) but are evaluated per file, so they must follow the paths. Rendering
`find -name '*.txt' dir` is not merely unconventional — both variants reject it
outright:

```
/usr/bin/find: illegal option -- n                       # BSD, exit 1
find: paths must precede expression: `/t'                 # GNU findutils, exit 1
```

A positional arg therefore carries a boolean:

```json
{ "name": "path", "type": "path", "variadic": true, "before_flags": true, "description": "..." }
```

It reaches `ScannedArg.before_flags` and then the emitted contract, as
`x-apexe-operand-position: "before-flags"` on that property. The executor
renders marked operands ahead of every flag and unmarked ones after, each group
still ordered by its recorded index — which is why placement is a separate
keyword rather than a sign on the index: `find` has one operand on each side of
the flags and both need their relative order kept.

Three rules for setting it:

- **It belongs to an operand, not to the flags.** `find`'s constraint is on
  where the paths go, not on any one of its 98 primaries. Marking the operand
  once is both correct and the only thing that scales.
- **It is not derivable, so it is stated or it is absent.** The usage line
  gives the operand order but never says which of a tool's options live inside
  the trailing operand slot — `find`'s predicates are documented in PRIMARIES,
  not in the synopsis. There is deliberately no heuristic for this; a scan
  without an overlay keeps the ordinary trailing-operand behaviour.
- **Check it by running the binary, both ways.** The wrong ordering fails
  loudly for `find`, which makes it cheap to verify and worth doing rather than
  reasoning from the synopsis.

Set today on exactly two operands, both verified against the real binaries:
`find@bsd`'s `path` and `find@gnu`'s `path`. `xargs`, `nice`, `env`, `timeout`
and `sudo` all place an operand before a trailing command and may need the same
treatment — do not guess at a tool you have not checked.

### `end_of_options` — passing a value that begins with `-`

apexe refuses any caller-supplied value starting with `-`, because the wrapped
tool's parser cannot distinguish it from an option the caller was never granted.
That is the right default, and it is wrong for the parameters that exist for
exactly this case: every one of `find`'s expression primaries begins with a
dash, and `-f` is documented as the way to name a path that would otherwise
parse as one.

`--` is what the tools themselves provide, and it is the stronger guarantee —
after the separator the wrapped parser *cannot* read the value as an option,
whereas refusing only establishes that the value did not look like one. So the
overlay states it at the root:

```json
{ "command": "find", "variant": "bsd", "end_of_options": true }
```

It reaches `ScannedCommand.end_of_options` and then the contract, as
`x-apexe-end-of-options: true` at the **schema root** — it describes the whole
invocation, not any one property. The executor then permits a `-`-leading value
and emits `--` at the point the command stops reading options: ahead of the
operands for a `before_flags` grammar like `find`, after every flag otherwise.
The separator is emitted only when some value actually needs it, so no existing
invocation is rewritten.

Three rules for setting it:

- **Verify the position, not just the support.** `--` works in exactly one
  place. Run both: `find -- . -name '*.txt'` exits 0 on BSD and GNU alike,
  while `find . -- -name '*.txt'` is `find: --: unknown primary or operator`
  (BSD) and ``find: unknown predicate `--'`` (GNU). Record both outcomes in
  `provenance.notes`.
- **Absent means refuse, and that is safe.** A tool nobody has checked keeps
  the rejection. A wrong guess would instead produce an invocation that *runs
  and fails*, which is worse than one that is refused with a message.
- **`--` protects the expression, not the paths.** A command that re-parses
  operands in a second pass still reads a leading `-` there as its own syntax:
  `find -- -weird-dir` is a usage error on BSD and "unknown predicate" on GNU.
  For `find` the documented route is `-f`, which the marker is what makes
  usable (`find -f ./-weird-dir -- -name '*.txt'` works; without `--` it is
  `illegal option -- n`). If a tool has this shape, say so in the operand's
  description rather than implying the separator covers it.

Set today on exactly two commands, both verified against the real binaries:
`find@bsd` and `find@gnu`.

---

## Open design questions

Deferred deliberately — recorded here so they are decided once, on evidence,
rather than improvised inside an unrelated change.

### BSD `tail`'s runtime usage line contradicts its own man page

The binary prints `[-q]` where its SYNOPSIS prints `[-qv]`. `-v` is real and
documented, so `tail@bsd` follows the man page. Noted in that overlay's
`provenance.notes`. If more cases like this appear, the scanner may need to
prefer one source explicitly rather than leaving it to whoever writes the overlay.
