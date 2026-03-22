use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::{load_config, ApexeConfig};

/// apexe -- Outside-In CLI-to-Agent Bridge.
///
/// Wraps CLI tools into governed apcore modules served via MCP/A2A.
#[derive(Debug, Parser)]
#[command(name = "apexe", version, about, long_about = None)]
pub struct Cli {
    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true, default_value = "info")]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Scan CLI tools and generate apcore binding files.
    Scan(ScanArgs),
    /// Start MCP/A2A server for scanned CLI tools.
    Serve(ServeArgs),
    /// List previously scanned CLI tools and their modules.
    List(ListArgs),
    /// Show or initialize apexe configuration.
    Config(ConfigArgs),
}

impl Cli {
    pub fn run(self) -> anyhow::Result<()> {
        let config = load_config(None, None)?;
        config.ensure_dirs()?;

        match self.command {
            Commands::Scan(args) => args.execute(&config),
            Commands::Serve(args) => args.execute(&config),
            Commands::List(args) => args.execute(&config),
            Commands::Config(args) => args.execute(&config),
        }
    }
}

/// Scan CLI tools and generate apcore binding files.
///
/// TOOLS: One or more CLI tool names to scan (e.g., git docker ffmpeg).
#[derive(Debug, clap::Args)]
pub struct ScanArgs {
    /// CLI tool names to scan
    #[arg(required = true)]
    pub tools: Vec<String>,

    /// Output directory for binding files (default: ~/.apexe/modules/)
    #[arg(long)]
    pub output_dir: Option<PathBuf>,

    /// Maximum subcommand recursion depth (1-5)
    #[arg(long, default_value = "2", value_parser = clap::value_parser!(u32).range(1..=5))]
    pub depth: u32,

    /// Force re-scan, bypassing cache
    #[arg(long)]
    pub no_cache: bool,

    /// Output format for scan results
    #[arg(long, default_value = "table", value_parser = ["json", "yaml", "table"])]
    pub format: String,
}

impl ScanArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let orchestrator = crate::scanner::ScanOrchestrator::new(config.clone());

        let results = orchestrator.scan(&self.tools, self.no_cache, self.depth)?;

        // Generate binding files (F3)
        let output_dir = self
            .output_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());
        let binding_generator = crate::binding::BindingGenerator::new();
        let binding_writer = crate::binding::BindingYAMLWriter;

        for tool in &results {
            // Generate and write binding YAML
            match binding_generator.generate(tool) {
                Ok(mut binding_file) => {
                    // F5: Annotate bindings with governance metadata
                    crate::governance::annotations::annotate_bindings(
                        &mut binding_file.bindings,
                    );

                    match binding_writer.write(&binding_file, &output_dir) {
                        Ok(path) => {
                            println!(
                                "Generated binding: {} ({} modules)",
                                path.display(),
                                binding_file.bindings.len()
                            );
                        }
                        Err(e) => {
                            eprintln!("Warning: Failed to write binding for {}: {e}", tool.name);
                        }
                    }

                    // F5: Generate and write ACL
                    let acl_bindings: Vec<serde_json::Map<String, serde_json::Value>> =
                        binding_file
                            .bindings
                            .iter()
                            .map(|b| {
                                let mut m = serde_json::Map::new();
                                m.insert(
                                    "module_id".to_string(),
                                    serde_json::Value::String(b.module_id.clone()),
                                );
                                m.insert(
                                    "annotations".to_string(),
                                    serde_json::json!(b.annotations),
                                );
                                m
                            })
                            .collect();
                    let acl_config =
                        crate::governance::acl::generate_acl(&acl_bindings, "deny");
                    let acl_path = config.config_dir.join("acl.yaml");
                    crate::governance::acl::write_acl(&acl_config, &acl_path);
                }
                Err(e) => {
                    eprintln!("Warning: Failed to generate binding for {}: {e}", tool.name);
                }
            }

            match self.format.as_str() {
                "json" => {
                    let json = serde_json::to_string_pretty(tool)?;
                    println!("{json}");
                }
                "yaml" => {
                    let yaml = serde_yaml::to_string(tool)?;
                    println!("{yaml}");
                }
                _ => {
                    // table format
                    println!(
                        "Tool: {} ({})",
                        tool.name,
                        tool.version.as_deref().unwrap_or("unknown")
                    );
                    println!("  Binary: {}", tool.binary_path);
                    println!("  Scan tier: {}", tool.scan_tier);
                    println!("  Subcommands: {}", tool.subcommands.len());
                    println!("  Global flags: {}", tool.global_flags.len());
                    if tool.structured_output.supported {
                        println!(
                            "  Structured output: {} ({})",
                            tool.structured_output.flag.as_deref().unwrap_or(""),
                            tool.structured_output.format.as_deref().unwrap_or("")
                        );
                    }
                    if !tool.warnings.is_empty() {
                        println!("  Warnings: {}", tool.warnings.join(", "));
                    }
                    println!();
                }
            }
        }

        Ok(())
    }
}

/// Start MCP/A2A server for scanned CLI tools.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// MCP transport type
    #[arg(long, default_value = "stdio", value_parser = ["stdio", "http", "sse"])]
    pub transport: String,

    /// Host for HTTP transports
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port for HTTP transports (1-65535)
    #[arg(long, default_value = "8000", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: u16,

    /// Enable A2A protocol alongside MCP
    #[arg(long)]
    pub a2a: bool,

    /// Enable browser-based Tool Explorer UI (HTTP only)
    #[arg(long)]
    pub explorer: bool,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,

    /// MCP server name
    #[arg(long, default_value = "apexe")]
    pub name: String,

    /// Print integration config snippet (claude-desktop, cursor)
    #[arg(long)]
    pub show_config: Option<String>,
}

impl ServeArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        // Handle --show-config
        if let Some(ref format) = self.show_config {
            let output = crate::serve::config_gen::generate_config(
                format,
                &self.name,
                &self.transport,
                &self.host,
                self.port,
            );
            println!("{output}");
            return Ok(());
        }

        let modules_dir = self
            .modules_dir
            .unwrap_or_else(|| config.modules_dir.clone());

        // Validate modules directory
        if !modules_dir.is_dir() {
            anyhow::bail!("Modules directory not found: {}", modules_dir.display());
        }

        // Load bindings
        let bindings = crate::serve::loader::load_bindings(&modules_dir)?;
        if bindings.is_empty() {
            anyhow::bail!(
                "No .binding.yaml files found in {}. Run `apexe scan` first.",
                modules_dir.display()
            );
        }

        eprintln!(
            "Loaded {} tool(s) from {}",
            bindings.len(),
            modules_dir.display()
        );

        // Build registry and handler
        let registry = crate::serve::registry::ToolRegistry::from_bindings(bindings);
        let handler = crate::serve::handler::McpHandler::new(registry, self.name.clone());

        // Serve based on transport
        match self.transport.as_str() {
            "stdio" => {
                if self.a2a {
                    eprintln!("Warning: A2A requires HTTP transport. Falling back to MCP-only.");
                }
                if self.explorer {
                    eprintln!("Warning: Explorer requires HTTP transport. Ignored.");
                }
                crate::serve::stdio::serve_stdio(&handler)
            }
            "http" | "sse" => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(crate::serve::http::serve_http(
                    handler, &self.host, self.port,
                ))
            }
            _ => anyhow::bail!("Unsupported transport: {}", self.transport),
        }
    }
}

/// List previously scanned CLI tools and their modules.
#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Output format
    #[arg(long, default_value = "table", value_parser = ["json", "table"])]
    pub format: String,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,
}

impl ListArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let modules_dir = self.modules_dir.as_ref().unwrap_or(&config.modules_dir);

        if !modules_dir.exists() {
            println!("No modules directory found at {}", modules_dir.display());
            println!("Run 'apexe scan <tool>' first to generate binding files.");
            return Ok(());
        }

        let mut bindings = Vec::new();
        for entry in std::fs::read_dir(modules_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                && path.to_string_lossy().contains(".binding.")
            {
                let contents = std::fs::read_to_string(&path)?;
                if let Ok(file) = serde_yaml::from_str::<serde_yaml::Value>(&contents) {
                    if let Some(entries) = file.get("bindings").and_then(|b| b.as_sequence()) {
                        for entry in entries {
                            let module_id = entry
                                .get("module_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let description = entry
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            bindings.push((module_id.to_string(), description.to_string()));
                        }
                    }
                }
            }
        }

        bindings.sort_by(|a, b| a.0.cmp(&b.0));

        if bindings.is_empty() {
            println!("No binding files found in {}", modules_dir.display());
            println!("Run 'apexe scan <tool>' first to generate binding files.");
            return Ok(());
        }

        match self.format.as_str() {
            "json" => {
                let json: Vec<serde_json::Value> = bindings
                    .iter()
                    .map(|(id, desc)| {
                        serde_json::json!({"module_id": id, "description": desc})
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                let header_id = "MODULE ID";
                let header_desc = "DESCRIPTION";
                println!("{header_id:<40} {header_desc}");
                println!("{:<40} {}", "─".repeat(40), "─".repeat(40));
                for (id, desc) in &bindings {
                    let desc_truncated = if desc.chars().count() > 60 {
                        let truncated: String = desc.chars().take(57).collect();
                        format!("{truncated}...")
                    } else {
                        desc.clone()
                    };
                    println!("{:<40} {}", id, desc_truncated);
                }
                println!("\n{} module(s) found.", bindings.len());
            }
        }

        Ok(())
    }
}

/// Show or initialize apexe configuration.
#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    /// Show current configuration
    #[arg(long)]
    pub show: bool,

    /// Initialize default config file
    #[arg(long)]
    pub init: bool,
}

impl ConfigArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        if self.show {
            let yaml = serde_yaml::to_string(config)?;
            println!("{yaml}");
        }
        if self.init {
            let config_path = config.config_dir.join("config.yaml");
            if !config_path.exists() {
                let default = ApexeConfig::default();
                let yaml = serde_yaml::to_string(&default)?;
                std::fs::write(&config_path, yaml)?;
                println!("Config written to {}", config_path.display());
            } else {
                println!("Config already exists at {}", config_path.display());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scan_subcommand() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        assert!(matches!(cli.command, Commands::Scan(_)));
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.tools, vec!["git".to_string()]);
        }
    }

    #[test]
    fn test_parse_no_subcommand_fails() {
        let result = Cli::try_parse_from(["apexe"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_log_level_flag() {
        let cli = Cli::try_parse_from(["apexe", "--log-level", "debug", "scan", "git"]).unwrap();
        assert_eq!(cli.log_level, "debug");
    }

    #[test]
    fn test_parse_default_log_level() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        assert_eq!(cli.log_level, "info");
    }

    // ScanArgs validation tests (F1-T9)
    #[test]
    fn test_scan_no_tools_fails() {
        let result = Cli::try_parse_from(["apexe", "scan"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_depth_zero_fails() {
        let result = Cli::try_parse_from(["apexe", "scan", "git", "--depth", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_depth_six_fails() {
        let result = Cli::try_parse_from(["apexe", "scan", "git", "--depth", "6"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_depth_three_succeeds() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git", "--depth", "3"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.depth, 3);
        }
    }

    #[test]
    fn test_scan_format_xml_fails() {
        let result = Cli::try_parse_from(["apexe", "scan", "git", "--format", "xml"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_format_json_succeeds() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git", "--format", "json"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.format, "json");
        }
    }

    #[test]
    fn test_scan_multiple_tools() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git", "docker"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.tools, vec!["git".to_string(), "docker".to_string()]);
        }
    }

    #[test]
    fn test_scan_default_depth() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.depth, 2);
        }
    }

    #[test]
    fn test_scan_default_format() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.format, "table");
        }
    }

    // ServeArgs validation tests (F1-T10)
    #[test]
    fn test_serve_defaults() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.transport, "stdio");
            assert_eq!(args.host, "127.0.0.1");
            assert_eq!(args.port, 8000);
            assert!(!args.a2a);
            assert!(!args.explorer);
        }
    }

    #[test]
    fn test_serve_invalid_transport_fails() {
        let result = Cli::try_parse_from(["apexe", "serve", "--transport", "invalid"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serve_port_zero_fails() {
        let result = Cli::try_parse_from(["apexe", "serve", "--port", "0"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serve_with_all_flags() {
        let cli = Cli::try_parse_from([
            "apexe", "serve", "--transport", "http", "--host", "0.0.0.0", "--port", "9000",
            "--a2a", "--explorer",
        ])
        .unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.transport, "http");
            assert_eq!(args.host, "0.0.0.0");
            assert_eq!(args.port, 9000);
            assert!(args.a2a);
            assert!(args.explorer);
        }
    }

    // ListArgs validation tests (F1-T10)
    #[test]
    fn test_list_default_format() {
        let cli = Cli::try_parse_from(["apexe", "list"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert_eq!(args.format, "table");
        }
    }

    #[test]
    fn test_list_format_json() {
        let cli = Cli::try_parse_from(["apexe", "list", "--format", "json"]).unwrap();
        if let Commands::List(args) = cli.command {
            assert_eq!(args.format, "json");
        }
    }

    #[test]
    fn test_list_format_xml_fails() {
        let result = Cli::try_parse_from(["apexe", "list", "--format", "xml"]);
        assert!(result.is_err());
    }

    // ConfigArgs tests
    #[test]
    fn test_config_show_flag() {
        let cli = Cli::try_parse_from(["apexe", "config", "--show"]).unwrap();
        if let Commands::Config(args) = cli.command {
            assert!(args.show);
            assert!(!args.init);
        }
    }

    #[test]
    fn test_config_init_flag() {
        let cli = Cli::try_parse_from(["apexe", "config", "--init"]).unwrap();
        if let Commands::Config(args) = cli.command {
            assert!(!args.show);
            assert!(args.init);
        }
    }

    #[test]
    fn test_config_no_flags_parses() {
        let cli = Cli::try_parse_from(["apexe", "config"]).unwrap();
        if let Commands::Config(args) = cli.command {
            assert!(!args.show);
            assert!(!args.init);
        }
    }

    // ConfigArgs execute tests (F1-T13)
    #[test]
    fn test_config_no_flags_is_noop() {
        let config = ApexeConfig::default();
        let args = ConfigArgs {
            show: false,
            init: false,
        };
        let result = args.execute(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_config_show_outputs_valid_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
        };

        // --show should serialize to valid YAML
        let yaml = serde_yaml::to_string(&config).unwrap();
        let deserialized: ApexeConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.log_level, "info");
        assert_eq!(deserialized.default_timeout, 30);
    }

    #[test]
    fn test_config_init_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
        };

        let args = ConfigArgs {
            show: false,
            init: true,
        };
        args.execute(&config).unwrap();

        let config_path = tmp.path().join("config.yaml");
        assert!(config_path.exists());

        // Verify the written file is valid YAML
        let contents = std::fs::read_to_string(&config_path).unwrap();
        let parsed: ApexeConfig = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(parsed.log_level, "info");
    }

    #[test]
    fn test_config_init_does_not_overwrite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(&config_path, "existing content").unwrap();

        let config = ApexeConfig {
            modules_dir: tmp.path().join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
        };

        let args = ConfigArgs {
            show: false,
            init: true,
        };
        args.execute(&config).unwrap();

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(contents, "existing content");
    }

    // ScanArgs execute error case test
    #[test]
    fn test_scan_execute_nonexistent_tool_errors() {
        let config = ApexeConfig::default();
        let args = ScanArgs {
            tools: vec!["nonexistent_tool_xyz_12345".to_string()],
            output_dir: None,
            depth: 2,
            no_cache: false,
            format: "table".to_string(),
        };
        let result = args.execute(&config);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found on PATH"),
            "Expected 'not found on PATH' in error, got: {err_msg}"
        );
    }
}
