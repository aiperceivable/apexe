//! Integration config snippets printed by `apexe serve --show-config`.
//!
//! A snippet has to launch (or address) a server that serves the *same* tool
//! surface as the invocation the operator just typed. Emitting a fixed
//! `["serve", "--transport", "stdio"]` regardless of `--modules-dir`,
//! `--prefix`, `--tags`, `--acl` or the governance toggles produced a config
//! that silently exposed a different (often empty) surface: an operator who
//! scanned into a non-default directory got a server with no tools at all.
//!
//! Carrying the flags across is only half of it — every path in a snippet is
//! also resolved against apexe's current directory, because the client that
//! runs the snippet launches `apexe` from its own. See [`path_str`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

/// `--name` default; a snippet only spells the flag out when it differs.
const DEFAULT_SERVER_NAME: &str = "apexe";

/// Client whose configuration file a snippet targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigFormat {
    ClaudeDesktop,
    Cursor,
}

impl ConfigFormat {
    /// Accepted `--show-config` values, shared with clap so the flag's
    /// possible-values list and the parser cannot drift apart.
    pub const VALUES: [&'static str; 2] = ["claude-desktop", "cursor"];

    /// Parse a `--show-config` value.
    pub fn parse(value: &str) -> Result<Self, ConfigGenError> {
        match value {
            "claude-desktop" => Ok(Self::ClaudeDesktop),
            "cursor" => Ok(Self::Cursor),
            other => Err(ConfigGenError::UnknownFormat {
                format: other.to_string(),
            }),
        }
    }
}

/// Failure modes of snippet generation.
///
/// An unknown format must be an error rather than a printed sentence:
/// `apexe serve --show-config vscode > mcp.json` used to write
/// `Unknown config format: vscode` into the config file and exit 0.
#[derive(Debug, Error)]
pub enum ConfigGenError {
    #[error("Unknown config format '{format}' (expected one of: claude-desktop, cursor)")]
    UnknownFormat { format: String },
}

/// The `apexe serve` invocation a snippet has to reproduce.
///
/// Deliberately carries **no credential**. `--auth-token` and `--jwt-secret`
/// are excluded by construction, not by a filter that could be forgotten: a
/// snippet is pasted into a config file that is routinely shared and often
/// committed, so a secret echoed into it leaks the moment it is generated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeInvocation {
    /// MCP server name (`--name`).
    pub name: String,
    /// `--transport`: `stdio`, `http` or `sse`.
    pub transport: String,
    /// `--host`, used to address the HTTP transports.
    pub host: String,
    /// `--port`, used to address the HTTP transports.
    pub port: u16,
    /// `--modules-dir`: without it a launched server reads the default
    /// directory, which is empty for anyone who scanned elsewhere. Stored as
    /// the operator typed it and made absolute when rendered — see
    /// [`path_str`].
    pub modules_dir: Option<PathBuf>,
    /// `--tags` (comma-separated, AND).
    pub tags: Option<String>,
    /// `--prefix`.
    pub prefix: Option<String>,
    /// `--acl`. Made absolute when rendered — see [`path_str`].
    pub acl: Option<PathBuf>,
    /// `--enable-approval`.
    pub enable_approval: bool,
    /// `--no-logging`.
    pub no_logging: bool,
    /// `--no-log-arguments`.
    pub no_log_arguments: bool,
    /// `--no-circuit-breaker`.
    pub no_circuit_breaker: bool,
    /// `--no-retry`.
    pub no_retry: bool,
}

impl Default for ServeInvocation {
    fn default() -> Self {
        Self {
            name: DEFAULT_SERVER_NAME.to_string(),
            transport: "stdio".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8000,
            modules_dir: None,
            tags: None,
            prefix: None,
            acl: None,
            enable_approval: false,
            no_logging: false,
            no_log_arguments: false,
            no_circuit_breaker: false,
            no_retry: false,
        }
    }
}

impl ServeInvocation {
    /// Whether this invocation is launched by the client (stdio) rather than
    /// addressed over the network.
    fn is_stdio(&self) -> bool {
        self.transport == "stdio"
    }

    /// URL an MCP client uses to reach a running server.
    ///
    /// The two HTTP transports are mounted on different paths by apcore-mcp:
    /// streamable HTTP on `/mcp`, the deprecated SSE transport on `/sse`.
    fn endpoint_url(&self) -> String {
        let path = if self.transport == "sse" {
            "/sse"
        } else {
            "/mcp"
        };
        format!("http://{}:{}{}", self.host, self.port, path)
    }

    /// argv a client passes to `apexe` to launch an equivalent stdio server.
    fn stdio_args(&self) -> Vec<String> {
        let mut args = vec![
            "serve".to_string(),
            "--transport".to_string(),
            "stdio".to_string(),
        ];
        if self.name != DEFAULT_SERVER_NAME {
            push_value_flag(&mut args, "--name", Some(self.name.as_str()));
        }
        self.push_surface_flags(&mut args);
        self.push_governance_flags(&mut args);
        args
    }

    /// Flags that decide *which* modules the server serves at all.
    fn push_surface_flags(&self, args: &mut Vec<String>) {
        let modules_dir = path_str(self.modules_dir.as_deref());
        push_value_flag(args, "--modules-dir", modules_dir.as_deref());
        push_value_flag(args, "--tags", self.tags.as_deref());
        push_value_flag(args, "--prefix", self.prefix.as_deref());
    }

    /// Flags that decide how calls are governed and recorded.
    fn push_governance_flags(&self, args: &mut Vec<String>) {
        let acl = path_str(self.acl.as_deref());
        push_value_flag(args, "--acl", acl.as_deref());
        push_bool_flag(args, "--enable-approval", self.enable_approval);
        push_bool_flag(args, "--no-logging", self.no_logging);
        push_bool_flag(args, "--no-log-arguments", self.no_log_arguments);
        push_bool_flag(args, "--no-circuit-breaker", self.no_circuit_breaker);
        push_bool_flag(args, "--no-retry", self.no_retry);
    }
}

/// Append `--flag value` when a value is present.
fn push_value_flag(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        args.push(flag.to_string());
        args.push(value.to_string());
    }
}

/// Append `--flag` when the toggle is on.
fn push_bool_flag(args: &mut Vec<String>, flag: &str, enabled: bool) {
    if enabled {
        args.push(flag.to_string());
    }
}

/// Render a path for argv, resolved against apexe's current directory.
///
/// A snippet is not executed where it was generated. The MCP client launches
/// `apexe` from *its own* working directory — Claude Desktop's is `/`, Cursor's
/// is the workspace root — so `--show-config claude-desktop --modules-dir
/// ./modules` emitted verbatim points the launched server at a directory that
/// does not exist. It then logs "Modules directory not found, starting with
/// zero tools" and exits 0: a server with no tools and no error. `--acl` fails
/// more quietly still — the file is simply absent, so the governance boundary
/// is not the one the operator asked for.
///
/// `std::path::absolute` rather than `canonicalize`: the target need not exist
/// yet (the ordinary case for a `--modules-dir` about to be scanned into), and
/// symlinks stay spelled the way the operator wrote them.
///
/// A path that is not valid UTF-8, or that cannot be resolved at all, is
/// dropped rather than lossily mangled: a wrong directory silently serves the
/// wrong surface, whereas an absent flag serves the documented default.
fn path_str(path: Option<&Path>) -> Option<String> {
    let path = path?;
    let absolute = std::path::absolute(path).ok()?;
    absolute.to_str().map(str::to_string)
}

/// Snippet for a client that launches the server itself.
fn generate_stdio_config(invocation: &ServeInvocation) -> String {
    let config = json!({
        "mcpServers": {
            invocation.name.as_str(): {
                "command": "apexe",
                "args": invocation.stdio_args()
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Snippet for a client that connects to an already-running server.
fn generate_http_config(invocation: &ServeInvocation) -> String {
    let config = json!({
        "mcpServers": {
            invocation.name.as_str(): {
                "url": invocation.endpoint_url()
            }
        }
    });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Generate the integration snippet for `format`.
///
/// Claude Desktop and Cursor read the same `mcpServers` shape, so both formats
/// currently render identically; the parameter is kept because the *file* they
/// go in differs and the shapes are versioned independently upstream. What
/// must not differ again is the transport: `cursor` previously ignored
/// `--transport` entirely and emitted a stdio command block for an HTTP server.
pub fn generate_config(
    format: &str,
    invocation: &ServeInvocation,
) -> Result<String, ConfigGenError> {
    // Matching on the enum (rather than ignoring it) is what makes a new
    // client format a compile error here instead of a silent alias.
    let snippet = match (ConfigFormat::parse(format)?, invocation.is_stdio()) {
        (ConfigFormat::ClaudeDesktop | ConfigFormat::Cursor, true) => {
            generate_stdio_config(invocation)
        }
        (ConfigFormat::ClaudeDesktop | ConfigFormat::Cursor, false) => {
            generate_http_config(invocation)
        }
    };
    Ok(snippet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn stdio_invocation() -> ServeInvocation {
        ServeInvocation::default()
    }

    fn http_invocation() -> ServeInvocation {
        ServeInvocation {
            transport: "http".to_string(),
            host: "localhost".to_string(),
            port: 8000,
            ..ServeInvocation::default()
        }
    }

    fn parse(snippet: &str) -> Value {
        serde_json::from_str(snippet).expect("snippet must be valid JSON")
    }

    fn args_of(snippet: &str, name: &str) -> Vec<String> {
        parse(snippet)["mcpServers"][name]["args"]
            .as_array()
            .expect("stdio snippet must carry args")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn test_generate_config_stdio_emits_command_block() {
        let snippet = generate_config("claude-desktop", &stdio_invocation()).unwrap();
        let parsed = parse(&snippet);
        assert_eq!(parsed["mcpServers"]["apexe"]["command"], "apexe");
        assert_eq!(
            args_of(&snippet, "apexe"),
            vec!["serve", "--transport", "stdio"]
        );
    }

    #[test]
    fn test_generate_config_http_emits_url() {
        let snippet = generate_config("claude-desktop", &http_invocation()).unwrap();
        assert_eq!(
            parse(&snippet)["mcpServers"]["apexe"]["url"],
            "http://localhost:8000/mcp"
        );
    }

    #[test]
    fn test_generate_config_sse_uses_the_sse_endpoint_path() {
        // apcore-mcp mounts streamable HTTP on /mcp and SSE on /sse; emitting
        // /mcp for an SSE server produced a snippet that could never connect.
        let invocation = ServeInvocation {
            transport: "sse".to_string(),
            ..http_invocation()
        };
        let snippet = generate_config("claude-desktop", &invocation).unwrap();
        assert_eq!(
            parse(&snippet)["mcpServers"]["apexe"]["url"],
            "http://localhost:8000/sse"
        );
    }

    #[test]
    fn test_generate_config_cursor_honours_http_transport() {
        // Regression for issue #37.3: `("cursor", _)` ignored the transport, so
        // `--show-config cursor --transport http` emitted a stdio command block
        // that launched a second, unconfigured server.
        let snippet = generate_config("cursor", &http_invocation()).unwrap();
        let parsed = parse(&snippet);
        assert_eq!(
            parsed["mcpServers"]["apexe"]["url"],
            "http://localhost:8000/mcp"
        );
        assert!(parsed["mcpServers"]["apexe"]["command"].is_null());
    }

    #[test]
    fn test_generate_config_stdio_carries_modules_dir() {
        // The defect that made a generated config expose no tools at all.
        let invocation = ServeInvocation {
            modules_dir: Some(PathBuf::from("/srv/apexe/modules")),
            ..stdio_invocation()
        };
        let snippet = generate_config("claude-desktop", &invocation).unwrap();
        let args = args_of(&snippet, "apexe");
        assert!(
            args.windows(2)
                .any(|w| w == ["--modules-dir", "/srv/apexe/modules"]),
            "args: {args:?}"
        );
    }

    #[test]
    fn test_generate_config_makes_relative_paths_absolute() {
        // The client launches `apexe` from its own working directory, not the
        // one `--show-config` ran in, so a verbatim `./modules` points the
        // launched server at a directory that does not exist: it logs
        // "Modules directory not found, starting with zero tools" and exits 0.
        let invocation = ServeInvocation {
            modules_dir: Some(PathBuf::from("./modules")),
            acl: Some(PathBuf::from("policy/acl.yaml")),
            ..stdio_invocation()
        };
        let args = args_of(
            &generate_config("claude-desktop", &invocation).unwrap(),
            "apexe",
        );

        for flag in ["--modules-dir", "--acl"] {
            let value = args
                .windows(2)
                .find(|w| w[0] == flag)
                .map(|w| w[1].clone())
                .unwrap_or_else(|| panic!("{flag} missing from {args:?}"));
            assert!(
                Path::new(&value).is_absolute(),
                "{flag} was emitted relative: {value}"
            );
        }

        let cwd = std::env::current_dir().unwrap();
        assert!(args
            .windows(2)
            .any(|w| { w[0] == "--modules-dir" && w[1] == cwd.join("modules").to_str().unwrap() }));
        assert!(args
            .windows(2)
            .any(|w| { w[0] == "--acl" && w[1] == cwd.join("policy/acl.yaml").to_str().unwrap() }));
    }

    #[test]
    fn test_generate_config_leaves_absolute_paths_alone() {
        let invocation = ServeInvocation {
            modules_dir: Some(PathBuf::from("/srv/apexe/modules")),
            acl: Some(PathBuf::from("/etc/apexe/acl.yaml")),
            ..stdio_invocation()
        };
        let args = args_of(&generate_config("cursor", &invocation).unwrap(), "apexe");
        assert!(args
            .windows(2)
            .any(|w| w == ["--modules-dir", "/srv/apexe/modules"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--acl", "/etc/apexe/acl.yaml"]));
    }

    #[test]
    fn test_generate_config_stdio_carries_surface_and_governance_flags() {
        let invocation = ServeInvocation {
            modules_dir: Some(PathBuf::from("/srv/modules")),
            tags: Some("readonly,git".to_string()),
            prefix: Some("cli.git".to_string()),
            acl: Some(PathBuf::from("/etc/apexe/acl.yaml")),
            enable_approval: true,
            no_logging: true,
            no_log_arguments: true,
            no_circuit_breaker: true,
            no_retry: true,
            ..stdio_invocation()
        };
        let args = args_of(
            &generate_config("cursor", &invocation).unwrap(),
            DEFAULT_SERVER_NAME,
        );
        for expected in [
            "--modules-dir",
            "--tags",
            "--prefix",
            "--acl",
            "--enable-approval",
            "--no-logging",
            "--no-log-arguments",
            "--no-circuit-breaker",
            "--no-retry",
        ] {
            assert!(
                args.iter().any(|a| a == expected),
                "{expected} missing from {args:?}"
            );
        }
        assert!(args.windows(2).any(|w| w == ["--tags", "readonly,git"]));
        assert!(args.windows(2).any(|w| w == ["--prefix", "cli.git"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--acl", "/etc/apexe/acl.yaml"]));
    }

    #[test]
    fn test_generate_config_stdio_omits_unset_flags() {
        let args = args_of(
            &generate_config("claude-desktop", &stdio_invocation()).unwrap(),
            "apexe",
        );
        assert_eq!(args.len(), 3, "unset flags must not be emitted: {args:?}");
    }

    #[test]
    fn test_generate_config_custom_name_is_key_and_flag() {
        let invocation = ServeInvocation {
            name: "my-tools".to_string(),
            ..stdio_invocation()
        };
        let snippet = generate_config("claude-desktop", &invocation).unwrap();
        assert!(parse(&snippet)["mcpServers"]["my-tools"].is_object());
        // The launched server must announce the same name the config keys it by.
        let args = args_of(&snippet, "my-tools");
        assert!(args.windows(2).any(|w| w == ["--name", "my-tools"]));
    }

    #[test]
    fn test_generate_config_default_name_is_not_spelled_out() {
        let args = args_of(
            &generate_config("claude-desktop", &stdio_invocation()).unwrap(),
            "apexe",
        );
        assert!(!args.iter().any(|a| a == "--name"), "args: {args:?}");
    }

    #[test]
    fn test_generate_config_never_echoes_credentials() {
        // A snippet is pasted into a shared config file. `--auth-token` and
        // `--jwt-secret` have no field on ServeInvocation for exactly this
        // reason; this test pins that they cannot reappear via any other flag.
        let invocation = ServeInvocation {
            transport: "http".to_string(),
            prefix: Some("cli.git".to_string()),
            ..stdio_invocation()
        };
        for format in ConfigFormat::VALUES {
            let snippet = generate_config(format, &invocation).unwrap();
            for forbidden in ["--auth-token", "--jwt-secret", "APEXE_AUTH_TOKEN"] {
                assert!(
                    !snippet.contains(forbidden),
                    "{format} snippet leaked {forbidden}: {snippet}"
                );
            }
        }
    }

    #[test]
    fn test_generate_config_unknown_format_is_an_error() {
        let err = generate_config("vscode", &stdio_invocation()).unwrap_err();
        assert!(matches!(err, ConfigGenError::UnknownFormat { .. }));
        let message = err.to_string();
        assert!(message.contains("vscode"), "message: {message}");
        assert!(message.contains("claude-desktop"), "message: {message}");
    }

    #[test]
    fn test_config_format_parse_accepts_every_advertised_value() {
        for value in ConfigFormat::VALUES {
            assert!(ConfigFormat::parse(value).is_ok(), "rejected {value}");
        }
    }

    #[test]
    fn test_endpoint_url_uses_host_and_port() {
        let invocation = ServeInvocation {
            transport: "http".to_string(),
            host: "0.0.0.0".to_string(),
            port: 9111,
            ..stdio_invocation()
        };
        assert_eq!(invocation.endpoint_url(), "http://0.0.0.0:9111/mcp");
    }
}
