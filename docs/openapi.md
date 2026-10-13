# OpenAPI import

Connect an existing HTTP API to apexe by supplying an OpenAPI 3.0 or 3.1
JSON/YAML document to `serve` or `a2a`. Operations are registered alongside CLI
bindings and share the executor's validation, ACL, approval configuration,
filters and audit sink. No helper executable or persisted HTTP binding is
required; the source is loaded again when the server starts.

## Try the runnable example

The checked-in example starts its own loopback API and drives both real server
entry points, including an actual MCP initialization and authenticated A2A calls:

```bash
cargo build --all-features
python3 examples/openapi_import/demo.py --apexe target/debug/apexe --remote-spec
```

See the [example walkthrough](../examples/openapi_import/README.md),
[OpenAPI input](../examples/openapi_import/openapi.json), and
[default-deny ACL](../examples/openapi_import/acl.yaml). The demo reads a pet,
creates in-memory data through a JSON body, and proves that a denied DELETE
never reaches the API. All credentials and process state are temporary.

## Import a document

```bash
# Local JSON or YAML, alongside the configured CLI binding directory.
apexe serve --openapi ./openapi.yaml \
  --openapi-base-url https://api.example.com --acl ./acl.yaml --no-log-arguments

# A protected remote specification can use separate fetch-only headers.
apexe serve --openapi https://api.example.com/openapi.json \
  --openapi-header 'X-Spec-Key: <specification credential>' \
  --openapi-prefix api.inventory --acl ./acl.yaml --no-log-arguments

# The A2A entry point accepts the same source options.
apexe a2a --openapi ./openapi.yaml \
  --openapi-base-url https://api.example.com --acl ./acl.yaml --no-log-arguments
```

`--openapi-base-url` overrides the first absolute `servers[0].url`; give an
absolute HTTP(S) origin when the source lacks one. The default namespace is
`api`; `operationId: getPet` becomes `api.get_pet`. Namespaces must follow
apcore module-ID rules. Module-ID collisions with CLI bindings fail startup.

| Option | Meaning |
|---|---|
| `--openapi <SOURCE>` | Local JSON/YAML file or exact HTTP(S) specification URL |
| `--openapi-base-url <URL>` | Absolute API base URL override |
| `--openapi-prefix <PREFIX>` | Imported namespace; defaults to `api` |
| `--openapi-header <NAME: VALUE>` | Repeatable specification-fetch headers |
| `--openapi-token <TOKEN>` | API bearer token, or `APEXE_OPENAPI_TOKEN` |
| `--timeout <SECONDS>` | Bound specification fetching and upstream calls |

## Keep the three credentials separate

| Credential | Destination |
|---|---|
| `--openapi-header` | Fetching the specification only |
| `APEXE_OPENAPI_TOKEN` / `--openapi-token` | Upstream API operations only |
| `APEXE_AUTH_TOKEN` / `--auth-token`, or `--auth jwt` | Incoming MCP HTTP or A2A requests |

MCP stdio inherits its process boundary and does not need transport auth.
MCP HTTP and A2A default to token authentication. Use launch environment
variables for persistent credentials. `--show-config` preserves the OpenAPI
source, origin and namespace but omits tokens and specification headers.

```bash
apexe serve --openapi ./openapi.yaml --acl ./acl.yaml \
  --show-config claude-desktop
```

Supply omitted credentials separately in the client's launch environment.
`--no-log-arguments` also keeps operation arguments and returned data out of
execution payload logs; audit records retain module, trace, caller and outcome.

## Governance and compatibility

`--tags` and `--prefix` filter both CLI and HTTP sources at registration, so
excluded operations are uncallable. A default-deny ACL can permit individual
API IDs; the example policy permits reads and its disposable creation operation
and denies DELETE. Imported operations are `open_world`. Destructive operations
also require approval when the gate is enabled; A2A has no interactive
elicitation and uses an approval store only through its library API.

Schema and ACL refusals happen before an HTTP request. The local subprocess
path guard does not govern a remote API's filesystem or data objects.

The current SDK proxy supports:

- Path parameters with escaped values.
- Query parameters on GET/DELETE/HEAD/OPTIONS/TRACE.
- JSON bodies on POST/PUT/PATCH.
- JSON responses.

A body method declaring query parameters fails startup because the SDK would
encode them in the body. Missing origins, malformed documents and registration
collisions also fail startup. Multipart uploads, per-input header/cookie
parameters and non-JSON responses are outside this initial import surface.
OpenAPI import is available to MCP/A2A; `apexe exec` and PATH shims remain CLI
execution surfaces.

## Test coverage

[OpenAPI integration tests](../tests/openapi_integration.rs) cover real path/query
calls, credential separation, schema/ACL refusals before network I/O, filtering,
CLI coexistence, collisions, invalid origins/encoding, protected YAML spec
fetching, both server builders, and an actual MCP CLI session.
[Example smoke tests](../tests/examples_smoke.rs) execute the runnable scripts,
covering local JSON and protected remote JSON specs, actual MCP/A2A calls, JSON
body creation and denied DELETE operations. Run the full suite with
`cargo test --all-features`.
