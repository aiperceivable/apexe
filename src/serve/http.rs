use std::sync::Arc;

use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tracing::info;

use super::handler::McpHandler;
use super::mcp_types::JsonRpcRequest;

/// Application state shared across HTTP handlers.
pub(crate) struct AppState {
    handler: McpHandler,
}

/// Serve MCP over HTTP using axum.
pub async fn serve_http(handler: McpHandler, host: &str, port: u16) -> Result<()> {
    let state = Arc::new(AppState { handler });

    let app = build_router(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "Starting MCP HTTP server");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the axum router (exposed for testing).
pub(crate) fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(handle_root))
        .route("/mcp", post(handle_mcp_post))
        .route("/mcp/sse", get(handle_sse_stub))
        .route("/health", get(handle_health))
        .route("/explorer", get(handle_explorer))
        .with_state(state)
}

async fn handle_mcp_post(
    State(state): State<Arc<AppState>>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let response = state.handler.handle_request(request);
    Json(response)
}

async fn handle_sse_stub() -> impl IntoResponse {
    // SSE stub for future implementation
    (
        StatusCode::NOT_IMPLEMENTED,
        "SSE transport not yet implemented",
    )
}

async fn handle_health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn handle_root(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let tools: Vec<&str> = state
        .handler
        .registry()
        .list()
        .iter()
        .map(|b| b.module_id.as_str())
        .collect();
    Json(json!({
        "name": state.handler.server_name(),
        "version": crate::VERSION,
        "protocol": "MCP",
        "transport": "http",
        "tools_count": tools.len(),
        "endpoints": {
            "mcp": "POST /mcp",
            "health": "GET /health",
            "explorer": "GET /explorer"
        }
    }))
}

async fn handle_explorer(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let tools = state.handler.registry().list();
    let mut rows = String::new();
    for tool in &tools {
        let annotations_json =
            serde_json::to_string(&tool.annotations).unwrap_or_else(|_| "{}".to_string());
        rows.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td><code>{}</code></td></tr>\n",
            tool.module_id, tool.description, annotations_json,
        ));
    }
    if rows.is_empty() {
        rows = "<tr><td colspan=\"3\">No tools loaded. Run <code>apexe scan &lt;tool&gt;</code> to add tools.</td></tr>".to_string();
    }
    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>{name} — Tool Explorer</title>
<style>
  body {{ font-family: system-ui, sans-serif; max-width: 960px; margin: 2rem auto; padding: 0 1rem; color: #1a1a1a; }}
  h1 {{ border-bottom: 2px solid #7c3aed; padding-bottom: 0.5rem; }}
  table {{ width: 100%; border-collapse: collapse; margin-top: 1rem; }}
  th, td {{ text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #e5e7eb; }}
  th {{ background: #f9fafb; font-weight: 600; }}
  code {{ background: #f3f4f6; padding: 0.1rem 0.3rem; border-radius: 3px; font-size: 0.9em; }}
  .meta {{ color: #6b7280; font-size: 0.9rem; margin-bottom: 1rem; }}
</style>
</head>
<body>
<h1>{name}</h1>
<p class="meta">apexe v{version} — {count} tool(s) loaded</p>
<table>
<thead><tr><th>Module ID</th><th>Description</th><th>Annotations</th></tr></thead>
<tbody>
{rows}
</tbody>
</table>
</body>
</html>"#,
        name = state.handler.server_name(),
        version = crate::VERSION,
        count = tools.len(),
        rows = rows,
    ))
}

/// Create an app state for testing.
pub fn make_test_app(handler: McpHandler) -> Router {
    let state = Arc::new(AppState { handler });
    build_router(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::loader::LoadedBinding;
    use crate::serve::mcp_types::JsonRpcResponse;
    use crate::serve::registry::ToolRegistry;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use std::collections::HashMap;
    use tower::ServiceExt;

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
        McpHandler::new(registry, "test-http".to_string())
    }

    fn make_json_request(body: Value) -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_http_tools_list() {
        let app = make_test_app(make_handler());

        let request = make_json_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }));

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: JsonRpcResponse = serde_json::from_slice(&body).unwrap();
        assert!(resp.error.is_none());
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "cli.echo");
    }

    #[tokio::test]
    async fn test_http_initialize() {
        let app = make_test_app(make_handler());

        let request = make_json_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        }));

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: JsonRpcResponse = serde_json::from_slice(&body).unwrap();
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["serverInfo"]["name"], "test-http");
    }

    #[tokio::test]
    async fn test_http_tools_call() {
        let app = make_test_app(make_handler());

        let request = make_json_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "cli.echo",
                "arguments": {}
            }
        }));

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: JsonRpcResponse = serde_json::from_slice(&body).unwrap();
        assert!(resp.error.is_none());
        let content = resp.result.unwrap()["content"].as_array().unwrap().clone();
        assert!(!content.is_empty());
        assert!(content[0]["text"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_http_health() {
        let app = make_test_app(make_handler());

        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_http_sse_stub() {
        let app = make_test_app(make_handler());

        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/mcp/sse")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
