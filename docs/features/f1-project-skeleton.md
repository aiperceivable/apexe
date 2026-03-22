# F1: Project Skeleton & CLI Framework

| Field | Value |
|-------|-------|
| **Feature** | F1 |
| **Priority** | P0 (prerequisite for all others) |
| **Effort** | Small (~700 LOC) |
| **Dependencies** | None |

---

## 1. Overview

Bootstrap the `apexe` Rust crate with ecosystem conventions (Cargo, clippy, cargo test) and implement a clap-based CLI with `scan`, `serve`, `list`, and `config` commands as placeholders. Establish the configuration resolution system and directory structure.

---

## 2. Module: `src/lib.rs`

```rust
pub mod cli;
pub mod config;
pub mod errors;
pub mod models;
pub mod scanner;
pub mod binding;
pub mod executor;
pub mod serve;
pub mod governance;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

---

## 3. Module: `src/main.rs`

### Function: `main()`

```rust
use clap::Parser;
use tracing_subscriber::EnvFilter;

use apexe::cli::Cli;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    cli.run()
}
```

**Logic:**
1. Parse CLI arguments via `Cli::parse()` (clap derive)
2. Initialize `tracing` subscriber with configured log level
3. Dispatch to `cli.run()` which routes to the correct subcommand

**Error handling:**
- clap handles `--help` and argument validation (exit code 2 on invalid args)
- Unhandled errors propagate via `anyhow::Result` and print to stderr (exit code 1)

---

## 4. Module: `src/cli/mod.rs`

### Struct: `Cli`

```rust
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
```

### Struct: `ScanArgs`

```rust
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
        for tool in &self.tools {
            let resolved = which::which(tool).map_err(|_| {
                anyhow::anyhow!("Tool '{}' not found on PATH", tool)
            })?;
            tracing::info!(tool = %tool, path = %resolved.display(), "Scanning");
        }
        // Delegate to ScanOrchestrator (implemented in F2)
        todo!("ScanOrchestrator.scan() — implemented in F2")
    }
}
```

**Error handling:**
- Tool not on PATH: `anyhow::anyhow!("Tool '{}' not found on PATH", tool)`
- Permission denied: anyhow error with suggestion to check permissions

**Validation:**
- `tools` must be non-empty (enforced by `required = true`)
- `depth` must be 1-5 (enforced by `value_parser`)
- `output_dir` if provided must be a writable directory

### Struct: `ServeArgs`

```rust
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
}

impl ServeArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        // Delegate to serve_command (implemented in F4)
        todo!("serve_command() — implemented in F4")
    }
}
```

**Logic (Phase 1 placeholder):**
1. Resolve `modules_dir` from flag, env var, or config
2. Validate directory exists and contains `.binding.yaml` files
3. Delegate to `serve_command()` (implemented in F4)

**Error handling:**
- No binding files: `anyhow::bail!("No binding files found")`
- A2A with stdio: `tracing::warn!("A2A requires HTTP transport")`

### Struct: `ListArgs`

```rust
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
        let pattern = modules_dir.join("*.binding.yaml");
        // Glob binding files, parse module IDs, format output
        todo!("list command implementation")
    }
}
```

**Logic:**
1. Resolve `modules_dir`
2. Glob `*.binding.yaml` files
3. Parse each file to extract module IDs and descriptions
4. Format as table or JSON

### Struct: `ConfigArgs`

```rust
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
```

**Logic:**
1. `--show`: Print current resolved config (merged from file + env + defaults)
2. `--init`: Write default `~/.apexe/config.yaml` if it does not exist

---

## 5. Module: `src/config.rs`

### Struct & Loader

```rust
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Global apexe configuration.
///
/// Resolution priority: CLI flags > env vars > config file > defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApexeConfig {
    pub modules_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub audit_log: PathBuf,
    pub log_level: String,
    pub default_timeout: u64,
    pub scan_depth: u32,
    pub json_output_preference: bool,
}

impl Default for ApexeConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let apexe_dir = home.join(".apexe");
        Self {
            modules_dir: apexe_dir.join("modules"),
            cache_dir: apexe_dir.join("cache"),
            config_dir: apexe_dir.clone(),
            audit_log: apexe_dir.join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
        }
    }
}

impl ApexeConfig {
    /// Create all required directories if they do not exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.modules_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.config_dir)?;
        Ok(())
    }
}

/// Load configuration with three-tier resolution.
///
/// 1. Start with defaults
/// 2. If config file exists, parse YAML and override matching fields
/// 3. Check env vars (APEXE_MODULES_DIR, APEXE_CACHE_DIR, APEXE_LOG_LEVEL,
///    APEXE_TIMEOUT) and override matching fields
/// 4. Apply cli_overrides
/// 5. Return ApexeConfig
pub fn load_config(
    config_path: Option<&Path>,
    cli_overrides: Option<&std::collections::HashMap<String, String>>,
) -> anyhow::Result<ApexeConfig> {
    let mut config = ApexeConfig::default();

    // Load from config file
    let file_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("config.yaml"));

    if file_path.exists() {
        let contents = std::fs::read_to_string(&file_path)?;
        match serde_yaml::from_str::<ApexeConfig>(&contents) {
            Ok(file_config) => config = file_config,
            Err(e) => warn!(path = %file_path.display(), "Malformed config file, using defaults: {e}"),
        }
    }

    // Override from env vars
    if let Ok(val) = std::env::var("APEXE_MODULES_DIR") {
        config.modules_dir = PathBuf::from(val);
    }
    if let Ok(val) = std::env::var("APEXE_CACHE_DIR") {
        config.cache_dir = PathBuf::from(val);
    }
    if let Ok(val) = std::env::var("APEXE_LOG_LEVEL") {
        config.log_level = val;
    }
    if let Ok(val) = std::env::var("APEXE_TIMEOUT") {
        match val.parse::<u64>() {
            Ok(t) => config.default_timeout = t,
            Err(_) => warn!("Invalid APEXE_TIMEOUT value: {val}, using default"),
        }
    }

    // Apply CLI overrides
    if let Some(overrides) = cli_overrides {
        if let Some(val) = overrides.get("modules_dir") {
            config.modules_dir = PathBuf::from(val);
        }
        if let Some(val) = overrides.get("log_level") {
            config.log_level = val.clone();
        }
        if let Some(val) = overrides.get("scan_depth") {
            if let Ok(d) = val.parse::<u32>() {
                config.scan_depth = d;
            }
        }
    }

    Ok(config)
}
```

**Field Mappings:**

| Config File Key | Env Var | CLI Flag | ApexeConfig Field |
|----------------|---------|----------|-------------------|
| `modules_dir` | `APEXE_MODULES_DIR` | `--output-dir` / `--modules-dir` | `modules_dir` |
| `cache_dir` | `APEXE_CACHE_DIR` | (none) | `cache_dir` |
| `log_level` | `APEXE_LOG_LEVEL` | `--log-level` | `log_level` |
| `default_timeout` | `APEXE_TIMEOUT` | (none) | `default_timeout` |
| `scan_depth` | (none) | `--depth` | `scan_depth` |

**Error handling:**
- Config file YAML parse error: log warning, continue with defaults
- Invalid env var value (e.g., `APEXE_TIMEOUT=abc`): log warning, use default
- Config file does not exist: use defaults (no error)

---

## 6. Module: `src/errors.rs`

```rust
use thiserror::Error;

/// Top-level error type for all apexe operations.
#[derive(Debug, Error)]
pub enum ApexeError {
    /// CLI tool binary not found on PATH.
    #[error("Tool '{tool_name}' not found on PATH")]
    ToolNotFound { tool_name: String },

    /// Scanning error (general).
    #[error("Scan error: {0}")]
    ScanError(String),

    /// Subprocess timed out during scanning.
    #[error("Command '{command}' timed out after {timeout}s")]
    ScanTimeout { command: String, timeout: u64 },

    /// Subprocess execution denied.
    #[error("Permission denied executing '{command}'")]
    ScanPermission { command: String },

    /// Input contains shell injection characters.
    #[error("Parameter '{param_name}' contains prohibited characters: {chars:?}")]
    CommandInjection {
        param_name: String,
        chars: Vec<char>,
    },

    /// Help text parsing failed.
    #[error("Parse error: {0}")]
    ParseError(String),

    /// IO error wrapper.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// YAML serialization error.
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    /// JSON serialization error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

---

## 7. `Cargo.toml`

```toml
[package]
name = "apexe"
version = "0.1.0"
edition = "2021"
description = "Outside-In CLI-to-Agent Bridge"
license = "Apache-2.0"
readme = "README.md"

[dependencies]
anyhow = "1"
chrono = { version = "0.4", features = ["serde"] }
clap = { version = "4", features = ["derive"] }
dirs = "5"
nom = "7"
regex = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
sha2 = "0.10"
shell-words = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4", "serde"] }
which = "7"

# apcore ecosystem (when published)
# apcore = "0.13"
# apcore-mcp = "0.1"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
rstest = "0.23"
tempfile = "3"
tokio-test = "0.4"

[[bin]]
name = "apexe"
path = "src/main.rs"
```

---

## 8. Test Scenarios

| Test ID | Scenario | Expected |
|---------|----------|----------|
| F1-T01 | `cargo install --path .` | Installation succeeds, `apexe` command available |
| F1-T02 | `apexe --help` | Shows group help with scan, serve, list, config commands |
| F1-T03 | `apexe --version` | Shows `apexe 0.1.0` |
| F1-T04 | `apexe scan --help` | Shows TOOLS argument, --output-dir, --depth, --no-cache, --format |
| F1-T05 | `apexe serve --help` | Shows --transport, --host, --port, --a2a, --explorer |
| F1-T06 | `apexe scan` (no args) | Exit code 2, "required arguments were not provided" |
| F1-T07 | `apexe scan nonexistent_tool` | Exit code 1, "Tool 'nonexistent_tool' not found on PATH" |
| F1-T08 | Config file loading | YAML parsed correctly, fields mapped to ApexeConfig |
| F1-T09 | Env var override | `APEXE_MODULES_DIR=/tmp/m` overrides config file value |
| F1-T10 | CLI flag override | `--log-level debug` overrides env var and config |
| F1-T11 | Directory creation | `ensure_dirs()` creates `~/.apexe/{modules,cache}` |
| F1-T12 | `apexe config --show` | Prints resolved config as YAML |
| F1-T13 | `apexe config --init` | Creates `~/.apexe/config.yaml` with defaults |
| F1-T14 | Malformed config.yaml | Warning logged, defaults used |

### Example Test (assert_cmd)

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_help_shows_subcommands() {
    Command::cargo_bin("apexe")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("config"));
}

#[test]
fn test_scan_requires_tool_argument() {
    Command::cargo_bin("apexe")
        .unwrap()
        .arg("scan")
        .assert()
        .failure()
        .code(2);
}
```

### Example Test (rstest for config)

```rust
use rstest::rstest;
use tempfile::TempDir;

#[rstest]
#[case("APEXE_MODULES_DIR", "/tmp/custom_modules")]
#[case("APEXE_LOG_LEVEL", "debug")]
fn test_env_var_override(#[case] var: &str, #[case] value: &str) {
    std::env::set_var(var, value);
    let config = load_config(None, None).unwrap();

    match var {
        "APEXE_MODULES_DIR" => assert_eq!(config.modules_dir.to_str().unwrap(), value),
        "APEXE_LOG_LEVEL" => assert_eq!(config.log_level, value),
        _ => unreachable!(),
    }

    std::env::remove_var(var);
}
```
