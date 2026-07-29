# Basic Example: Scan Tools, Explore via Browser

Scans `ls`, `jq` and `curl`, then starts an HTTP server with Explorer UI for browser-based testing.

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
3. Prints Claude Desktop and Cursor integration configs
4. Starts HTTP server with Explorer UI at http://127.0.0.1:8000/explorer

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
A request body works too — quotes and `&` are ordinary argument bytes, since no
shell is involved:
```json
{"url": "https://httpbin.org/post", "request": "POST",
 "data": "{\"name\":\"apexe\"}", "silent": true}
```

5. The response includes `stdout`, `stderr`, `exit_code`, `trace_id`, and `duration_ms`

> Explorer prefills **every** property in the schema as a blank template. Delete
> the ones you are not using before calling — an empty string is a value, not an
> omission, and tools that reject a blank argument (`curl --output` among them)
> will fail on it.

## Claude Desktop integration

After testing with Explorer, switch to stdio for Claude Desktop:

```bash
apexe serve --show-config claude-desktop
```

Copy the JSON into `~/Library/Application Support/Claude/claude_desktop_config.json`, restart Claude Desktop.
