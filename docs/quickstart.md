# apexe Quick Start

Install, initialize this machine, inspect its command modules, then choose local
execution, shell activation or an MCP/A2A server.

## 1. Install

Use a [prebuilt release](https://github.com/aiperceivable/apexe/releases), or build
from source with Rust and Cargo:

```bash
git clone https://github.com/aiperceivable/apexe.git
cd apexe
cargo install --path .
apexe --version
```

Git is required to acquire rules snapshots. Command binaries must already be
installed on this machine.

## 2. Initialize

```bash
apexe init
```

This obtains the configured rules source, normally
[cli-permissions](https://github.com/aiperceivable/cli-permissions), and discovers
commands declared by the loaded rules. For installed tools it uses the existing
scanner to resolve binaries, detect variants and match platform/version/probe
conditions. It writes bindings under `~/.apexe/bindings/` and a starter policy at
`~/.apexe/acl.yaml`. Missing tools and unmatched variants are reported and skipped.
It does not scan every executable on PATH, install programs, activate shims or
start a server.

Read the report for initialized and preserved bindings, skipped tools, warnings, native-ready module IDs
and rules commits. `apexe config --show` displays configured values; its
`default_overlay_repository` URL alone does not prove a snapshot or bindings
exist. `overlay_dirs: []` still permits managed rules snapshots. There is no
separate `apexe overlays` command.

```bash
# Optional source override for this invocation: HTTPS/GitHub or local Git.
apexe init --overlays https://github.com/aiperceivable/cli-permissions
apexe init --overlays /path/to/cli-permissions
# Optional reviewed revision.
apexe init --overlays /path/to/cli-permissions --ref <COMMIT>
# Use cached/local rules without network access.
apexe init --offline
# Inspect a machine-readable report.
apexe init --format json
```

For a standing mirror or source, configure `default_overlay_repository` or
`APEXE_DEFAULT_OVERLAY_REPOSITORY`. Normal init reuses the cached commit and
preserves existing bindings. `apexe init --refresh` updates the selected source
and regenerates unchanged bindings created by init. Custom and edited bindings
are preserved. A pinned commit stays pinned until you select another `--ref`.
Any explicit-source acquisition or validation failure, or a failed refresh,
exits nonzero even when other local rules are available.

If the default source cannot be acquired and no local rules are usable, init
reports degraded coverage and scans only the fallback core (`cat`, `head`, `ls`,
`wc`) with built-in classification. Unknown modules remain denied in a new ACL.
Fallback bindings lack reviewed native grammar, so use their JSON module mode
when permitted; native exec/env may remain unavailable until matching rules are
installed. A run with scan failures or no initialized/preserved modules exits
nonzero after reporting its outcome.

If apcore configuration sets a custom `bindings.pattern`, it must include the
canonical `*.binding.yaml` files init generates. Excluded existing bindings are
preserved with a report warning; excluded new filenames are refused before
bindings and ACL are written.

## 3. List and review

```bash
apexe list
apexe list --verbose
apexe list --format json
```

Module IDs normally use the `cli.*` namespace. Inspect the bindings and generated
ACL before executing. A new policy defaults to deny; local readonly operations
receive allow rules, while destructive, network-reaching and unknown modules
are denied. Initialization preserves manual rules and an existing policy default.
These are generated suggestions rather than your own review.

## 4. Execute locally

Module JSON execution works with initialized bindings when policy permits:

```bash
apexe exec cli.ls
```

The optional native examples below require matching reviewed grammar. Check
`native_ready` in the init report; GNU scanning support alone is insufficient.
The current native pilot covers BSD cat/head/ls.

```bash
apexe exec --dry-run -- ls -la
apexe exec -- ls -la
printf 'one\ntwo\n' | apexe exec -- cat | apexe exec -- head -n1
# Captured JSON mode; use your binding's input property names.
apexe exec cli.ls --input '{"file":["."]}'
```

Native argv needs reviewed grammar; the current BSD pilot covers cat/head/ls.
Put apexe options before `--`, then the command and its arguments. The shell
handles pipelines; each stage above enters apexe. stdin is inherited only with
an explicit reviewed capability. Local execution loads the default ACL when
present and uses caller `local`; approval-required local calls fail closed.
See the [exec reference](user-manual.md#47-apexe-exec) for unmanaged policies.

## 5. Optionally activate your shell

Use this step only when initialization reports native-ready modules. An
installation with no eligible native bindings can use module JSON and serve
instead.

```bash
apexe env --enable
source ~/.apexe/bin/apexe-activate.sh
apexe env --check
printf 'one\ntwo\n' | cat | head -n1
source ~/.apexe/bin/apexe-deactivate.sh
```

The installed helpers require no repository checkout. Activation automatically
enables or reloads eligible shims, updates the current shell and clears its
command cache; no additional eval or rehash is needed. POSIX sh uses `.` rather
than `source`. Deactivation removes owned links/startup blocks and restores the
saved environment; the helpers remain available for reactivation. Other commands,
builtins, aliases and absolute paths remain outside shim coverage. See the
[env reference](user-manual.md#48-apexe-env) for supported shells and checks.

After refreshing initialized bindings, refresh an active shim installation:

```bash
apexe init --refresh
apexe env --reload
```

## 6. Optionally serve agents

```bash
apexe serve --acl ~/.apexe/acl.yaml
# HTTP mode with the browser explorer.
apexe serve --transport http --port 8000 --explorer --acl ~/.apexe/acl.yaml
# A2A uses the same initialized command bindings.
apexe a2a --acl ~/.apexe/acl.yaml
```

Local exec/env load the default ACL automatically when present. MCP/A2A servers
currently require the explicit `--acl` flag; initialization does not enable
server enforcement. HTTP-family transports require authentication by default.
See the [threat model](threat-model.md) before exposing tools to agents.

### Claude Desktop and Cursor

```bash
apexe serve --show-config claude-desktop --acl ~/.apexe/acl.yaml
apexe serve --show-config cursor --acl ~/.apexe/acl.yaml
```

Copy the relevant output to your client's MCP configuration, then restart it.
If bindings are elsewhere, include `--bindings-dir` when generating the snippet.
Transport credentials are never included. MCP approval prompting requires an
elicitation-capable client and `--enable-approval`; A2A has no interactive
elicitation transport.

## 7. Customize and maintain

Routine use after initialization is `list`, `exec`, `env` or `serve`. Use `scan`
for commands outside the initialized rules or a targeted rescan:

```bash
apexe scan git jq my-tool
apexe scan git --depth 3 --no-cache
# Explicit single-tool overlay, after verifying it applies to this installation.
apexe scan my-tool --overlay ./my-tool.json
# Read uncommitted rule edits during development.
APEXE_OVERLAY_DIRS=/path/to/cli-permissions/overlays apexe scan ls --no-cache
```

Scans merge generated ACL rules and preserve unrelated bindings. A custom scan
is an explicit request to regenerate that tool's bindings, including ones init
would otherwise preserve. The first online scan can also acquire the default
corpus automatically; `scan --offline` prevents that download. Refresh enabled
shims after binding changes. `config --init` only writes a configuration template;
it does not initialize command coverage.

### Check source drift and explicit behavior

```bash
apexe verify --report ./drift-report.json
apexe verify --cases ./readonly-cases.json --record
# Digest-pinned dry-run cases require explicit authorization and working isolation.
apexe verify --cases ./dry-run-cases.json --allow-dry-run --isolation required --record
# Run a disposable demo with no external corpus or destructive commands.
python3 examples/verification/demo.py --apexe target/debug/apexe
```

Prepare cases against the actual scanned input schema. `--record` stores
case-scoped evidence; binding, executable or case-file changes invalidate it.
The case modes are `readonly`, `dry_run` and `isolated`. Isolation uses macOS
`sandbox-exec` or Linux `bwrap` with unprivileged namespaces. Unavailable required
isolation reports `unverifiable` and runs no case. See
[verification](verification.md) and [the demo](../examples/verification/README.md).

### Import an existing HTTP API with OpenAPI

Supply an OpenAPI 3.0/3.1 JSON or YAML document and a reviewed ACL to either
server. Set `APEXE_OPENAPI_TOKEN` in the launch environment if the upstream API
requires a bearer credential:

```bash
apexe serve --openapi ./openapi.yaml \
  --openapi-base-url https://api.example.com --acl ./acl.yaml --no-log-arguments
apexe a2a --openapi ./openapi.yaml \
  --openapi-base-url https://api.example.com --acl ./acl.yaml --no-log-arguments
```

Operations use the `api.*` namespace by default. They share schema validation,
ACL and auditing with CLI modules. A2A transport authentication is separate
from the upstream API credential. `--openapi-header` authenticates specification
fetching only. See the [OpenAPI guide](openapi.md) for supported request formats
and proxy limits.

Try both MCP and A2A against a temporary local API from the repository root:

```bash
cargo build --all-features
python3 examples/openapi_import/demo.py --apexe target/debug/apexe
# Also fetch a protected specification with a separate credential:
python3 examples/openapi_import/demo.py --apexe target/debug/apexe --remote-spec
```

The [OpenAPI import demo](../examples/openapi_import/README.md) checks GET path
and query parameters, POST JSON bodies, ACL-denied DELETE and credential
separation. It requires Python 3 and creates its own local API and credentials.

### Explore more options

```bash
# Deep scan with 3 levels of subcommands
apexe scan git --depth 3

# HTTP server for remote agents
apexe serve --transport http --port 8000 --acl ~/.apexe/acl.yaml

# SSE transport (deprecated upstream -- prefer --transport http)
apexe serve --transport sse --port 8000 --acl ~/.apexe/acl.yaml

# Also generate Claude Skills (.claude/skills/<id>/SKILL.md) per module
apexe scan git --skills-dir ./out

# Observability: /metrics (Prometheus) + /usage (HTTP/SSE only)
apexe serve --transport http --metrics --acl ~/.apexe/acl.yaml

# Initialize a config file for customization
apexe config --init

# View resolved configuration
apexe config --show
```

See [User Manual](user-manual.md) for full documentation.
