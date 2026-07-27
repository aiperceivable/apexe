//! CLI Scanner Engine — Three-tier deterministic parser that scans arbitrary
//! CLI tools into structured metadata.
//!
//! ## Architecture
//!
//! - **Tier 1**: Parse `--help` output using format-specific parsers (GNU, Click, Cobra, Clap)
//! - **Tier 2**: Enrich with man page data
//! - **Tier 3**: Enrich with shell completion scripts
//! - **Tier 4**: Replace or refine the result with a curated overlay matched by
//!   detected tool *variant* (BSD vs GNU coreutils vs BusyBox)
//!
//! Tiers 1-3 are heuristics over human-readable text and are labelled as such
//! in the emitted contract; only Tier 4 produces `verified` facts.
//!
//! The `ScanOrchestrator` coordinates all tiers, with caching and plugin support.

pub mod cache;
pub mod completion;
pub mod discovery;
pub mod exec;
pub mod man_page;
pub mod orchestrator;
pub mod overlay_store;
pub mod parsers;
pub mod pipeline;
pub mod plugins;
pub mod protocol;
pub mod resolver;
pub mod variant;

// Re-export key types for convenient access
pub use orchestrator::ScanOrchestrator;
pub use overlay_store::{OverlayError, OverlayStore};
pub use pipeline::ParserPipeline;
pub use protocol::{CliParser, ParsedHelp};
pub use resolver::{ResolvedTool, ToolResolver};
pub use variant::{classify_variant, current_platform};
