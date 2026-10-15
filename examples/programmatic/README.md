# Programmatic Example: Custom MCP Server and Tool Export

This example shows how to use apexe as a Rust library to:

1. Scan CLI tools programmatically
2. Convert scan results to apcore ScannedModules
3. Build an MCP server from the generated bindings
4. Export OpenAI-compatible tool definitions

## Run

```bash
cargo run --example programmatic
```

## Key APIs demonstrated

- `ScanOrchestrator::new(config).scan(tools, no_cache, depth)`
- `CliToolConverter::new().convert_all(tools)`
- `YamlOutput::new().write(modules, dir, dry_run)`
- `McpServerBuilder::new().bindings_dir(...).build()`
- `McpServerBuilder::new().export_openai_tools()`
