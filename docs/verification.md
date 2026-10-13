# Verify scanned CLI contracts

`apexe verify` compares stored bindings with freshly read CLI help, man pages
and completions. Explicit cases additionally test concrete invocations. Start
with a scan of a trusted installed CLI:

```bash
apexe scan cat --no-cache
apexe verify --report ./drift-report.json
apexe verify --cases ./readonly-cases.json --record --report ./verification.json
```

The source baseline is captured before overlays. Breaking, unclassified or
unverifiable source changes fail the command; `--strict` also fails on additive
or cosmetic changes. Existing binding documents are preserved. Source probing
uses the scanner's trusted-CLI help/version/completion convention; behavioral
isolation applies to the explicit case executions.

## Explicit cases

Cases are JSON, limited to 1 MiB and 100 cases. Only allowlisted module IDs run.
Inputs must match the binding schema. String values are restricted to annotated
path parameters naming `$WORKDIR` or a declared `$FIXTURE/name`. Other supported
inputs are boolean or numeric literal options. No ambient files, credentials,
stdin programs or networked modules are sampled.

```json
{
  "allowlist": ["cli.cat"],
  "cases": [{
    "module_id": "cli.cat",
    "mode": "readonly",
    "input": {"file": ["$FIXTURE/input.txt"]},
    "fixtures": {"input.txt": "hello\n"},
    "expected_stdout": "hello\n",
    "expected_stderr": ""
  }]
}
```

Use the actual property name and scalar/array shape from your binding.
`expected_stderr` defaults to empty; exit zero and both expected streams must
match. Optional `expected_files` maps fixture filenames to expected contents.
Fixture and expected filenames must be single relative components. Digest-pinned
isolated/dry-run cases may also use an empty fixture map and boolean/numeric
inputs without path operands. Readonly declarations reject expected file mutations. Files are
created in a disposable work directory; HOME and the default-deny verification
ACL are temporary too. Output and execution time are bounded.

| Case mode | Admission and execution |
|---|---|
| `readonly` (default) | Readonly/idempotent, non-destructive, no approval or network requirement, and an independently reviewed system cat/head/ls root binary |
| `dry_run` | `--allow-dry-run`, explicit executable SHA-256, declared true dry-run boolean, and working OS isolation; fixture inputs are read-only |
| `isolated` | Readonly/idempotent, non-destructive, no approval or network requirement, explicit executable SHA-256, and working OS isolation; idempotent cases may check fixture writes with `expected_files`; readonly claims retain read-only inputs |

Digest-pinned modes admit literal subcommands matching `command_path`. Execution
wrappers and fixed option prefixes are refused. The ordinary readonly allowlist
continues to cover root cat/head/ls only. Every case uses a governed child with
its exact temporary binding and ACL. Cases that cannot be admitted are reported
as `unverifiable` with a reason; a requested failed or unverifiable case exits
nonzero. No cases means no behavioral execution.

## Explicit dry-run authorization

Declare `dry_run_parameter`, the input property whose literal switch is
`--dry-run`, `--check`, `--noop`, `--plan` or `--whatif`, and set it to true.
The property must be boolean. Its presence alone never authorizes execution:
the case also supplies the SHA-256 of the actual executable and the caller
must give `--allow-dry-run`.

```bash
# Calculate the installed executable's SHA-256 for the reviewed case file.
shasum -a 256 /absolute/path/to/cleanup
apexe verify --cases ./dry-run-cases.json --allow-dry-run \
  --isolation required --record --report ./dry-run-report.json
```

The case includes these fields in addition to the input, fixtures and expected
outputs:

```json
{
  "module_id": "cli.cleanup",
  "mode": "dry_run",
  "binary_digest": "REPLACE_WITH_THE_EXECUTABLE_SHA256",
  "dry_run_parameter": "dry_run",
  "input": {"dry_run": true, "file": "$FIXTURE/input.txt"}
}
```

Approval-required or destructive annotations can enter only this explicitly
authorized dry-run path. Its temporary binding scopes approval to the reviewed
dry-run invocation; the original module retains its normal annotations and ACL.
An incorrect dry-run implementation cannot write the fixture inputs, external
filesystem or network. A missing, false or unsupported switch, digest mismatch,
network annotation or unavailable isolation refuses execution.

## Local operating-system isolation

`--isolation auto|required|none` defaults to `auto`.

- macOS uses the system `sandbox-exec` with default denial, no network or Mach
  service access, and writes confined to the temporary case. Readonly/dry-run
  cases allow writes only in their temporary HOME.
- Linux uses [bubblewrap](https://github.com/containers/bubblewrap/blob/main/README.md) (`bwrap`) and enabled unprivileged user namespaces. The filesystem
  starts empty; system binaries/libraries and the tested executable are mounted
  read-only, host runtime/socket directories are absent, and network, IPC and
  PID namespaces are private. Only case data is writable. `bwrap` is an optional
  runtime dependency, not installed by apexe.
- Other platforms report required isolation as unavailable. They do not run
  dry-run or isolated cases without it.

Backend availability is probed before executing the tested command. `required`
refuses when the backend is missing or cannot start. `auto` permits fallback
only for the independently reviewed readonly binary allowlist. `none` also
applies only to that readonly subset. Both other case modes always require
working isolation. Timeout/completion cleanup terminates the verification process group; Linux
also tears down its private PID namespace.
This isolation is scoped to verification; normal module serving and execution
continue to use their configured host boundary.

## Earned confidence and invalidation

Source provenance and runtime evidence are distinct. The corpus's legacy
`confidence: verified` label remains an author claim requiring provenance;
apexe caps source-derived flag confidence at `high`. It earns module
`metadata.confidence: verified` only for passed explicit cases, an unchanged
source contract and matching recorded digests. `metadata.verification.scope`
is `explicit_cases`: it identifies the tested cases rather than certifying every
possible invocation or all flag behavior.

`--record` writes `.apexe-verification/evidence.json` under the binding directory.
It records the exact binding, executable, case-file and case digests, verifier identity, mode,
backend and time. Binding documents and their source baselines are preserved.
Subsequent binding loads validate those digests before attaching evidence.
Changed bindings, executor metadata, binaries, changed/missing case files or
verification executables/versions,
failed cases or missing evidence remove earned confidence. Malformed or
symlinked evidence records fail loading. Keep case files available at their
recorded absolute paths for reproducibility.

Each `--record` run replaces the evidence snapshot; include all cases/modules
whose evidence you want retained. A requested run that fails replaces prior
success with the new result. Without `--record`, verification prints a report
and does not update stored evidence. Reports can also be saved with `--report`
outside the binding directory. Evidence is trusted local state under the same
filesystem ownership boundary as bindings and ACLs, rather than a signed remote
attestation. Running servers retain their startup descriptors: reverify and
restart them after an upgrade.

Run the [self-contained example](../examples/verification/README.md) to exercise
dry-run authorization, fixture effects and automatic evidence invalidation.
