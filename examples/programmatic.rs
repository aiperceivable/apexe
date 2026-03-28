//! Programmatic example: use apexe as a library to scan tools, build a
//! filtered MCP server, and export OpenAI-compatible tool definitions.
//!
//! Run with: cargo run --example programmatic

use apexe::adapter::CliToolConverter;
use apexe::config::ApexeConfig;
use apexe::mcp::McpServerBuilder;
use apexe::output::YamlOutput;
use apexe::scanner::ScanOrchestrator;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let config = ApexeConfig::default();
    config.ensure_dirs()?;

    // --- Step 1: Scan a CLI tool ---
    println!("=== Scanning 'echo' (a simple tool for demonstration) ===\n");
    let orchestrator = ScanOrchestrator::new(config.clone());
    let tools = orchestrator.scan(&["echo".to_string()], true, 1)?;

    for tool in &tools {
        println!(
            "Scanned: {} (tier {}, {} subcommands, {} global flags)",
            tool.name,
            tool.scan_tier,
            tool.subcommands.len(),
            tool.global_flags.len()
        );
    }

    // --- Step 2: Convert to ScannedModules ---
    println!("\n=== Converting to ScannedModules ===\n");
    let converter = CliToolConverter::new();
    let modules = converter.convert_all(&tools);

    for m in &modules {
        println!("Module: {} — {}", m.module_id, m.description);
        if let Some(ref ann) = m.annotations {
            println!(
                "  readonly={}, destructive={}, requires_approval={}",
                ann.readonly, ann.destructive, ann.requires_approval
            );
        }
        if let Some(display) = m.metadata.get("display") {
            if let Some(alias) = display.get("alias").and_then(|v| v.as_str()) {
                println!("  display alias: {alias}");
            }
        }
    }

    // --- Step 3: Write binding YAML ---
    println!("\n=== Writing binding files ===\n");
    let output_dir = config.modules_dir.clone();
    let yaml_output = YamlOutput::new();
    let results = yaml_output.write(&modules, &output_dir, false)?;
    for wr in &results {
        if let Some(ref path) = wr.path {
            println!("Written: {path}");
        }
    }

    // --- Step 4: Export OpenAI tools ---
    println!("\n=== Exporting OpenAI-compatible tool definitions ===\n");
    let openai_tools = McpServerBuilder::new()
        .modules_dir(&output_dir)
        .export_openai_tools();

    match openai_tools {
        Ok(tools) => {
            println!("{} tool(s) exported:\n", tools.len());
            let json = serde_json::to_string_pretty(&tools)?;
            // Print first 500 chars to keep output manageable
            let truncated: String = json.chars().take(500).collect();
            println!("{truncated}...");
        }
        Err(e) => eprintln!("Export failed: {e}"),
    }

    // --- Step 5: Build MCP server (don't start it — just show it works) ---
    println!("\n=== Building MCP server (not starting) ===\n");
    let server = McpServerBuilder::new()
        .name("example-server")
        .transport("stdio")
        .modules_dir(&output_dir)
        .enable_logging(true)
        .enable_approval(false)
        .build();

    match server {
        Ok(_) => println!("MCP server built successfully. Call server.serve() to start."),
        Err(e) => eprintln!("Build failed: {e}"),
    }

    println!("\nDone. See examples/programmatic/README.md for details.");
    Ok(())
}
