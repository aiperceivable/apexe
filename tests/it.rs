//! Shared integration-test target: one compilation and link for these modules.
//!
//! Tests isolate child environments and files in temporary directories; they
//! must not mutate process-global environment, cwd, tracing, or path-guard state.
//! The long-running CLI runtime matrix stays a separate Cargo target so it does
//! not compete with this suite's subprocesses. Add new test files below, or
//! declare a separate target in Cargo.toml when process isolation is required.

mod a2a_integration;
mod acl_diagnostics;
mod agent_shell;
mod agent_shell_pty;
mod argv_rendering;
mod cli_integration;
mod documentation_consistency;
mod documentation_publish;
mod env_check;
mod examples_smoke;
mod initialization;
mod local_execution;
mod logging;
mod mcp_integration;
mod native_exec;
mod native_shims;
mod openapi_integration;
mod scanner_integration;
mod shell_initialization;
mod structured_cli;
mod verification;
mod verification_safety;
