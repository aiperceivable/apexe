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
use apexe::adapter::CliToolConverter;
use apexe::mcp::McpServerBuilder;
use apexe::models::ScannedCLITool;
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
        filter: apexe::module::ModuleFilter::default(),
        audit_path: None,
        enable_logging: false,
        log_arguments: false,
        enable_approval: false,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
    }
}

fn build_test_executor(dir: &Path) -> Arc<Executor> {
    build_executor(&exec_opts(dir)).expect("executor should build from bindings")
}

/// Write a binding the way `apexe scan` actually produces one — through
/// `CliToolConverter`, which runs the `DisplayResolver` that computes
/// `metadata["display"]` (including the MCP alias).
///
/// `write_echo_binding` above builds a `ScannedModule` directly and therefore
/// carries no display overlay at all, which is precisely why the earlier
/// coverage could not see the alias/module_id mismatch: with no alias present,
/// apcore-mcp always fell back to the `module_id` in tests while real scans
/// shipped an alias that broke every call.
fn write_converted_echo_binding(dir: &Path) -> String {
    let tool = ScannedCLITool {
        name: "echo".to_string(),
        description: "Echo a message".to_string(),
        binary_path: "/bin/echo".to_string(),
        ..Default::default()
    };
    let modules = CliToolConverter::new().convert(&tool);
    let module_id = modules[0].module_id.clone();
    YamlOutput::without_verification()
        .write(&modules, dir, false)
        .expect("binding should be written");
    module_id
}

/// Resolve the MCP tool name the way apcore-mcp's `build_tools` does:
/// `descriptor.display`, falling back to `metadata["display"]`, then
/// `.mcp.alias`; absent an alias the `module_id` is used verbatim.
fn upstream_tool_name(executor: &Executor, module_id: &str) -> String {
    let descriptor = executor
        .registry()
        .get_definition(module_id)
        .expect("registry lookup should succeed")
        .expect("module should be registered");

    descriptor
        .display
        .as_ref()
        .or_else(|| descriptor.metadata.get("display"))
        .and_then(|v| v.get("mcp"))
        .and_then(|d| d.get("alias"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| descriptor.module_id.clone())
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
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    // The OpenAI exporter normalizes dots to hyphens (`ModuleIDNormalizer`),
    // so assert the exact normalized id rather than a substring: `contains
    // ("echo")` was true for `cli-echo`, `cli.echo` and a bare `echo` alike,
    // making the original assertion incapable of failing.
    assert_eq!(
        names,
        vec!["cli-echo"],
        "scanned echo module should be exported under its normalized module id"
    );
}

#[tokio::test]
async fn test_mcp_advertised_tool_name_is_callable() {
    // Regression: apcore-mcp advertises `display.mcp.alias` as the MCP tool
    // name, but routes an incoming tools/call by looking the received name up
    // as a `module_id` — there is no alias-to-id reverse map on that path. The
    // DisplayResolver sanitizes `.` to `_`, so a dotted module_id guaranteed
    // alias != module_id and *every* advertised tool failed with
    // ModuleNotFound. The name tools/list hands out must be the name
    // tools/call accepts.
    let dir = TempDir::new().unwrap();
    let module_id = write_converted_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let advertised = upstream_tool_name(&executor, &module_id);
    assert_eq!(
        advertised, module_id,
        "the advertised MCP tool name must equal the module_id apcore-mcp \
         routes on, otherwise the tool lists but cannot be called"
    );

    // And the advertised name really does dispatch.
    let result = executor
        .call(&advertised, json!({}), None, None)
        .await
        .expect("the advertised tool name should be callable");
    assert_eq!(result["exit_code"], 0);
}

#[test]
fn test_mcp_display_overlay_keeps_description_after_alias_strip() {
    // Only the *name* is load-bearing for routing, so the rest of the display
    // overlay (description/guidance, and the A2A side entirely) must survive.
    let dir = TempDir::new().unwrap();
    let module_id = write_converted_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let descriptor = executor
        .registry()
        .get_definition(&module_id)
        .unwrap()
        .expect("module should be registered");
    let display = descriptor
        .display
        .as_ref()
        .or_else(|| descriptor.metadata.get("display"))
        .expect("display overlay should still be present");

    assert!(
        display["mcp"]["alias"].is_null(),
        "the MCP alias must be stripped: {display}"
    );
    assert!(
        display["a2a"]["alias"].is_string(),
        "the A2A alias keeps id and display name in separate fields and must \
         be left alone: {display}"
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

/// Write a binding for a tool that has subcommands, produced the way `apexe
/// scan` produces one. `/bin/echo` stands in for the wrapped binary so the
/// argv the module builds is visible on stdout: whatever `echo` prints is
/// exactly what was passed to it.
fn write_converted_subcommand_binding(dir: &Path, subcommand: &str) -> String {
    let tool = ScannedCLITool {
        name: "echo".to_string(),
        description: "Echo a message".to_string(),
        binary_path: "/bin/echo".to_string(),
        subcommands: vec![apexe::models::ScannedCommand {
            name: subcommand.to_string(),
            full_command: format!("echo {subcommand}"),
            description: format!("Run {subcommand}"),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: apexe::models::HelpFormat::Gnu,
            structured_output: apexe::models::StructuredOutputInfo::default(),
            end_of_options: false,
            raw_help: String::new(),
        }],
        ..Default::default()
    };
    let modules = CliToolConverter::new().convert(&tool);
    let module_id = modules[0].module_id.clone();
    YamlOutput::without_verification()
        .write(&modules, dir, false)
        .expect("binding should be written");
    module_id
}

#[tokio::test]
async fn test_subcommand_module_runs_the_subcommand_not_the_tool_name_twice() {
    // Regression (#17): the target was built from `full_command`, which already
    // carries the tool name, so `cli.git.log` ran `git git log`. A rendered
    // target is not a running target — assert the argv the child actually
    // received, not the string that produced it.
    let dir = TempDir::new().unwrap();
    let module_id = write_converted_subcommand_binding(dir.path(), "hello");
    assert_eq!(module_id, "cli.echo.hello");

    let executor = build_test_executor(dir.path());
    let result = executor
        .call(&module_id, json!({}), None, None)
        .await
        .expect("the subcommand module should execute");

    assert_eq!(
        result["stdout"].as_str().unwrap_or_default().trim(),
        "hello",
        "argv must be `<binary> <subcommand>`, with the tool name spelled once: {result}"
    );
    assert_eq!(result["exit_code"], 0);
}

#[tokio::test]
async fn test_hyphenated_subcommand_registers_and_runs() {
    // Regression (#19): `git cat-file` generated the id `cli.git.cat-file`,
    // which apcore rejects at registration. The scan reported success and
    // `apexe list` counted it, but the module never reached a client — 63 of
    // git's 142 subcommands vanished this way. The id is now folded, and the
    // hyphen still has to reach argv.
    let dir = TempDir::new().unwrap();
    let module_id = write_converted_subcommand_binding(dir.path(), "cat-file");
    assert_eq!(module_id, "cli.echo.cat_file");

    let executor = build_test_executor(dir.path());
    assert!(
        executor
            .registry()
            .get_definition(&module_id)
            .expect("registry lookup should succeed")
            .is_some(),
        "a module the scan wrote must be a module the server serves"
    );

    let result = executor
        .call(&module_id, json!({}), None, None)
        .await
        .expect("the hyphenated subcommand should execute");
    assert_eq!(
        result["stdout"].as_str().unwrap_or_default().trim(),
        "cat-file",
        "the folded id must not reach the command line: {result}"
    );
}

/// Write an `/bin/echo`-backed binding whose two flags are declared mutually
/// exclusive, so a caller can provoke the conflict refusal without the wrapped
/// binary ever being spawned.
fn write_conflicting_flags_binding(dir: &Path) {
    let module = ScannedModule::new(
        "cli.echo".to_string(),
        "Echo a message".to_string(),
        json!({
            "type": "object",
            "properties": {
                "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["one"] },
                "one": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
                "message": { "type": "string" },
            },
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
async fn test_repeated_validation_refusals_do_not_trip_the_breaker() {
    // Regression (#22): five conflict refusals opened the circuit for the whole
    // module, and the next perfectly valid call was rejected with
    // CircuitBreakerOpen. Schema trial-and-error is the normal behaviour of an
    // LLM caller — an agent that reads a refusal and corrects itself is the
    // success case, and doing it five times must not disable the tool.
    let dir = TempDir::new().unwrap();
    write_conflicting_flags_binding(dir.path());

    let mut opts = exec_opts(dir.path());
    opts.enable_circuit_breaker = true;
    let executor = build_executor(&opts).expect("executor should build");

    for _ in 0..5 {
        let err = executor
            .call("cli.echo", json!({ "l": true, "one": true }), None, None)
            .await
            .expect_err("conflicting flags must be refused");
        assert_ne!(
            err.code,
            apcore::ErrorCode::CircuitBreakerOpen,
            "a validation refusal must not become a breaker trip: {err:?}"
        );
    }

    let result = executor
        .call(
            "cli.echo",
            json!({ "message": "still_working" }),
            None,
            None,
        )
        .await
        .expect("a valid call after five refusals must still execute");
    assert_eq!(result["exit_code"], 0);
    assert!(result["stdout"]
        .as_str()
        .unwrap_or_default()
        .contains("still_working"));
}

#[tokio::test]
async fn test_mcp_tools_call_option_injection_blocked() {
    // A client-supplied value that the wrapped command would parse as an
    // option must be rejected on the request path. This is the injection that
    // exists when argv goes to execve with no shell in between; shell
    // metacharacters are inert there and are no longer filtered.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let result = executor
        .call(
            "cli.echo",
            json!({ "message": "--output=/etc/passwd" }),
            None,
            None,
        )
        .await;

    assert!(
        result.is_err(),
        "option-like values must be rejected, got: {result:?}"
    );
}

#[tokio::test]
async fn test_mcp_tools_call_passes_shell_metacharacters_through() {
    // The counterpart: a payload full of shell metacharacters reaches the
    // command intact. This is what makes `curl` able to POST a body and `jq`
    // able to receive a filter.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let executor = build_test_executor(dir.path());

    let payload = r#"{"q":"a|b && c","n":$x}"#;
    let result = executor
        .call("cli.echo", json!({ "message": payload }), None, None)
        .await
        .expect("shell metacharacters are ordinary argv bytes and must pass");

    let stdout = result["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains(payload),
        "payload should reach the command unmodified: {result}"
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

/// Drive one JSON-RPC request through a real `apexe serve --transport stdio`
/// process and return the parsed response.
///
/// Closing stdin after the request gives the transport its EOF, so the child
/// answers and exits on its own — no port binding and no timeout juggling.
/// Logs go to stderr (`src/main.rs` pins the subscriber writer), so stdout is
/// pure JSON-RPC.
fn stdio_jsonrpc_roundtrip(modules_dir: &Path, request: &serde_json::Value) -> serde_json::Value {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(env!("CARGO_BIN_EXE_apexe"))
        .args(["serve", "--transport", "stdio", "--modules-dir"])
        .arg(modules_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("apexe serve should start");

    {
        let stdin = child.stdin.as_mut().expect("stdin should be piped");
        writeln!(stdin, "{request}").expect("request should be written");
    }
    // Dropping stdin closes it, which is the transport's shutdown signal.
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("child should exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value.get("result").is_some() || value.get("error").is_some())
        .unwrap_or_else(|| panic!("no JSON-RPC response on stdout: {stdout}"))
}

#[test]
fn test_mcp_initialize_reports_the_apexe_version() {
    // #35 item 4: `serverInfo.version` reported 0.17.2 — apcore-mcp's crate
    // version — so a client could not learn which apexe it was talking to.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());

    let response = stdio_jsonrpc_roundtrip(
        dir.path(),
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "apexe-test", "version": "0" }
            }
        }),
    );

    let server_info = &response["result"]["serverInfo"];
    assert_eq!(
        server_info["version"].as_str(),
        Some(apexe::VERSION),
        "serverInfo.version must be apexe's version: {response}"
    );
    assert_eq!(server_info["name"].as_str(), Some("apexe"));
}

/// Write an ACL file whose single rule carries the given target list.
fn write_acl_with_targets(dir: &Path, targets: &str) -> std::path::PathBuf {
    let path = dir.join("acl.yaml");
    std::fs::write(
        &path,
        format!("default_effect: deny\nrules:\n  - callers: [\"*\"]\n    targets: {targets}\n    effect: deny\n"),
    )
    .expect("acl should be written");
    path
}

#[test]
fn test_mcp_serve_refuses_acl_whose_target_misspells_a_registered_module() {
    // #39 item 5(a): `cli.echo` is registered, the rule names `echo`, and the
    // deny protected nothing. Refuse to build the server rather than serve an
    // ACL the operator wrote and did not get.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let acl_path = write_acl_with_targets(dir.path(), "[\"echo\"]");

    let err = McpServerBuilder::new()
        .modules_dir(dir.path())
        .acl_path(&acl_path)
        .build()
        .err()
        .expect("a near-miss ACL target must refuse to start");
    assert!(
        err.message.contains("cli.echo"),
        "error should name the spelling that works: {}",
        err.message
    );
}

#[test]
fn test_mcp_serve_refuses_acl_with_an_empty_target_list() {
    // #39 item 5(b): apcore accepts `targets: []` and its matcher returns false
    // for an empty pattern list, so the rule is inert.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let acl_path = write_acl_with_targets(dir.path(), "[]");

    let err = McpServerBuilder::new()
        .modules_dir(dir.path())
        .acl_path(&acl_path)
        .build()
        .err()
        .expect("an inert ACL rule must refuse to start");
    assert!(
        err.message.contains("empty list"),
        "error should name the defect: {}",
        err.message
    );
}

#[test]
fn test_mcp_serve_accepts_acl_targeting_the_registered_module() {
    // The validator must not cry wolf on a rule that works.
    let dir = TempDir::new().unwrap();
    write_echo_binding(dir.path());
    let acl_path = write_acl_with_targets(dir.path(), "[\"cli.echo\"]");

    assert!(
        McpServerBuilder::new()
            .modules_dir(dir.path())
            .acl_path(&acl_path)
            .build()
            .is_ok(),
        "a correct ACL must load"
    );
}

/// `x-sensitive` must survive the write to disk, not just the schema builder.
///
/// Redaction is schema-driven and the schema travels in the binding file, so
/// the marker only protects anything if it is still there when the file is
/// read back. Every existing test stops at `build_input_schema`, which leaves
/// the link that actually matters — scan → YAML → load → redact — unguarded:
/// a write path that dropped the annotation would keep all of them green while
/// every credential went to the log verbatim.
///
/// This is also what the manual's re-scan instruction rests on. §11 tells an
/// operator that a binding scanned before the marker existed logs credentials
/// and that re-scanning fixes it; that is only true while a fresh scan really
/// does write the marker.
#[test]
fn test_sensitive_marker_survives_the_write_to_a_binding_file() {
    use apexe::models::{ScannedFlag, ValueType};

    let dir = TempDir::new().unwrap();
    let tool = ScannedCLITool {
        name: "fetch".to_string(),
        description: "Fetch a URL".to_string(),
        binary_path: "/usr/bin/true".to_string(),
        global_flags: vec![ScannedFlag {
            long_name: Some("--user".to_string()),
            description: "Server user and password.".to_string(),
            value_type: ValueType::String,
            ..Default::default()
        }],
        ..Default::default()
    };

    let modules = CliToolConverter::new().convert(&tool);
    YamlOutput::without_verification()
        .write(&modules, dir.path(), false)
        .expect("binding should be written");

    let written: String = std::fs::read_dir(dir.path())
        .expect("the output directory exists")
        .filter_map(Result::ok)
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect();

    assert!(
        written.contains("--user"),
        "the credential-bearing flag must reach the binding: {written}"
    );
    assert!(
        written.contains("x-sensitive"),
        "the marker apcore redacts on must survive the write, or redaction is \
         inert for every scanned tool: {written}"
    );
}
