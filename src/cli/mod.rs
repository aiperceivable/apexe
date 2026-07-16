mod config_gen;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::info;

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
    /// Start MCP server for scanned CLI tools.
    Serve(ServeArgs),
    /// Start A2A agent server for scanned CLI tools.
    A2a(A2aArgs),
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
            Commands::A2a(args) => args.execute(&config),
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

    /// Also write a Claude Skill (`SKILL.md`) per module under
    /// `<DIR>/.claude/skills/<module_id>/SKILL.md`
    #[arg(long)]
    pub skills_dir: Option<PathBuf>,
}

impl ScanArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let orchestrator = crate::scanner::ScanOrchestrator::new(config.clone());
        let results = orchestrator.scan(&self.tools, self.no_cache, self.depth)?;

        let output_dir = self
            .output_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());

        let converter = crate::adapter::CliToolConverter::new();
        let modules = converter.convert_all(&results);

        // These are the command's deliverables. A failed write must surface as
        // a non-zero exit, not a swallowed warning — otherwise `apexe scan`
        // reports success with no bindings (or, worse, no ACL policy) written.
        self.write_bindings(&modules, &output_dir)?;
        self.write_acl(&modules, config)?;
        self.write_skills(&modules)?;
        self.print_results(&results)?;

        Ok(())
    }

    fn write_bindings(
        &self,
        modules: &[apcore_toolkit::ScannedModule],
        output_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let yaml_output = crate::output::YamlOutput::new();
        let write_results = yaml_output
            .write(modules, output_dir, false)
            .map_err(|e| anyhow::anyhow!("Failed to write binding files: {e}"))?;
        for wr in &write_results {
            if let Some(ref path) = wr.path {
                info!(path, "Generated binding");
            }
        }
        Ok(())
    }

    fn write_acl(
        &self,
        modules: &[apcore_toolkit::ScannedModule],
        config: &ApexeConfig,
    ) -> anyhow::Result<()> {
        let acl_manager = crate::governance::AclManager::generate_default(modules);
        let acl_path = config.config_dir.join("acl.yaml");
        acl_manager
            .write_config(&acl_path)
            .map_err(|e| anyhow::anyhow!("Failed to write ACL: {e}"))?;
        Ok(())
    }

    fn write_skills(&self, modules: &[apcore_toolkit::ScannedModule]) -> anyhow::Result<()> {
        let Some(ref skills_dir) = self.skills_dir else {
            return Ok(());
        };
        let paths = crate::output::SkillOutput::new()
            .write(modules, skills_dir)
            .map_err(|e| anyhow::anyhow!("Failed to write skill files: {e}"))?;
        for path in &paths {
            info!(path = %path.display(), "Generated skill");
        }
        Ok(())
    }

    fn print_results(&self, results: &[crate::models::ScannedCLITool]) -> anyhow::Result<()> {
        for tool in results {
            match self.format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(tool)?),
                "yaml" => println!("{}", serde_yaml::to_string(tool)?),
                _ => Self::print_tool_table(tool),
            }
        }
        Ok(())
    }

    fn print_tool_table(tool: &crate::models::ScannedCLITool) {
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

/// Start MCP server for scanned CLI tools.
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

    /// Filter exposed tools by tags (comma-separated, AND logic)
    #[arg(long)]
    pub tags: Option<String>,

    /// Filter exposed tools by module ID prefix
    #[arg(long)]
    pub prefix: Option<String>,

    /// Path to ACL policy YAML file
    #[arg(long)]
    pub acl: Option<PathBuf>,

    /// Enable approval handler for destructive commands
    #[arg(long)]
    pub enable_approval: bool,

    /// Disable structured logging middleware
    #[arg(long)]
    pub no_logging: bool,

    /// Disable circuit breaker (short-circuits a hanging/broken tool)
    #[arg(long)]
    pub no_circuit_breaker: bool,

    /// Disable retry (only ever retries idempotent commands after a timeout)
    #[arg(long)]
    pub no_retry: bool,

    /// Enable /metrics (Prometheus) and /usage observability endpoints
    /// (HTTP/SSE transports only)
    #[arg(long)]
    pub metrics: bool,

    /// Skip input validation against tool schemas
    #[arg(long)]
    pub skip_validation: bool,
}

impl ServeArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        if let Some(ref format) = self.show_config {
            println!(
                "{}",
                config_gen::generate_config(
                    format,
                    &self.name,
                    &self.transport,
                    &self.host,
                    self.port,
                )
            );
            return Ok(());
        }

        let server = self.build_server(config)?;
        let opts = self.serve_options();
        server
            .serve_with_options(opts)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn build_server(&self, config: &ApexeConfig) -> anyhow::Result<apcore_mcp::APCoreMCP> {
        let modules_dir = self
            .modules_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());
        let mut builder = crate::mcp::McpServerBuilder::new()
            .name(&self.name)
            .transport(&self.transport)
            .host(&self.host)
            .port(self.port)
            .explorer(self.explorer)
            .modules_dir(modules_dir)
            .timeout_ms(config.default_timeout * 1000)
            .enable_logging(!self.no_logging)
            .enable_approval(self.enable_approval)
            .enable_circuit_breaker(!self.no_circuit_breaker)
            .enable_retry(!self.no_retry)
            .enable_metrics(self.metrics)
            .validate_inputs(!self.skip_validation);

        // Only load ACL when explicitly specified via --acl flag.
        // Without --acl, the server runs without access control (all tools allowed).
        if let Some(ref acl_path) = self.acl {
            builder = builder.acl_path(acl_path);
        }

        if let Some(ref tags_str) = self.tags {
            let tags: Vec<String> = tags_str.split(',').map(|s| s.trim().to_string()).collect();
            builder = builder.tags(tags);
        }
        if let Some(ref prefix) = self.prefix {
            builder = builder.prefix(prefix);
        }

        builder.build().map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn serve_options(&self) -> apcore_mcp::ServeOptions {
        apcore_mcp::ServeOptions {
            explorer: apcore_mcp::ExplorerOptions {
                explorer: self.explorer,
                allow_execute: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Start A2A agent server for scanned CLI tools.
#[derive(Debug, clap::Args)]
pub struct A2aArgs {
    /// Base URL to bind the A2A server to
    #[arg(long, default_value = "http://127.0.0.1:8000")]
    pub url: String,

    /// Directory containing binding files
    #[arg(long)]
    pub modules_dir: Option<PathBuf>,

    /// A2A agent name
    #[arg(long, default_value = "apexe")]
    pub name: String,

    /// Enable browser-based Explorer UI
    #[arg(long)]
    pub explorer: bool,

    /// Path to ACL policy YAML file
    #[arg(long)]
    pub acl: Option<PathBuf>,

    /// Enable approval handler for destructive commands
    #[arg(long)]
    pub enable_approval: bool,

    /// Disable structured logging middleware
    #[arg(long)]
    pub no_logging: bool,

    /// Disable circuit breaker (short-circuits a hanging/broken tool)
    #[arg(long)]
    pub no_circuit_breaker: bool,

    /// Disable retry (only ever retries idempotent commands after a timeout)
    #[arg(long)]
    pub no_retry: bool,

    /// Per-task execution timeout in seconds
    #[arg(long, default_value = "300")]
    pub execution_timeout: u64,

    /// Allowed CORS origin (repeatable)
    #[arg(long)]
    pub cors_origin: Vec<String>,
}

impl A2aArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let server = self.build_server(config);
        let runtime = tokio::runtime::Runtime::new()?;
        runtime
            .block_on(server.serve())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn build_server(&self, config: &ApexeConfig) -> crate::a2a::A2aServerBuilder {
        let modules_dir = self
            .modules_dir
            .clone()
            .unwrap_or_else(|| config.modules_dir.clone());
        let mut builder = crate::a2a::A2aServerBuilder::new()
            .name(&self.name)
            .url(&self.url)
            .explorer(self.explorer)
            .modules_dir(modules_dir)
            .timeout_ms(config.default_timeout * 1000)
            .enable_logging(!self.no_logging)
            .enable_approval(self.enable_approval)
            .enable_circuit_breaker(!self.no_circuit_breaker)
            .enable_retry(!self.no_retry)
            .execution_timeout(self.execution_timeout)
            .cors_origins(self.cors_origin.clone());

        // Only load ACL when explicitly specified via --acl flag.
        // Without --acl, the server runs without access control (all tools allowed).
        if let Some(ref acl_path) = self.acl {
            builder = builder.acl_path(acl_path);
        }

        builder
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

        let modules = self.load_modules(modules_dir)?;
        if modules.is_empty() {
            println!("No modules found. Run 'apexe scan <tool>' first.");
            return Ok(());
        }

        self.print_modules(&modules)?;
        Ok(())
    }

    fn load_modules(
        &self,
        dir: &std::path::Path,
    ) -> anyhow::Result<Vec<apcore_toolkit::ScannedModule>> {
        // An absent directory legitimately means "no modules yet". A load
        // FAILURE (e.g. a corrupt .binding.yaml) must surface, not be masked as
        // an empty list that misleadingly prints "No modules found".
        if !dir.exists() {
            return Ok(vec![]);
        }
        crate::output::load_modules_from_dir(dir).map_err(|e| anyhow::anyhow!(e))
    }

    fn print_modules(&self, modules: &[apcore_toolkit::ScannedModule]) -> anyhow::Result<()> {
        let mut sorted: Vec<_> = modules
            .iter()
            .map(|m| (m.module_id.as_str(), m.description.as_str()))
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));

        match self.format.as_str() {
            "json" => {
                let json: Vec<serde_json::Value> = sorted
                    .iter()
                    .map(|(id, desc)| serde_json::json!({"module_id": id, "description": desc}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            _ => {
                println!("{:<40} DESCRIPTION", "MODULE ID");
                println!("{:<40} {}", "\u{2500}".repeat(40), "\u{2500}".repeat(40));
                for (id, desc) in &sorted {
                    let truncated = if desc.chars().count() > 60 {
                        format!("{}...", desc.chars().take(57).collect::<String>())
                    } else {
                        desc.to_string()
                    };
                    println!("{:<40} {}", id, truncated);
                }
                println!("\n{} module(s) found.", sorted.len());
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

    // ScanArgs validation tests
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

    #[test]
    fn test_scan_skills_dir_default_none() {
        let cli = Cli::try_parse_from(["apexe", "scan", "git"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert!(args.skills_dir.is_none());
        }
    }

    #[test]
    fn test_scan_skills_dir_flag() {
        let cli =
            Cli::try_parse_from(["apexe", "scan", "git", "--skills-dir", "/tmp/skills"]).unwrap();
        if let Commands::Scan(args) = cli.command {
            assert_eq!(args.skills_dir, Some(PathBuf::from("/tmp/skills")));
        }
    }

    // ServeArgs validation tests
    #[test]
    fn test_serve_defaults() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.transport, "stdio");
            assert_eq!(args.host, "127.0.0.1");
            assert_eq!(args.port, 8000);
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
            "apexe",
            "serve",
            "--transport",
            "http",
            "--host",
            "0.0.0.0",
            "--port",
            "9000",
            "--explorer",
        ])
        .unwrap();
        if let Commands::Serve(args) = cli.command {
            assert_eq!(args.transport, "http");
            assert_eq!(args.host, "0.0.0.0");
            assert_eq!(args.port, 9000);
            assert!(args.explorer);
        }
    }

    #[test]
    fn test_serve_resilience_flags_default_enabled() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(!args.no_circuit_breaker);
            assert!(!args.no_retry);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_metrics_default_disabled() {
        let cli = Cli::try_parse_from(["apexe", "serve"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(!args.metrics);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_metrics_flag() {
        let cli = Cli::try_parse_from(["apexe", "serve", "--metrics"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(args.metrics);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    #[test]
    fn test_serve_resilience_flags_can_be_disabled() {
        let cli =
            Cli::try_parse_from(["apexe", "serve", "--no-circuit-breaker", "--no-retry"]).unwrap();
        if let Commands::Serve(args) = cli.command {
            assert!(args.no_circuit_breaker);
            assert!(args.no_retry);
        } else {
            panic!("expected Commands::Serve");
        }
    }

    // A2aArgs validation tests
    #[test]
    fn test_a2a_defaults() {
        let cli = Cli::try_parse_from(["apexe", "a2a"]).unwrap();
        if let Commands::A2a(args) = cli.command {
            assert_eq!(args.url, "http://127.0.0.1:8000");
            assert_eq!(args.execution_timeout, 300);
            assert!(!args.explorer);
            assert!(args.cors_origin.is_empty());
        } else {
            panic!("expected Commands::A2a");
        }
    }

    #[test]
    fn test_a2a_with_flags() {
        let cli = Cli::try_parse_from([
            "apexe",
            "a2a",
            "--url",
            "http://0.0.0.0:9090",
            "--explorer",
            "--execution-timeout",
            "600",
            "--cors-origin",
            "https://example.com",
            "--cors-origin",
            "https://foo.example.com",
        ])
        .unwrap();
        if let Commands::A2a(args) = cli.command {
            assert_eq!(args.url, "http://0.0.0.0:9090");
            assert!(args.explorer);
            assert_eq!(args.execution_timeout, 600);
            assert_eq!(
                args.cors_origin,
                vec!["https://example.com", "https://foo.example.com"]
            );
        } else {
            panic!("expected Commands::A2a");
        }
    }

    #[test]
    fn test_a2a_resilience_flags_can_be_disabled() {
        let cli =
            Cli::try_parse_from(["apexe", "a2a", "--no-circuit-breaker", "--no-retry"]).unwrap();
        if let Commands::A2a(args) = cli.command {
            assert!(args.no_circuit_breaker);
            assert!(args.no_retry);
        } else {
            panic!("expected Commands::A2a");
        }
    }

    // ListArgs validation tests
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

    // ConfigArgs execute tests
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
            ..ApexeConfig::default()
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
            ..ApexeConfig::default()
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
            ..ApexeConfig::default()
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
            skills_dir: None,
        };
        let result = args.execute(&config);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found on PATH"),
            "Expected 'not found on PATH' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_scan_write_bindings_surfaces_write_failure() {
        // A binding-write failure must return Err (non-zero exit), not warn-and-continue.
        let tmp = tempfile::TempDir::new().unwrap();
        // Use an output path whose parent component is a file, so create_dir_all fails.
        let file_path = tmp.path().join("iamafile");
        std::fs::write(&file_path, "x").unwrap();
        let bad_output = file_path.join("nested");

        let args = ScanArgs {
            tools: vec!["echo".to_string()],
            output_dir: None,
            depth: 2,
            no_cache: false,
            format: "table".to_string(),
            skills_dir: None,
        };
        let modules = vec![apcore_toolkit::ScannedModule::new(
            "cli.echo".to_string(),
            "Echo".to_string(),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/echo".to_string(),
        )];

        let result = args.write_bindings(&modules, &bad_output);
        assert!(
            result.is_err(),
            "write_bindings must surface a write failure, not swallow it"
        );
    }

    #[test]
    fn test_list_load_modules_surfaces_corrupt_binding() {
        // A corrupt binding file must surface as an error, not a misleading empty list.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("bad.binding.yaml"),
            "not: valid: binding: [[[",
        )
        .unwrap();

        let args = ListArgs {
            format: "table".to_string(),
            modules_dir: Some(tmp.path().to_path_buf()),
        };
        let result = args.load_modules(tmp.path());
        assert!(
            result.is_err(),
            "corrupt binding must surface, not collapse to an empty module list"
        );
    }
}
