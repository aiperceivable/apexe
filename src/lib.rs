pub mod cli;
pub mod config;
pub mod errors;

// Placeholder modules for future features
pub mod binding;
pub mod executor;
pub mod governance;
pub mod models;
pub mod scanner;
pub mod serve;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
