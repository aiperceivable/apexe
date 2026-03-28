//! apexe — Outside-In CLI-to-Agent Bridge.
//!
//! # Public Modules
//!
//! - [`cli`] — CLI entry point (clap commands)
//! - [`config`] — Configuration loading and resolution
//! - [`errors`] — Error types and `From<ApexeError> for ModuleError`
//!
//! # Library API (for programmatic use)
//!
//! - [`adapter`] — `CliToolConverter`: ScannedCLITool → ScannedModule
//! - [`scanner`] — `ScanOrchestrator`: 3-tier CLI scanner engine
//! - [`output`] — `YamlOutput` + `load_modules_from_dir`
//! - [`mcp`] — `McpServerBuilder`: build and start MCP servers
//! - [`module`] — `CliModule`: apcore Module trait for CLI execution
//! - [`governance`] — `AclManager`, `AuditManager`, `SandboxManager`
//! - [`models`] — Scanner data types (ScannedCLITool, ScannedCommand, etc.)

pub mod adapter;
pub mod cli;
pub mod config;
pub mod errors;
pub mod governance;
pub mod mcp;
pub mod models;
pub mod module;
pub mod output;
pub mod scanner;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
