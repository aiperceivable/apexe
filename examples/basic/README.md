# Basic Example: Scan Tools, Explore via Browser

Scans `ls`, `jq` and `curl`, reviews the generated default-deny ACL, then
starts an HTTP server with both Explorer UI and ACL enforcement.

## Prerequisites

- `apexe` installed (`cargo install --path ../..`)
- `ls`, `jq`, and `curl` on your `$PATH`

## Run

```bash
./run.sh
```

## What it does

1. Scans `ls`, `jq`, and `curl` (extracts flags and generates JSON Schema)
2. Writes binding files to `./output/modules/`
3. Prints the generated `~/.apexe/acl.yaml` for review
4. Prints Claude Desktop and Cursor integration configs that include `--acl`
5. Starts HTTP server with Explorer UI and `--acl` at http://127.0.0.1:8000/explorer

The generated ACL allows local readonly tools and denies destructive,
open-world, and unclassified modules by default. `cli.ls` is a useful safe call
for this example, while `cli.curl` is expected to be denied until an operator
reviews the policy and adds a narrowly scoped allow rule. Inspect the generated
ACL for `cli.jq` rather than assuming a classification from its name.

## Using the Explorer UI

1. Open **http://127.0.0.1:8000/explorer** in your browser

2. Click **`cli.ls`** → type `{}` → click **Call**:
```json
{}
```
You'll see your current directory listing immediately. This is the quickest way to verify apexe works.

3. Click **`cli.ls`** again to pass a short option and an operand:
```json
{"l": true, "file": ["/etc/hosts"]}
```

4. Click **`cli.curl`**. `url` is the operand and is required:
```json
{"url": "https://httpbin.org/get", "silent": true}
```
The generated ACL should refuse this call because `curl` is an open-world
tool. This is the expected result; review `~/.apexe/acl.yaml` before deciding
whether your use case merits a narrow allow rule.

5. The response includes `stdout`, `stderr`, `exit_code`, `trace_id`, and `duration_ms`

> Explorer prefills only required fields. It uses `null` when a required field
> has no declared default, so replace `null` with a value before executing the
> call. An untouched required string is rejected by schema validation rather
> than being sent to the CLI.

## Claude Desktop integration

After testing with Explorer, switch to stdio for Claude Desktop:

```bash
apexe serve --show-config claude-desktop --acl ~/.apexe/acl.yaml
```

Copy the JSON into `~/Library/Application Support/Claude/claude_desktop_config.json`, restart Claude Desktop.
