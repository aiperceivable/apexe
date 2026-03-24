use serde_json::{json, Value};
use tracing::{info, warn};

use super::mcp_types::*;
use super::registry::ToolRegistry;
use crate::executor::execute_cli;

/// MCP protocol handler that dispatches JSON-RPC requests to the tool registry.
pub struct McpHandler {
    registry: ToolRegistry,
    server_name: String,
}

impl McpHandler {
    /// Create a new handler with the given registry and server name.
    pub fn new(registry: ToolRegistry, server_name: String) -> Self {
        Self {
            registry,
            server_name,
        }
    }

    /// Returns a reference to the tool registry.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Returns the server name.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Handle a JSON-RPC request and return a response.
    pub fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        info!(method = %request.method, "Handling MCP request");

        match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id),
            "initialized" => {
                // Notification, return empty success
                JsonRpcResponse::success(request.id, json!({}))
            }
            "tools/list" => self.handle_tools_list(request.id),
            "tools/call" => self.handle_tools_call(request.id, request.params),
            "ping" => JsonRpcResponse::success(request.id, json!({})),
            _ => {
                warn!(method = %request.method, "Unknown method");
                JsonRpcResponse::error(
                    request.id,
                    METHOD_NOT_FOUND,
                    format!("Method not found: {}", request.method),
                )
            }
        }
    }

    fn handle_initialize(&self, id: Value) -> JsonRpcResponse {
        let result = InitializeResult {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
            },
            server_info: ServerInfo {
                name: self.server_name.clone(),
                version: crate::VERSION.to_string(),
            },
        };

        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap_or(json!({})))
    }

    fn handle_tools_list(&self, id: Value) -> JsonRpcResponse {
        let tools: Vec<McpTool> = self
            .registry
            .list()
            .iter()
            .map(|binding| McpTool {
                name: binding.module_id.clone(),
                description: if binding.description.is_empty() {
                    None
                } else {
                    Some(binding.description.clone())
                },
                input_schema: binding.input_schema.clone(),
            })
            .collect();

        let result = ToolsListResult { tools };
        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap_or(json!({})))
    }

    fn handle_tools_call(&self, id: Value, params: Option<Value>) -> JsonRpcResponse {
        let params = match params {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing params");
            }
        };

        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => {
                return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing 'name' in params");
            }
        };

        let binding = match self.registry.get(&tool_name) {
            Some(b) => b,
            None => {
                return JsonRpcResponse::error(
                    id,
                    INVALID_PARAMS,
                    format!("Tool not found: {tool_name}"),
                );
            }
        };

        // Extract arguments from params
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        // Validate arguments against input_schema (basic type check)
        if let Some(schema_type) = binding.input_schema.get("type").and_then(|t| t.as_str()) {
            if schema_type == "object" && !arguments.is_object() {
                return JsonRpcResponse::error(id, INVALID_PARAMS, "Arguments must be an object");
            }
        }

        let kwargs = match arguments.as_object() {
            Some(map) => map.clone(),
            None => serde_json::Map::new(),
        };

        // Execute the tool
        match execute_cli(
            &binding.tool_binary,
            &binding.tool_command,
            binding.timeout,
            binding.json_flag.as_deref(),
            None,
            &kwargs,
        ) {
            Ok(result_map) => {
                let stdout = result_map
                    .get("stdout")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let stderr = result_map
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let exit_code = result_map
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1);

                let mut content = vec![ToolCallContent {
                    content_type: "text".to_string(),
                    text: stdout,
                }];

                if !stderr.is_empty() {
                    content.push(ToolCallContent {
                        content_type: "text".to_string(),
                        text: format!("[stderr] {stderr}"),
                    });
                }

                let is_error = if exit_code != 0 { Some(true) } else { None };

                let call_result = ToolsCallResult { content, is_error };
                JsonRpcResponse::success(id, serde_json::to_value(call_result).unwrap_or(json!({})))
            }
            Err(e) => {
                let call_result = ToolsCallResult {
                    content: vec![ToolCallContent {
                        content_type: "text".to_string(),
                        text: format!("Execution error: {e}"),
                    }],
                    is_error: Some(true),
                };
                JsonRpcResponse::success(id, serde_json::to_value(call_result).unwrap_or(json!({})))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::loader::LoadedBinding;
    use std::collections::HashMap;

    fn make_echo_binding() -> LoadedBinding {
        LoadedBinding {
            module_id: "cli.echo".to_string(),
            description: "Echo text to stdout".to_string(),
            input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            output_schema: json!({"type": "object"}),
            annotations: HashMap::new(),
            tool_command: vec!["echo".to_string(), "hello".to_string()],
            tool_binary: "echo".to_string(),
            timeout: 30,
            json_flag: None,
        }
    }

    fn make_handler() -> McpHandler {
        let registry = ToolRegistry::from_bindings(vec![make_echo_binding()]);
        McpHandler::new(registry, "test-server".to_string())
    }

    fn make_request(method: &str, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn test_handle_initialize() {
        let handler = make_handler();
        let req = make_request("initialize", None);
        let resp = handler.handle_request(req);

        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "test-server");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn test_handle_tools_list() {
        let handler = make_handler();
        let req = make_request("tools/list", None);
        let resp = handler.handle_request(req);

        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "cli.echo");
        assert_eq!(tools[0]["description"], "Echo text to stdout");
    }

    #[test]
    fn test_handle_tools_call() {
        let handler = make_handler();
        let req = make_request(
            "tools/call",
            Some(json!({"name": "cli.echo", "arguments": {}})),
        );
        let resp = handler.handle_request(req);

        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        assert!(!content.is_empty());
        // echo prints "hello\n"
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("hello"));
    }

    #[test]
    fn test_handle_tools_call_not_found() {
        let handler = make_handler();
        let req = make_request("tools/call", Some(json!({"name": "nonexistent"})));
        let resp = handler.handle_request(req);

        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn test_handle_tools_call_missing_params() {
        let handler = make_handler();
        let req = make_request("tools/call", None);
        let resp = handler.handle_request(req);

        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn test_handle_tools_call_missing_name() {
        let handler = make_handler();
        let req = make_request("tools/call", Some(json!({"arguments": {}})));
        let resp = handler.handle_request(req);

        assert!(resp.error.is_some());
    }

    #[test]
    fn test_handle_unknown_method() {
        let handler = make_handler();
        let req = make_request("unknown/method", None);
        let resp = handler.handle_request(req);

        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[test]
    fn test_handle_ping() {
        let handler = make_handler();
        let req = make_request("ping", None);
        let resp = handler.handle_request(req);

        assert!(resp.error.is_none());
    }

    #[test]
    fn test_handle_initialized_notification() {
        let handler = make_handler();
        let req = make_request("initialized", None);
        let resp = handler.handle_request(req);

        assert!(resp.error.is_none());
    }

    #[test]
    fn test_response_id_preserved() {
        let handler = make_handler();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!("my-id-123"),
            method: "ping".to_string(),
            params: None,
        };
        let resp = handler.handle_request(req);
        assert_eq!(resp.id, json!("my-id-123"));
    }
}
