//! End-to-end integration coverage for the MCP serve path.
//!
//! The prior review flagged that `serve` had only unit coverage: the request
//! path (a `tools/call` that actually executes a CLI, and injection blocking on
//! that path) was never exercised end to end. These tests drive a *real*
//! on-disk `.binding.yaml` through the exact pipeline MCP dispatches into —
//! `McpServerBuilder::build()` for the server surface, and `Executor::call`
//! (what apcore-mcp's `tools/call` handler invokes) for execution + governance.
//! The JSON-RPC transport framing itself is apcore-mcp's responsibility and is
//! tested there.

use std::path::Path;

use apcore::Executor;
use apcore_toolkit::ScannedModule;
use apexe::mcp::McpServerBuilder;
use apexe::module::{build_executor, ExecutorOptions};
use apexe::output::YamlOutput;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;

/// Write a real `cli.echo` binding (targeting `/bin/echo`) into `dir`, the same
/// artifact `apexe scan` produces and `serve` loads.
fn write_echo_binding(dir: &Path) {
    let module = ScannedModule::new(
        "cli.echo".to_string(),
        "Echo a message".to_string(),
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "additionalProperties": false
        }),
        json!({ "type": "object" }),
        vec!["cli".to_string()],
        "exec:///bin/echo".to_string(),
    );
    YamlOutput::without_verification()
        .write(&[module], dir, false)
        .expect("binding should be written");
}

fn exec_opts(dir: &Path) -> ExecutorOptions<'_> {
    ExecutorOptions {
        modules_dir: Some(dir),
        timeout_ms: 5_000,
        acl_path: None,
        audit_path: None,
        enable_logging: false,
        enable_approval: false,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
    }
}

fn build_test_executor(dir: &Path) -> Arc<Executor> {
    build_executor(&exec_opts(dir)).expect("executor should build from bindings")
}

#[test]
fn test_mcp_server_builds_and_exposes_scanned_tools() {
    // The MCP server builds from on-disk bindings and exposes the scanned
    // module as a callable tool (tools/list surface).
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    McpServerBuilder::new()
        .modules_dir(dir.path())
        .build()
        .expect("MCP server should build from bindings");

    let tools = McpServerBuilder::new()
        .modules_dir(dir.path())
        .export_openai_tools()
        .expect("tool export should succeed");
    let blob = serde_json::to_string(&tools).unwrap();
    assert!(
        blob.contains("echo"),
        "scanned echo module should be exposed as a tool: {blob}"
    );
}

#[tokio::test]
async fn test_mcp_tools_call_executes() {
    // tools/call -> Executor::call runs the full pipeline and executes the CLI.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let result = executor
        .call(
            "cli.echo",
            json!({ "message": "hello_from_mcp" }),
            None,
            None,
        )
        .await
        .expect("tools/call should execute");

    let stdout = result["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("hello_from_mcp"),
        "echo should emit the message on stdout: {result}"
    );
    assert_eq!(result["exit_code"], 0);
}

#[tokio::test]
async fn test_mcp_tools_call_injection_blocked() {
    // A shell metacharacter in a client-supplied argument must be rejected on
    // the request path (defense-in-depth: direct argv + injection filter).
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let result = executor
        .call("cli.echo", json!({ "message": "ok; rm -rf /" }), None, None)
        .await;

    assert!(
        result.is_err(),
        "shell metacharacters must be rejected, got: {result:?}"
    );
}

#[tokio::test]
async fn test_mcp_tools_call_unknown_module_errors() {
    // A tools/call for a module that was never scanned returns an error rather
    // than executing anything.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let result = executor
        .call("cli.nonexistent", json!({}), None, None)
        .await;

    assert!(result.is_err(), "unknown module should error");
}
