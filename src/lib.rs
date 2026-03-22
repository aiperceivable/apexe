pub mod cli;
pub mod config;
pub mod errors;

// Placeholder modules for future features
pub mod models;
pub mod scanner;
pub mod binding;
pub mod executor;
pub mod serve;
pub mod governance;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
