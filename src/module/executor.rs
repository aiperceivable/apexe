use apcore::{ErrorCode, ModuleError};
use serde_json::Value;

/// Characters that MUST NOT appear in command arguments to prevent injection.
/// Includes shell metacharacters, quotes, null bytes, and redirection operators.
const SHELL_INJECTION_CHARS: &[char] = &[
    ';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r', '\0', '(', ')', '<', '>',
];

/// Validate that a value does not contain shell injection characters.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn validate_no_injection(param_name: &str, value: &str) -> Result<(), ModuleError> {
    let found: Vec<char> = value
        .chars()
        .filter(|c| SHELL_INJECTION_CHARS.contains(c))
        .collect();
    if !found.is_empty() {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Parameter '{}' contains prohibited characters: {:?}",
                param_name, found
            ),
        ));
    }
    Ok(())
}

fn json_value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Build CLI args from JSON kwargs. Returns Vec of --flag value pairs.
///
/// Bool true becomes `--flag`, false is skipped, null is skipped,
/// arrays repeat `--flag item` for each element, and underscores in
/// keys become hyphens in flag names.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn build_arguments(
    kwargs: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, ModuleError> {
    let mut args: Vec<String> = Vec::new();
    for (key, value) in kwargs {
        match value {
            Value::Null => continue,
            Value::Bool(b) => {
                if *b {
                    let flag = format!("--{}", key.replace('_', "-"));
                    args.push(flag);
                }
            }
            Value::Array(items) => {
                for item in items {
                    let s = json_value_to_string(item);
                    validate_no_injection(key, &s)?;
                    args.push(format!("--{}", key.replace('_', "-")));
                    args.push(s);
                }
            }
            other => {
                let s = json_value_to_string(other);
                validate_no_injection(key, &s)?;
                args.push(format!("--{}", key.replace('_', "-")));
                args.push(s);
            }
        }
    }
    Ok(args)
}

/// Output from a subprocess execution.
#[derive(Debug)]
pub struct SubprocessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Execute a subprocess using `tokio::task::spawn_blocking` with timeout.
///
/// Runs the given binary with args directly (no shell), optionally appending
/// json_flag parts. Returns stdout, stderr, and exit code.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub async fn execute_subprocess(
    binary_path: &str,
    args: &[String],
    json_flag: Option<&str>,
    timeout_ms: u64,
) -> Result<SubprocessOutput, ModuleError> {
    let mut full_args: Vec<String> = args.to_vec();
    if let Some(flag) = json_flag {
        for part in shell_words::split(flag).unwrap_or_default() {
            full_args.push(part);
        }
    }

    let binary = binary_path.to_string();
    let timeout_duration = std::time::Duration::from_millis(timeout_ms);

    let result = tokio::time::timeout(
        timeout_duration,
        tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new(&binary)
                .args(&full_args)
                .output()
                .map_err(|e| {
                    ModuleError::new(
                        ErrorCode::ModuleExecuteError,
                        format!("Failed to execute '{}': {}", binary, e),
                    )
                })?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            Ok::<SubprocessOutput, ModuleError>(SubprocessOutput {
                stdout,
                stderr,
                exit_code,
            })
        }),
    )
    .await;

    match result {
        Ok(join_result) => {
            // INVARIANT: spawn_blocking should not panic; unwrap is safe here.
            join_result.map_err(|e| {
                ModuleError::new(
                    ErrorCode::ModuleExecuteError,
                    format!("Subprocess task panicked: {}", e),
                )
            })?
        }
        Err(_elapsed) => Err(ModuleError::new(
            ErrorCode::ModuleTimeout,
            format!("Command '{}' timed out after {}ms", binary_path, timeout_ms),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_arguments_string_value() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!("test.txt"));
        let args = build_arguments(&kwargs).unwrap();
        assert_eq!(args, vec!["--file", "test.txt"]);
    }

    #[test]
    fn test_build_arguments_boolean_true() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("all".to_string(), json!(true));
        let args = build_arguments(&kwargs).unwrap();
        assert_eq!(args, vec!["--all"]);
    }

    #[test]
    fn test_build_arguments_boolean_false() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("all".to_string(), json!(false));
        let args = build_arguments(&kwargs).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_arguments_null_skipped() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("x".to_string(), json!(null));
        let args = build_arguments(&kwargs).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_arguments_array_values() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("include".to_string(), json!(["a", "b"]));
        let args = build_arguments(&kwargs).unwrap();
        assert_eq!(args, vec!["--include", "a", "--include", "b"]);
    }

    #[test]
    fn test_build_arguments_underscore_to_hyphen() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("no_cache".to_string(), json!(true));
        let args = build_arguments(&kwargs).unwrap();
        assert_eq!(args, vec!["--no-cache"]);
    }

    #[test]
    fn test_build_arguments_integer_value() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("count".to_string(), json!(5));
        let args = build_arguments(&kwargs).unwrap();
        assert_eq!(args, vec!["--count", "5"]);
    }

    #[test]
    fn test_build_arguments_injection_blocked() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("msg".to_string(), json!("hi; rm"));
        let result = build_arguments(&kwargs);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_validate_no_injection_clean() {
        let result = validate_no_injection("file", "hello world");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_no_injection_semicolon() {
        let result = validate_no_injection("arg", "a;b");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[tokio::test]
    async fn test_execute_subprocess_echo() {
        let result = execute_subprocess("echo", &["hello".to_string()], None, 5000)
            .await
            .unwrap();
        assert_eq!(result.stdout, "hello\n");
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_execute_subprocess_false() {
        let result = execute_subprocess("false", &[], None, 5000).await.unwrap();
        assert_ne!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_execute_subprocess_nonexistent() {
        let result =
            execute_subprocess("/nonexistent_binary_that_does_not_exist", &[], None, 5000).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::ModuleExecuteError);
    }
}
