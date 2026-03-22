use serde_json::json;
use tempfile::TempDir;

use apexe::serve::handler::McpHandler;
use apexe::serve::loader::load_bindings;
use apexe::serve::mcp_types::{JsonRpcRequest, JsonRpcResponse, INVALID_PARAMS, METHOD_NOT_FOUND};
use apexe::serve::registry::ToolRegistry;
use apexe::serve::stdio::serve_stdio_with_io;

fn write_echo_binding(dir: &std::path::Path) {
    let content = r#"bindings:
  - module_id: "cli.echo"
    description: "Echo text to stdout"
    target: "apexe::executor::execute_cli"
    input_schema:
      type: object
      properties: {}
    output_schema:
      type: object
      properties:
        stdout:
          type: string
        stderr:
          type: string
        exit_code:
          type: integer
    tags:
      - cli
    version: "1.0.0"
    annotations: {}
    metadata:
      apexe_binary: "echo"
      apexe_command:
        - "echo"
        - "hello"
      apexe_timeout: 30
"#;
    std::fs::write(dir.join("echo.binding.yaml"), content).unwrap();
}

fn setup_handler(tmp: &TempDir) -> McpHandler {
    write_echo_binding(tmp.path());
    let bindings = load_bindings(tmp.path()).unwrap();
    let registry = ToolRegistry::from_bindings(bindings);
    McpHandler::new(registry, "integration-test".to_string())
}

#[test]
fn test_full_pipeline_load_and_list() {
    let tmp = TempDir::new().unwrap();
    let handler = setup_handler(&tmp);

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: json!(1),
        method: "tools/list".to_string(),
        params: None,
    };

    let resp = handler.handle_request(req);
    assert!(resp.error.is_none());

    let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "cli.echo");
    assert_eq!(tools[0]["description"], "Echo text to stdout");
}

#[test]
fn test_full_pipeline_call_tool() {
    let tmp = TempDir::new().unwrap();
    let handler = setup_handler(&tmp);

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: json!(2),
        method: "tools/call".to_string(),
        params: Some(json!({"name": "cli.echo", "arguments": {}})),
    };

    let resp = handler.handle_request(req);
    assert!(resp.error.is_none());

    let result = resp.result.unwrap();
    let content = result["content"].as_array().unwrap();
    assert!(!content.is_empty());
    let text = content[0]["text"].as_str().unwrap();
    assert!(text.contains("hello"));
}

#[test]
fn test_full_pipeline_call_nonexistent_tool() {
    let tmp = TempDir::new().unwrap();
    let handler = setup_handler(&tmp);

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: json!(3),
        method: "tools/call".to_string(),
        params: Some(json!({"name": "nonexistent.tool"})),
    };

    let resp = handler.handle_request(req);
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
}

#[test]
fn test_full_pipeline_unknown_method() {
    let tmp = TempDir::new().unwrap();
    let handler = setup_handler(&tmp);

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: json!(4),
        method: "unknown/method".to_string(),
        params: None,
    };

    let resp = handler.handle_request(req);
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
}

#[test]
fn test_stdio_integration() {
    let tmp = TempDir::new().unwrap();
    let handler = setup_handler(&tmp);

    // Send initialize + tools/list + tools/call via stdio
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cli.echo","arguments":{}}}"#,
        "\n",
    );

    let reader = std::io::Cursor::new(input.as_bytes());
    let mut output = Vec::new();
    serve_stdio_with_io(&handler, reader, &mut output).unwrap();

    let output_str = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = output_str.trim().lines().collect();
    assert_eq!(lines.len(), 3);

    // Verify initialize
    let resp1: JsonRpcResponse = serde_json::from_str(lines[0]).unwrap();
    assert!(resp1.error.is_none());
    assert_eq!(
        resp1.result.unwrap()["serverInfo"]["name"],
        "integration-test"
    );

    // Verify tools/list
    let resp2: JsonRpcResponse = serde_json::from_str(lines[1]).unwrap();
    assert!(resp2.error.is_none());
    let tools = resp2.result.unwrap()["tools"].as_array().unwrap().clone();
    assert_eq!(tools[0]["name"], "cli.echo");

    // Verify tools/call
    let resp3: JsonRpcResponse = serde_json::from_str(lines[2]).unwrap();
    assert!(resp3.error.is_none());
    let text = resp3.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("hello"));
}

#[test]
fn test_empty_modules_dir() {
    let tmp = TempDir::new().unwrap();
    let bindings = load_bindings(tmp.path()).unwrap();
    assert!(bindings.is_empty());
}

#[test]
fn test_multiple_binding_files() {
    let tmp = TempDir::new().unwrap();

    // Write two binding files
    write_echo_binding(tmp.path());

    let content2 = r#"bindings:
  - module_id: "cli.cat"
    description: "Concatenate files"
    target: "apexe::executor::execute_cli"
    input_schema:
      type: object
      properties: {}
    output_schema:
      type: object
    tags:
      - cli
    version: "1.0.0"
    annotations: {}
    metadata:
      apexe_binary: "cat"
      apexe_command:
        - "cat"
      apexe_timeout: 30
"#;
    std::fs::write(tmp.path().join("cat.binding.yaml"), content2).unwrap();

    let bindings = load_bindings(tmp.path()).unwrap();
    assert_eq!(bindings.len(), 2);

    let registry = ToolRegistry::from_bindings(bindings);
    let handler = McpHandler::new(registry, "test".to_string());

    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: json!(1),
        method: "tools/list".to_string(),
        params: None,
    };

    let resp = handler.handle_request(req);
    let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
    assert_eq!(tools.len(), 2);
}
