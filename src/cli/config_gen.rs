use serde_json::json;

/// Generate Claude Desktop configuration snippet for stdio transport.
pub fn generate_claude_config_stdio(name: &str) -> String {
    let config = json!({
        "mcpServers": {
            name: {
                "command": "apexe",
                "args": ["serve", "--transport", "stdio"]
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Generate Claude Desktop configuration snippet for HTTP transport.
pub fn generate_claude_config_http(name: &str, host: &str, port: u16) -> String {
    let url = format!("http://{host}:{port}/mcp");
    let config = json!({
        "mcpServers": {
            name: {
                "url": url
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Generate Cursor MCP configuration snippet.
pub fn generate_cursor_config(name: &str) -> String {
    let config = json!({
        "mcpServers": {
            name: {
                "command": "apexe",
                "args": ["serve"]
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Generate config based on transport type.
pub fn generate_config(format: &str, name: &str, transport: &str, host: &str, port: u16) -> String {
    match (format, transport) {
        ("claude-desktop", "stdio") => generate_claude_config_stdio(name),
        ("claude-desktop", _) => generate_claude_config_http(name, host, port),
        ("cursor", _) => generate_cursor_config(name),
        _ => format!("Unknown config format: {format}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_generate_claude_desktop_stdio_config() {
        let config = generate_claude_config_stdio("apexe");
        let parsed: Value = serde_json::from_str(&config).unwrap();

        assert_eq!(parsed["mcpServers"]["apexe"]["command"], "apexe");
        let args = parsed["mcpServers"]["apexe"]["args"].as_array().unwrap();
        assert_eq!(args, &["serve", "--transport", "stdio"]);
    }

    #[test]
    fn test_generate_claude_desktop_http_config() {
        let config = generate_claude_config_http("apexe", "localhost", 8000);
        let parsed: Value = serde_json::from_str(&config).unwrap();

        assert_eq!(
            parsed["mcpServers"]["apexe"]["url"],
            "http://localhost:8000/mcp"
        );
    }

    #[test]
    fn test_generate_cursor_config() {
        let config = generate_cursor_config("apexe");
        let parsed: Value = serde_json::from_str(&config).unwrap();

        assert_eq!(parsed["mcpServers"]["apexe"]["command"], "apexe");
        let args = parsed["mcpServers"]["apexe"]["args"].as_array().unwrap();
        assert_eq!(args, &["serve"]);
    }

    #[test]
    fn test_config_custom_name() {
        let config = generate_claude_config_stdio("my-tools");
        let parsed: Value = serde_json::from_str(&config).unwrap();
        assert!(parsed["mcpServers"]["my-tools"].is_object());
    }

    #[test]
    fn test_config_custom_port() {
        let config = generate_claude_config_http("apexe", "localhost", 9000);
        let parsed: Value = serde_json::from_str(&config).unwrap();
        assert_eq!(
            parsed["mcpServers"]["apexe"]["url"],
            "http://localhost:9000/mcp"
        );
    }

    #[test]
    fn test_generate_config_dispatcher() {
        let stdio = generate_config("claude-desktop", "apexe", "stdio", "localhost", 8000);
        assert!(stdio.contains("command"));

        let http = generate_config("claude-desktop", "apexe", "http", "localhost", 8000);
        assert!(http.contains("url"));

        let cursor = generate_config("cursor", "apexe", "stdio", "localhost", 8000);
        assert!(cursor.contains("command"));
    }

    #[test]
    fn test_unknown_format() {
        let result = generate_config("unknown", "apexe", "stdio", "localhost", 8000);
        assert!(result.contains("Unknown config format"));
    }
}
