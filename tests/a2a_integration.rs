//! End-to-end integration coverage for the A2A serve path.
//!
//! Like the MCP path, A2A dispatches into the same governed `Executor`
//! (`build_executor` + `Executor::call`), so execution/injection/ACL behavior
//! is covered by the MCP integration + unit tests. These tests exercise the
//! A2A-*specific* serve assembly in-process (no flaky port binding), using
//! `A2aServerBuilder::agent_card` — which runs the exact `serve` assembly and
//! returns the agent card apcore-a2a would expose at
//! `/.well-known/agent-card.json`.

use std::path::Path;

use apcore_toolkit::ScannedModule;
use apexe::a2a::A2aServerBuilder;
use apexe::output::YamlOutput;
use serde_json::json;
use tempfile::TempDir;

/// Write a real `cli.echo` binding into `dir`, as `apexe scan` would.
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

#[tokio::test]
async fn test_a2a_agent_card_exposes_scanned_skill() {
    // The A2A serve assembly builds an agent card from on-disk bindings and
    // exposes the scanned module as a skill (the A2A analog of tools/list).
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let card = A2aServerBuilder::new()
        .name("apexe-test")
        .modules_dir(dir.path())
        .agent_card()
        .await
        .expect("agent card should build from bindings");

    let blob = serde_json::to_string(&card).unwrap();
    assert!(
        blob.contains("skills"),
        "agent card should carry a skills list: {blob}"
    );
    assert!(
        blob.contains("echo"),
        "scanned echo module should be exposed as a skill: {blob}"
    );
}

#[tokio::test]
async fn test_a2a_empty_registry_errors() {
    // No bindings -> the A2A app cannot be assembled (empty registry) and the
    // serve assembly surfaces an error rather than standing up an empty agent.
    let dir = TempDir::new().unwrap();

    let result = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .agent_card()
        .await;

    assert!(
        result.is_err(),
        "an empty registry must error, not build an empty agent"
    );
}

#[tokio::test]
async fn test_a2a_serve_fails_fast_on_enable_approval_without_store() {
    // A2A has no elicitation transport: --enable-approval without an approval
    // store must fail fast, before any port is bound. (End-to-end assertion of
    // the precondition shared by serve() and agent_card().)
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let result = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .enable_approval(true)
        .agent_card()
        .await;

    assert!(
        result.is_err(),
        "enable_approval without a store must fail fast"
    );
}
