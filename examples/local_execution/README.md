# Governed local execution

This runnable example scans the installed cat/head tools, inspects a non-executing
preflight, executes cat with captured JSON output, and checks ACL denial and
audit records. When the scanned operand has a path marker, it also checks
protected-path refusal through preflight. Without that marker `path_denied` is
null; provide the reviewed corpus to exercise that check. Python 3 uses only its standard library.
The example uses a temporary HOME and working directory and removes them on
exit. Its scan uses `--offline` so the example performs no automatic corpus
download. The default run does not install shims; the optional native run installs
them only inside the temporary HOME. Your actual home is unchanged.

From the repository root:

```bash
cargo build --all-features
python3 examples/local_execution/demo.py --apexe target/debug/apexe
```

For Stage 2, provide the real reviewed companion corpus. The current native
pilot is BSD cat/head on macOS; other variants require their own verified
native capabilities. The example refuses when either tool is ineligible.

```bash
python3 examples/local_execution/demo.py --apexe target/debug/apexe \
  --corpus ../cli-permissions/overlays --shims
```

The Stage 2 path runs `apexe env --enable` to install shims and configure a
temporary bash startup file, starts a new interactive bash that automatically loads them,
executes `cat sample.txt | head -n1`, and uninstalls the owned links and startup
block. Expected native output is `one\n`. The final JSON summary reports the
checks that actually succeeded; a failed check exits nonzero.

For your own shell, scan with reviewed overlays and inspect the generated ACL
before activating shims:

```bash
APEXE_OVERLAY_DIRS=/path/to/cli-permissions/overlays apexe scan cat head --no-cache
apexe run cli.cat --input '{"file":["README.md"]}' --dry-run
# Install the helpers once, then activate this terminal.
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
printf 'one\ntwo\n' | cat | head -n1
# Update the installed shims after later scans.
source ~/.apexe/bin/apexe-activate.sh
# Uninstall shims and restore the terminal's previous environment.
source ~/.apexe/bin/apexe-deactivate.sh
```

Enabling detects the shell from `SHELL`. Use `--shell` and `--rc-file`
for a custom startup file. sh uses a login profile; PowerShell requires
`--shell powershell --rc-file "$PROFILE"`. The installed namespaced helpers
need no checkout and remain available after disable. Sourcing the activation
helper again automatically re-enables shims. POSIX sh uses `.` instead of
`source`; no extra `eval` or `rehash` is needed.
This optional demo requires bash; all startup changes remain inside its
temporary HOME.

Reload keeps the installed binding directory, ACL and filters, and preserves
startup configuration by default. If disabled, it automatically enables using
the current settings. The demo checks reload before starting its child shell.

Read the scanned schema rather than assuming a property name across variants.
Only explicitly marked modules inherit stdin. Builtins, aliases, functions,
absolute paths and `command -p` can bypass shims; plain `command cat` still
uses the activated PATH. See the [F8 specification](../../docs/features/v2-f8-local-execution.md)
and [command reference](../../docs/user-manual.md#47-apexe-run).

## Automated checks

The captured demo runs in `tests/examples_smoke.rs`. On macOS the native demo
is opt-in because it depends on the external reviewed corpus:

```bash
cargo test --all-features --test examples_smoke \
  test_local_example_runs_native_pipeline_and_owned_uninstall -- --ignored --exact
```
