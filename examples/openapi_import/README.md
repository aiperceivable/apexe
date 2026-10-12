# Import an HTTP API from OpenAPI

This self-contained example starts an authenticated HTTP API on loopback,
imports the checked-in OpenAPI document into apexe, and calls it through MCP
and A2A. Python 3 uses only its standard library. No external service, API key,
client application or generated binding files are required.

From the repository root:

```bash
cargo build --all-features
python3 examples/openapi_import/demo.py --apexe target/debug/apexe
```

To also demonstrate authenticated specification fetching with credentials that
are distinct from the upstream API token:

```bash
python3 examples/openapi_import/demo.py --apexe target/debug/apexe --remote-spec
```

Use `--surface mcp` or `--surface a2a` to run one transport. The default is both.
The JSON summary confirms these real checks:

- GET `api.get_pet` interpolates the path ID and encodes a query parameter.
- POST `api.create_pet` sends the required JSON body to the in-memory API.
- DELETE `api.delete_pet` is refused by `acl.yaml` before an upstream request.
- A2A uses an independent transport bearer token.
- With `--remote-spec`, each server fetches the protected spec using
  `--openapi-header`; those headers never reach API operations.

The demo generates all three credentials at runtime. It creates a temporary
HOME, writes no credentials into bindings, and terminates both apexe processes
and the API server on exit. Four upstream operations succeed when both surfaces
run; the denied DELETE is never executed. Only the process-local example data
can be created, and it is discarded when the demo exits.

## Files

| File | Purpose |
|---|---|
| `openapi.json` | OpenAPI 3.1 input with path/query parameters and a required JSON body |
| `acl.yaml` | Default-deny policy allowing only `api.get_pet` and `api.create_pet` |
| `demo.py` | Local API, MCP client, authenticated A2A client and actual assertions |

## Connect your own API

Replace the source and origin with your API's values, and review a policy that
names only the operations the agent should call:

```bash
# Supply your API credential in APEXE_OPENAPI_TOKEN.
apexe serve --openapi ./openapi.json \
  --openapi-base-url https://api.example.com --acl ./acl.yaml --no-log-arguments

# A2A also needs an independent transport token, or uses the generated one.
apexe a2a --openapi ./openapi.json \
  --openapi-base-url https://api.example.com --acl ./acl.yaml --no-log-arguments
```

The [OpenAPI guide](../../docs/openapi.md) describes YAML, protected spec URLs,
namespaces, client configuration, timeout handling, and the current proxy
limits. Multipart uploads, per-input header/cookie parameters, non-JSON
responses and query parameters on body methods are outside the implemented
proxy contract. The F8 PATH shims execute CLI bindings; they do not route these
HTTP proxy modules.

## Automated checks

```bash
cargo test --all-features --test openapi_integration --test examples_smoke
```

The OpenAPI smoke tests run both server surfaces for local JSON and protected
remote JSON sources. Integration tests additionally cover protected YAML,
filtering, schema refusals, registration collisions and unsupported encodings.
