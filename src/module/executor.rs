use apcore::{ErrorCode, ModuleError};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Characters that MUST NOT appear in command arguments to prevent injection.
/// Includes shell metacharacters, quotes, null bytes, and redirection operators.
const SHELL_INJECTION_CHARS: &[char] = &[
    ';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r', '\0', '(', ')', '<', '>',
];

/// Default cap on captured stdout/stderr per stream, matching apcore-cli's
/// `Sandbox::with_max_output_bytes` default. Prevents a runaway CLI tool
/// (e.g. one that dumps a multi-GB file to stdout) from exhausting memory.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

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
    /// `true` if `stdout` was cut short at `max_output_bytes`.
    pub stdout_truncated: bool,
    /// `true` if `stderr` was cut short at `max_output_bytes`.
    pub stderr_truncated: bool,
}

/// Truncate `bytes` to at most `max_len` bytes on a UTF-8 char boundary,
/// returning the decoded string and whether truncation occurred.
fn truncate_output(bytes: &[u8], max_len: usize) -> (String, bool) {
    if bytes.len() <= max_len {
        return (String::from_utf8_lossy(bytes).to_string(), false);
    }
    // Back up off a UTF-8 continuation byte (top two bits `10`) so the cut
    // lands on a char boundary; `bytes` has no `str::is_char_boundary`.
    let mut cut = max_len;
    while cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
        cut -= 1;
    }
    (String::from_utf8_lossy(&bytes[..cut]).to_string(), true)
}

/// Read up to `max_len` bytes from `reader`, stopping early at EOF.
///
/// Deliberately does *not* drain the reader to EOF: a runaway process (e.g.
/// one that never stops writing) would otherwise buffer unboundedly. Once
/// the cap is hit, the pipe is left unread; the writer sees backpressure and
/// blocks, which is resolved by the outer timeout killing the child.
async fn read_up_to<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; max_len];
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Execute a subprocess with a timeout, capturing stdout/stderr.
///
/// Runs the given binary with args directly (no shell), optionally appending
/// json_flag parts. Captured stdout/stderr are each capped at
/// `max_output_bytes` to bound memory use against runaway output; stdout,
/// stderr, and the exit status are collected concurrently to avoid pipe
/// deadlock. The child has `kill_on_drop` set, so if `timeout_ms` elapses
/// the process is actually killed rather than left running as an orphan.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub async fn execute_subprocess(
    binary_path: &str,
    args: &[String],
    json_flag: Option<&str>,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<SubprocessOutput, ModuleError> {
    let mut full_args: Vec<String> = args.to_vec();
    if let Some(flag) = json_flag {
        for part in shell_words::split(flag).unwrap_or_default() {
            full_args.push(part);
        }
    }

    let timeout_duration = std::time::Duration::from_millis(timeout_ms);

    let run = async {
        let mut child = Command::new(binary_path)
            .args(&full_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ModuleError::new(
                    ErrorCode::ModuleExecuteError,
                    format!("Failed to execute '{}': {}", binary_path, e),
                )
            })?;

        // Read one extra byte past the cap so truncation can be detected,
        // and drive stdout/stderr/wait concurrently: reading each stream
        // sequentially while the other stays unread can deadlock the child
        // once its pipe buffer fills.
        let read_cap = max_output_bytes.saturating_add(1);
        let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
        let (stdout_bytes, stderr_bytes, status) = tokio::join!(
            read_up_to(&mut stdout_pipe, read_cap),
            read_up_to(&mut stderr_pipe, read_cap),
            child.wait(),
        );

        let stdout_bytes = stdout_bytes.map_err(|e| {
            ModuleError::new(
                ErrorCode::ModuleExecuteError,
                format!("Failed to read stdout of '{binary_path}': {e}"),
            )
        })?;
        let stderr_bytes = stderr_bytes.map_err(|e| {
            ModuleError::new(
                ErrorCode::ModuleExecuteError,
                format!("Failed to read stderr of '{binary_path}': {e}"),
            )
        })?;
        let status = status.map_err(|e| {
            ModuleError::new(
                ErrorCode::ModuleExecuteError,
                format!("Failed to wait on '{binary_path}': {e}"),
            )
        })?;

        let (stdout, stdout_truncated) = truncate_output(&stdout_bytes, max_output_bytes);
        let (stderr, stderr_truncated) = truncate_output(&stderr_bytes, max_output_bytes);

        Ok::<SubprocessOutput, ModuleError>(SubprocessOutput {
            stdout,
            stderr,
            exit_code: status.code().unwrap_or(-1),
            stdout_truncated,
            stderr_truncated,
        })
    };

    match tokio::time::timeout(timeout_duration, run).await {
        Ok(result) => result,
        // `retryable` is deliberately left unset here: a killed process may
        // have partially completed a non-idempotent side effect (e.g. `rm
        // -rf` deleted some files before timing out), so whether a retry is
        // *safe* depends on the module's `idempotent` annotation, which this
        // function has no visibility into. `CliModule::execute` sets it.
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
        let result = execute_subprocess(
            "echo",
            &["hello".to_string()],
            None,
            5000,
            DEFAULT_MAX_OUTPUT_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(result.stdout, "hello\n");
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout_truncated);
        assert!(!result.stderr_truncated);
    }

    #[tokio::test]
    async fn test_execute_subprocess_false() {
        let result = execute_subprocess("false", &[], None, 5000, DEFAULT_MAX_OUTPUT_BYTES)
            .await
            .unwrap();
        assert_ne!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_execute_subprocess_nonexistent() {
        let result = execute_subprocess(
            "/nonexistent_binary_that_does_not_exist",
            &[],
            None,
            5000,
            DEFAULT_MAX_OUTPUT_BYTES,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::ModuleExecuteError);
    }

    #[tokio::test]
    async fn test_execute_subprocess_timeout_leaves_retryable_unset() {
        // Retryability depends on the module's `idempotent` annotation,
        // which this layer doesn't have; `CliModule::execute` decides it.
        let result = execute_subprocess("sleep", &["1".to_string()], None, 10, 1024).await;
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::ModuleTimeout);
        assert_eq!(err.retryable, None);
    }

    #[tokio::test]
    async fn test_execute_subprocess_truncates_large_output() {
        // `seq 1 500` terminates on its own (unlike `yes`, which never stops)
        // and its ~2KB of output comfortably fits in the OS pipe buffer, so
        // the process can finish writing (and exit) even though we stop
        // reading well before EOF -- avoiding the write-blocks-forever
        // deadlock a larger range would hit against the 64-byte cap.
        let result = execute_subprocess(
            "seq",
            &["1".to_string(), "500".to_string()],
            None,
            5000,
            /* max_output_bytes */ 64,
        )
        .await
        .unwrap();
        assert!(result.stdout.len() <= 64);
        assert!(result.stdout_truncated);
    }

    #[tokio::test]
    async fn test_execute_subprocess_kills_hung_process_on_timeout() {
        // `sleep 10` outlives the 20ms timeout; kill_on_drop must actually
        // terminate it (verified indirectly: the call returns promptly
        // instead of blocking for the full 10s).
        let start = std::time::Instant::now();
        let result = execute_subprocess("sleep", &["10".to_string()], None, 20, 1024).await;
        assert!(result.is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "execute_subprocess should return promptly once the timeout elapses"
        );
    }

    #[test]
    fn test_truncate_output_under_limit() {
        let (s, truncated) = truncate_output(b"hello", 100);
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_output_over_limit() {
        let (s, truncated) = truncate_output(b"hello world", 5);
        assert_eq!(s, "hello");
        assert!(truncated);
    }

    #[test]
    fn test_truncate_output_respects_utf8_boundary() {
        // "héllo" - 'é' is 2 bytes (0xC3 0xA9); cutting at byte 2 would split it.
        let bytes = "héllo".as_bytes();
        let (s, truncated) = truncate_output(bytes, 2);
        assert!(truncated);
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(s, "h");
    }
}
