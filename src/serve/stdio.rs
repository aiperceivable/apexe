use std::io::{self, BufRead, Write};

use anyhow::Result;
use tracing::{debug, error, info};

use super::handler::McpHandler;
use super::mcp_types::{JsonRpcRequest, JsonRpcResponse, PARSE_ERROR};

/// Serve MCP over stdio (stdin/stdout), one JSON-RPC message per line.
///
/// This is the transport used by Claude Desktop, Cursor, and other MCP clients.
pub fn serve_stdio(handler: &McpHandler) -> Result<()> {
    info!("Starting MCP stdio server");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to read stdin: {e}");
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        debug!(input = %line, "Received JSON-RPC message");

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => handler.handle_request(request),
            Err(e) => {
                error!("Failed to parse JSON-RPC request: {e}");
                JsonRpcResponse::error(
                    serde_json::Value::Null,
                    PARSE_ERROR,
                    format!("Parse error: {e}"),
                )
            }
        };

        let response_json = serde_json::to_string(&response)?;
        debug!(output = %response_json, "Sending JSON-RPC response");

        writeln!(stdout, "{response_json}")?;
        stdout.flush()?;
    }

    info!("MCP stdio server shutting down");
    Ok(())
}

/// Serve MCP over stdio using custom readers/writers (for testing).
pub fn serve_stdio_with_io<R: BufRead, W: Write>(
    handler: &McpHandler,
    reader: R,
    writer: &mut W,
) -> Result<()> {
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to read: {e}");
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => handler.handle_request(request),
            Err(e) => JsonRpcResponse::error(
                serde_json::Value::Null,
                PARSE_ERROR,
                format!("Parse error: {e}"),
            ),
        };

        let response_json = serde_json::to_string(&response)?;
        writeln!(writer, "{response_json}")?;
        writer.flush()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::loader::LoadedBinding;
    use crate::serve::registry::ToolRegistry;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::Cursor;

    fn make_handler() -> McpHandler {
        let binding = LoadedBinding {
            module_id: "cli.echo".to_string(),
            description: "Echo text".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            output_schema: json!({"type": "object"}),
            annotations: HashMap::new(),
            tool_command: vec!["echo".to_string(), "hello".to_string()],
            tool_binary: "echo".to_string(),
            timeout: 30,
            json_flag: None,
        };
        let registry = ToolRegistry::from_bindings(vec![binding]);
        McpHandler::new(registry, "test".to_string())
    }

    #[test]
    fn test_stdio_tools_list() {
        let handler = make_handler();
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        serve_stdio_with_io(&handler, reader, &mut output).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(output_str.trim()).unwrap();
        assert!(resp.error.is_none());
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "cli.echo");
    }

    #[test]
    fn test_stdio_initialize() {
        let handler = make_handler();
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        serve_stdio_with_io(&handler, reader, &mut output).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(output_str.trim()).unwrap();
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["serverInfo"]["name"], "test");
    }

    #[test]
    fn test_stdio_multiple_messages() {
        let handler = make_handler();
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        serve_stdio_with_io(&handler, reader, &mut output).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = output_str.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let resp1: JsonRpcResponse = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(resp1.id, json!(1));

        let resp2: JsonRpcResponse = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(resp2.id, json!(2));
    }

    #[test]
    fn test_stdio_invalid_json() {
        let handler = make_handler();
        let input = "not valid json\n";
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        serve_stdio_with_io(&handler, reader, &mut output).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(output_str.trim()).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, PARSE_ERROR);
    }

    #[test]
    fn test_stdio_empty_lines_skipped() {
        let handler = make_handler();
        let input = concat!(
            "\n",
            "\n",
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            "\n",
            "\n",
        );
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        serve_stdio_with_io(&handler, reader, &mut output).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = output_str.trim().lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_stdio_eof_graceful() {
        let handler = make_handler();
        let reader = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let result = serve_stdio_with_io(&handler, reader, &mut output);
        assert!(result.is_ok());
    }
}
