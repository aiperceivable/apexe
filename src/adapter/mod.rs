pub mod annotations;
pub mod contract;
pub mod converter;
pub mod overlay;
pub mod schema;

pub use contract::{CommandContract, Risk, ScanReport};
pub use converter::CliToolConverter;
pub use overlay::{apply_overlay, MatchContext, MatchStrength, Platform, ToolOverlay};
