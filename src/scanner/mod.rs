//! CLI Scanner Engine — Three-tier deterministic parser that scans arbitrary
//! CLI tools into structured metadata.
//!
//! ## Architecture
//!
//! - **Tier 1**: Parse `--help` output using format-specific parsers (GNU, Click, Cobra, Clap)
//! - **Tier 2**: Enrich with man page data
//! - **Tier 3**: Enrich with shell completion scripts
//!
//! The `ScanOrchestrator` coordinates all tiers, with caching and plugin support.

pub mod cache;
pub mod completion;
pub mod discovery;
pub mod man_page;
pub mod orchestrator;
pub mod parsers;
pub mod pipeline;
pub mod plugins;
pub mod protocol;
pub mod resolver;

// Re-export key types for convenient access
pub use orchestrator::ScanOrchestrator;
pub use pipeline::ParserPipeline;
pub use protocol::{CliParser, ParsedHelp};
pub use resolver::{ResolvedTool, ToolResolver};
