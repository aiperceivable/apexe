//! CLI Scanner Engine — Three-tier deterministic parser that scans arbitrary
//! CLI tools into structured metadata.
//!
//! ## Architecture
//!
//! - **Tier 1**: Parse `--help` output using format-specific parsers (Man, BSD Usage,
//!   GNU, Click, Cobra, Clap)
//! - **Tier 2**: Enrich with man page data
//! - **Tier 3**: Enrich with shell completion scripts
//! - **Tier 4**: Replace or refine the result with a curated overlay matched by
//!   detected tool *variant* (BSD vs GNU coreutils vs BusyBox)
//!
//! Tiers 1-3 are heuristics over human-readable text and are labelled as such
//! in the emitted contract; only Tier 4 produces `verified` facts.
//!
//! The `ScanOrchestrator` coordinates all tiers, with caching and plugin support.

/// Phrases a tool uses to reject an option it does not recognise.
///
/// Shared because two tiers ask the same question of the same text and must not
/// disagree: variant detection reads a rejected `--version` as evidence of a
/// non-GNU userland, and the BSD usage parser strips these lines so an error
/// message is never mistaken for a description. They had drifted apart, which
/// is why OpenBSD failed to classify.
///
/// The wording is not uniform across BSDs — macOS says "unrecognized option",
/// OpenBSD says "unknown option" — and a missing marker fails silently: the
/// tool just comes back `unknown`.
pub const OPTION_REJECTION_MARKERS: &[&str] = &[
    "unrecognized option",
    "unrecognised option",
    "illegal option",
    "invalid option",
    "unknown option",
];

/// The highest tier a scan can report in [`ScannedCLITool::scan_tier`].
///
/// Tiers 1-3 read the binary — `--help`, the man page, the shell completion
/// script. Tier 4 is a curated overlay applied on top of what they found, so a
/// tool with no matching overlay tops out at 3.
///
/// Exported because the number is stated in four documents, and a doc that
/// describes only the tiers that read the binary reads as a complete account of
/// the pipeline while omitting the one tier a human wrote by hand.
/// `tests/documentation_consistency.rs` holds them to this constant, the way
/// [`ParserPipeline::parser_count`] already holds them to the parser inventory.
///
/// [`ScannedCLITool::scan_tier`]: crate::models::ScannedCLITool::scan_tier
pub const MAX_SCAN_TIER: u32 = 4;

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
pub mod value_placeholder;
pub mod variant;

// Re-export key types for convenient access
pub use orchestrator::{ScanFailure, ScanOrchestrator, ScanOutcome};
pub use overlay_store::{OverlayError, OverlayStore};
pub use pipeline::ParserPipeline;
pub use protocol::{CliParser, ParsedHelp};
pub use resolver::{ResolvedTool, ToolResolver};
pub use variant::{classify_variant, current_platform};
