//! apexe — Governed execution for CLI tools and HTTP APIs.
//!
//! Scan CLI tools into typed apcore modules, execute them locally, and expose
//! CLI and imported OpenAPI operations through MCP or A2A with configurable
//! access controls, approval and auditing.
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
//! - [`scanner`] — `ScanOrchestrator`: deterministic scanning and matching overlays
//! - [`output`] — YAML bindings, binding loading and Claude Skill generation
//! - [`mcp`] — `McpServerBuilder`: build and start MCP servers
//! - [`a2a`] — `A2aServerBuilder`: build and start A2A agent servers
//! - [`module`] — `CliModule` and shared executor construction
//! - [`governance`] — `AclManager`, `AuditManager`, `PathGuard`
//! - [`auth`] — Token/JWT authentication for MCP HTTP/SSE and A2A
//! - [`openapi`] — OpenAPI source options for HTTP operation import
//! - [`verification`] — CLI source drift reports and explicit case evidence
//! - [`models`] — Scanner data types (ScannedCLITool, ScannedCommand, etc.)

pub mod a2a;
pub mod adapter;
pub mod auth;
pub mod cli;
pub mod config;
mod corpus;
pub mod errors;
pub mod governance;
pub mod mcp;
pub mod models;
pub mod module;
pub mod openapi;
pub mod output;
mod runtime_path;
pub mod scanner;
pub mod verification;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
