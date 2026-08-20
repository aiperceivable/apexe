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
use apexe::adapter::CliToolConverter;
use apexe::models::ScannedCLITool;
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
async fn test_a2a_agent_card_reports_the_apexe_version() {
    // #35 item 4: the card reported "0.4.1" (apcore-a2a's stale VERSION
    // constant) while running apexe 0.6.0, because `config.version` was never
    // set. A client reading the card must learn which apexe it is talking to.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let card = A2aServerBuilder::new()
        .name("apexe-test")
        .modules_dir(dir.path())
        .agent_card()
        .await
        .expect("agent card should build from bindings");

    assert_eq!(
        card.get("version").and_then(serde_json::Value::as_str),
        Some(apexe::VERSION),
        "agent card must advertise apexe's version: {card}"
    );
    assert_eq!(
        card.get("name").and_then(serde_json::Value::as_str),
        Some("apexe-test"),
        "agent card must advertise the configured agent name: {card}"
    );
}

#[tokio::test]
async fn test_a2a_agent_card_skill_id_is_the_module_id() {
    // The card's contents were previously only asserted via string-contains,
    // which cannot tell a skill id from a description. Routing keys on
    // `skill.id` alone, so pin it to the `module_id` exactly.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let card = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .agent_card()
        .await
        .expect("agent card should build from bindings");

    let skills = card
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .expect("card should carry a skills array");
    assert_eq!(skills.len(), 1, "one binding -> one skill: {card}");
    assert_eq!(
        skills[0].get("id").and_then(serde_json::Value::as_str),
        Some("cli.echo"),
        "skill id must be the module_id verbatim: {card}"
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

/// Write a second binding so a filter has something to exclude.
fn write_true_binding(dir: &Path) {
    let module = ScannedModule::new(
        "sys.true".to_string(),
        "Do nothing, successfully".to_string(),
        json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        json!({ "type": "object" }),
        vec!["sys".to_string()],
        "exec:///usr/bin/true".to_string(),
    );
    YamlOutput::without_verification()
        .write(&[module], dir, false)
        .expect("binding should be written");
}

/// `--prefix` must narrow the A2A surface, not just the card.
///
/// `apexe serve` gained registration-level filtering in #28; `apexe a2a` was
/// left with `ModuleFilter::default()` hardcoded, so the A2A surface could not
/// be narrowed at all. That gap matters more here than on MCP: `apexe a2a` has
/// no authenticator, so choosing which wrapped binaries the surface exposes is
/// the only limiting mechanism available.
#[tokio::test]
async fn test_a2a_prefix_filter_excludes_a_module_from_the_card() {
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    write_true_binding(dir.path());

    let card = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .prefix("cli.")
        .agent_card()
        .await
        .expect("agent card should build from bindings");

    let blob = serde_json::to_string(&card).unwrap();
    assert!(blob.contains("cli.echo"), "the admitted skill: {blob}");
    assert!(
        !blob.contains("sys.true"),
        "an excluded module must not be advertised: {blob}"
    );
}

#[tokio::test]
async fn test_a2a_tags_filter_excludes_a_module_from_the_card() {
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    write_true_binding(dir.path());

    let card = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .tags(vec!["sys".to_string()])
        .agent_card()
        .await
        .expect("agent card should build from bindings");

    let blob = serde_json::to_string(&card).unwrap();
    assert!(blob.contains("sys.true"), "the admitted skill: {blob}");
    assert!(
        !blob.contains("cli.echo"),
        "an excluded module must not be advertised: {blob}"
    );
}

/// The filter runs at registration, so exclusion is not merely cosmetic.
///
/// A card-only filter would leave `sys.true` callable by name — the exact
/// divergence #28 closed on the MCP side. An empty registry is the observable
/// proof that nothing was registered, since `agent_card` refuses one.
#[tokio::test]
async fn test_a2a_filter_that_admits_nothing_leaves_an_empty_registry() {
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let result = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .prefix("nomatch.")
        .agent_card()
        .await;

    assert!(
        result.is_err(),
        "a filter admitting nothing must leave no registered module"
    );
}

/// Every apexe skill must advertise `application/json` and nothing else.
///
/// §11 tells a reader to send a DataPart and to trust the skill's own
/// `inputModes` rather than the card's agent-level `defaultInputModes`, which
/// apcore-a2a hardcodes to `["text/plain", "application/json"]` and apexe
/// cannot narrow. That advice only holds while the per-skill computation stays
/// correct — it is apcore-a2a's `compute_input_modes`, derived from the schema
/// root, so an upstream change would make the manual wrong with nothing here
/// noticing.
///
/// The premise is pinned separately by
/// `test_a_scanned_tool_always_yields_an_object_rooted_schema` below — the
/// fixture here writes its own schema, so it cannot speak for what
/// `build_input_schema` produces.
#[tokio::test]
async fn test_every_skill_advertises_json_only_input() {
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let card = A2aServerBuilder::new()
        .modules_dir(dir.path())
        .agent_card()
        .await
        .expect("agent card should build from bindings");

    let skills = card["skills"].as_array().expect("the card lists skills");
    assert!(
        !skills.is_empty(),
        "the fixture registers one skill: {card}"
    );

    for skill in skills {
        let modes: Vec<&str> = skill["inputModes"]
            .as_array()
            .expect("every skill declares inputModes")
            .iter()
            .filter_map(|m| m.as_str())
            .collect();
        assert_eq!(
            modes,
            ["application/json"],
            "an apexe skill takes a JSON object, so prose in a TextPart is not a \
             mode it can honour: {skill}"
        );
    }

    // The agent-level default is upstream's and stays broad. Asserted so the
    // manual's explanation of *why* the two disagree cannot go stale silently.
    let default_modes: Vec<&str> = card["defaultInputModes"]
        .as_array()
        .expect("the card declares defaultInputModes")
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    assert!(
        default_modes.contains(&"text/plain"),
        "if upstream narrowed the agent default, §11's caveat should be deleted \
         rather than left explaining a disagreement that no longer exists: {card}"
    );
}

/// `build_input_schema` must keep producing an object-rooted schema.
///
/// This is the premise the whole `inputModes` story rests on: apcore-a2a's
/// `compute_input_modes` returns `["application/json", "text/plain"]` for a
/// string-rooted schema and `["application/json"]` for anything else, so a
/// string root is what would make §11's "prose never works" advice wrong. It is
/// asserted against the real converter rather than a hand-written fixture.
#[test]
fn test_a_scanned_tool_always_yields_an_object_rooted_schema() {
    use apexe::models::{ScannedFlag, ValueType};

    let tool = ScannedCLITool {
        name: "probe".to_string(),
        description: "A tool with exactly one string flag".to_string(),
        binary_path: "/usr/bin/true".to_string(),
        global_flags: vec![ScannedFlag {
            long_name: Some("--only".to_string()),
            description: "The single option.".to_string(),
            value_type: ValueType::String,
            ..Default::default()
        }],
        ..Default::default()
    };

    for module in CliToolConverter::new().convert(&tool) {
        assert_eq!(
            module.input_schema["type"], "object",
            "a string-rooted schema would make the skill advertise text/plain, \
             which apexe cannot honour: {}",
            module.input_schema
        );
    }
}
