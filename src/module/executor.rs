use apcore::{ErrorCode, ModuleError};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Characters that MUST NOT appear in values taken from a binding file.
///
/// `json_flag` and `command_parts` are build-time artifacts produced by a scan,
/// not runtime data: nothing legitimate puts a shell metacharacter in them, so
/// the conservative set is kept as a tamper signal for binding files edited by
/// hand or in transit. Runtime parameter values are a different trust class and
/// go through [`validate_argument_value`] instead.
const BINDING_INJECTION_CHARS: &[char] = &[
    ';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r', '\0', '(', ')', '<', '>',
];

/// Characters rejected in *any* argument value, whatever its source.
///
/// This is deliberately tiny. Every argument reaches the child through
/// [`execute_subprocess`], which builds a `Command` and passes argv straight to
/// `execve` — no shell is spawned anywhere on the execution path (the one
/// `sh`/`zsh` invocation in the crate, in `scanner::completion`, runs a
/// hardcoded constant script with no interpolation). Shell metacharacters are
/// therefore ordinary bytes in argv: `;`, `|`, `&`, `$`, backtick and quotes
/// cannot start a command, open a pipe, or expand a variable. Rejecting them
/// bought no safety and cost the tool's core use cases — `curl` could not send
/// a JSON body (`{"a":1}`) or a form (`a=1&b=2`), and no `jq` filter containing
/// `|`, `(`, `)` or `$` could be passed at all.
///
/// What remains:
/// - `\0` — `Command` rejects it anyway; catching it here gives a better error.
/// - `\n` / `\r` — not an execve risk, but they corrupt the audit log's line
///   framing and any line-oriented protocol the wrapped tool speaks.
const CONTROL_CHARS: &[char] = &['\0', '\n', '\r'];

/// Default cap on captured stdout/stderr per stream, matching apcore-cli's
/// `Sandbox::with_max_output_bytes` default. Prevents a runaway CLI tool
/// (e.g. one that dumps a multi-GB file to stdout) from exhausting memory.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Validate a value that came from a binding file (`json_flag`,
/// `command_parts`), rejecting the conservative [`BINDING_INJECTION_CHARS`] set.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn validate_no_injection(param_name: &str, value: &str) -> Result<(), ModuleError> {
    let found: Vec<char> = value
        .chars()
        .filter(|c| BINDING_INJECTION_CHARS.contains(c))
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

/// Validate a caller-supplied parameter value before it becomes one argv entry.
///
/// Two checks, both about what a value can do *as an argument* rather than what
/// it would mean to a shell that never runs:
///
/// 1. [`CONTROL_CHARS`] are rejected outright.
/// 2. A value that starts with `-` is rejected, because the wrapped tool's own
///    parser reads argv positionally: a value of `--output=/etc/passwd` or `-K
///    /etc/shadow` is indistinguishable from an option the caller was never
///    granted. This is the injection that actually exists on an execve path,
///    and the previous shell-metacharacter blacklist did not cover it — `-` was
///    not in that set. It matters most for values that reach argv bare (a
///    positional argument) and for options whose argument is optional, where
///    the tool will happily treat the next `-`-prefixed token as a new flag.
///
/// `numeric` exempts check 2 for values that arrived as a JSON number, so a
/// legitimate negative number still works. A *string* `"-1"` is still rejected:
/// the caller asked for text, and honoring a leading `-` in text is exactly the
/// ambiguity being closed.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn validate_argument_value(
    param_name: &str,
    value: &str,
    numeric: bool,
) -> Result<(), ModuleError> {
    let found: Vec<char> = value
        .chars()
        .filter(|c| CONTROL_CHARS.contains(c))
        .collect();
    if !found.is_empty() {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Parameter '{}' contains prohibited control characters: {:?}",
                param_name, found
            ),
        ));
    }

    if !numeric && value.starts_with('-') {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Parameter '{param_name}' starts with '-', which the wrapped \
                 command would parse as an option rather than a value"
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

/// How a single input property is spelled on the command line.
enum ArgForm {
    /// Passed behind a flag token, e.g. `-l` or `--output FILE`.
    Flag(String),
    /// Passed bare, at the given index among the command's operands.
    Positional(u64),
}

/// Decide how `key` is spelled, from the `x-apexe-*` annotations the scanner
/// wrote onto its schema property.
///
/// Falls back to `--{key-with-hyphens}` when the schema says nothing. Bindings
/// generated before these annotations existed carry no such markers, and
/// silently rendering nothing for them would turn a wrong command into a
/// missing argument; the fallback keeps their prior behaviour exactly.
fn arg_form(schema: Option<&Value>, key: &str) -> ArgForm {
    let property = schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(key));

    if let Some(index) = property
        .and_then(|p| p.get("x-apexe-positional"))
        .and_then(Value::as_u64)
    {
        return ArgForm::Positional(index);
    }

    if let Some(literal) = property
        .and_then(|p| p.get("x-apexe-flag"))
        .and_then(Value::as_str)
    {
        return ArgForm::Flag(literal.to_string());
    }

    ArgForm::Flag(format!("--{}", key.replace('_', "-")))
}

/// Whether a value will actually reach the command line.
///
/// `false` and null render nothing, so naming such a key is not "sending" the
/// flag and must not trip the conflict check — `{"f": true, "i": false}` is a
/// perfectly good way to say "force, not interactive".
fn is_effective(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

/// Reject inputs that name two flags the tool cannot take together.
///
/// Without this the pair is not an error but something worse: it renders, and
/// which of the two wins is decided by the order the caller happened to write
/// the JSON keys in. An object has no ordering anyone intended as an interface,
/// so `{"f": true, "i": true}` on `rm` is not "interactive wins" — it is
/// whichever key was typed last. Failing closed turns an unpredictable
/// invocation into a message naming both flags.
///
/// Only curated overlays state mutual exclusion, so this fires only where a
/// human recorded it; a schema without `x-apexe-conflicts-with` is unaffected.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
fn reject_conflicting_flags(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
) -> Result<(), ModuleError> {
    let Some(properties) = input_schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };

    for (key, value) in kwargs {
        if !is_effective(value) {
            continue;
        }
        let Some(conflicts) = properties
            .get(key)
            .and_then(|p| p.get("x-apexe-conflicts-with"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for other in conflicts.iter().filter_map(Value::as_str) {
            // Report each pair once: the relation is declared on both flags.
            if other <= key.as_str() {
                continue;
            }
            if kwargs.get(other).is_some_and(is_effective) {
                return Err(ModuleError::new(
                    ErrorCode::GeneralInvalidInput,
                    format!(
                        "Parameters '{key}' and '{other}' cannot be used together; \
                         send one or the other"
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// Build CLI args from JSON kwargs, spelling each one the way `input_schema`
/// says the wrapped tool accepts it.
///
/// Bool `true` emits the bare flag, `false` and null are skipped, and an array
/// repeats the flag once per element. Values marked `x-apexe-positional` are
/// emitted bare, after every flag and ordered by their recorded index — an
/// input object has no ordering of its own, so without that index `cp source
/// target` would depend on the order the caller happened to write the JSON
/// keys in.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn build_arguments(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
) -> Result<Vec<String>, ModuleError> {
    reject_conflicting_flags(kwargs, input_schema)?;

    let mut flags: Vec<String> = Vec::new();
    // (index, values) -- sorted before being appended, so operands land in the
    // order the usage line declared regardless of JSON key order.
    let mut operands: Vec<(u64, Vec<String>)> = Vec::new();

    for (key, value) in kwargs {
        let form = arg_form(input_schema, key);

        // A bare operand cannot carry a boolean: there is no token to emit for
        // `true` and no way to express `false`. Skipping is the only coherent
        // reading, and matches how `false` is treated for flags.
        if let Value::Bool(enabled) = value {
            if let ArgForm::Flag(ref literal) = form {
                if *enabled {
                    flags.push(literal.clone());
                }
            }
            continue;
        }

        if value.is_null() {
            continue;
        }

        let items: Vec<&Value> = match value {
            Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };

        let mut rendered: Vec<String> = Vec::new();
        for item in items {
            let text = json_value_to_string(item);
            validate_argument_value(key, &text, item.is_number())?;
            match form {
                // A long option carries its value with `=`, as one argv entry.
                // Options whose value is *optional* accept no other spelling:
                // GNU `ls --classify never` reads `never` as a file to list and
                // fails, while `--classify=never` works. Options whose value is
                // required accept both, so `=` is the one form that is always
                // right, and the schema does not record which kind a flag is.
                // Short options are the opposite -- `-I=PATTERN` passes a value
                // that begins with `=` -- so they stay two entries.
                ArgForm::Flag(ref literal) if literal.starts_with("--") => {
                    rendered.push(format!("{literal}={text}"));
                }
                ArgForm::Flag(ref literal) => {
                    rendered.push(literal.clone());
                    rendered.push(text);
                }
                ArgForm::Positional(_) => rendered.push(text),
            }
        }

        match form {
            ArgForm::Flag(_) => flags.extend(rendered),
            ArgForm::Positional(index) => operands.push((index, rendered)),
        }
    }

    operands.sort_by_key(|(index, _)| *index);
    let mut args = flags;
    args.extend(operands.into_iter().flat_map(|(_, values)| values));
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

/// Size of the scratch buffer used to pull each chunk out of the pipe in
/// [`read_up_to`]. Small and fixed, independent of `max_len`.
const READ_CHUNK_SIZE: usize = 8 * 1024;

/// Read up to `max_len` bytes from `reader`, stopping early at EOF.
///
/// Deliberately does *not* drain the reader to EOF: a runaway process (e.g.
/// one that never stops writing) would otherwise buffer unboundedly. Once
/// the cap is hit, the pipe is left unread; the writer sees backpressure and
/// blocks, which is resolved by the outer timeout killing the child.
///
/// Grows the output buffer incrementally as bytes actually arrive instead of
/// pre-allocating `max_len` upfront — `max_len` is a 64 MiB+ cap per stream,
/// and eagerly allocating it for every subprocess call (stdout *and*
/// stderr, times however many calls run concurrently) costs real memory
/// even when the actual output is a few bytes.
async fn read_up_to<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_SIZE];
    while buf.len() < max_len {
        let to_read = READ_CHUNK_SIZE.min(max_len - buf.len());
        let n = reader.read(&mut chunk[..to_read]).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

/// Base environment variables passed through to a wrapped CLI subprocess.
///
/// A wrapped tool is invoked by an external agent over MCP/A2A, so it must not
/// inherit apexe's *full* process environment — that could hold secrets (API
/// tokens, cloud credentials) the agent should never see. The subprocess env is
/// scrubbed to this allowlist plus any `LC_*` locale variable; file-based
/// credentials under `$HOME` still work. (Passing tool-specific credential env
/// through is intended as a future opt-in config knob.)
const ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "TERM", "TZ", "TMPDIR",
];

/// Whether an environment variable is allowed through to a wrapped subprocess.
fn is_allowed_env(key: &str) -> bool {
    ENV_ALLOWLIST.contains(&key) || key.starts_with("LC_")
}

/// Execute a subprocess with a timeout, capturing stdout/stderr.
///
/// Runs the given binary with args directly (no shell), optionally appending
/// json_flag parts. The environment is scrubbed to [`ENV_ALLOWLIST`] so secrets
/// in apexe's own environment do not leak to the wrapped tool. Captured
/// stdout/stderr are each capped at `max_output_bytes` to bound memory use
/// against runaway output; stdout, stderr, and the exit status are collected
/// concurrently to avoid pipe deadlock. The child has `kill_on_drop` set, so if
/// `timeout_ms` elapses the process is actually killed rather than left running
/// as an orphan.
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
        let mut command = Command::new(binary_path);
        command
            .args(&full_args)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Re-add only allowlisted environment variables (scrub secrets).
        for (key, value) in std::env::vars() {
            if is_allowed_env(&key) {
                command.env(key, value);
            }
        }
        let mut child = command.spawn().map_err(|e| {
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
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--file=test.txt"]);
    }

    #[test]
    fn test_build_arguments_boolean_true() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("all".to_string(), json!(true));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--all"]);
    }

    #[test]
    fn test_build_arguments_boolean_false() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("all".to_string(), json!(false));
        let args = build_arguments(&kwargs, None).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_arguments_null_skipped() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("x".to_string(), json!(null));
        let args = build_arguments(&kwargs, None).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_arguments_array_values() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("include".to_string(), json!(["a", "b"]));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--include=a", "--include=b"]);
    }

    #[test]
    fn test_build_arguments_underscore_to_hyphen() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("no_cache".to_string(), json!(true));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--no-cache"]);
    }

    #[test]
    fn test_build_arguments_integer_value() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("count".to_string(), json!(5));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--count=5"]);
    }

    #[test]
    fn test_build_arguments_option_like_value_blocked() {
        // The injection that exists on an execve path: a value the wrapped
        // tool's own parser would read as an option. `--output=` would make
        // curl write to an arbitrary file.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!("--output=/etc/passwd"));
        let result = build_arguments(&kwargs, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_build_arguments_option_like_value_blocked_in_array() {
        // F2 §5.5: array elements go through the same validation as scalars.
        // The array branch re-emits `--flag item` per element, so a `-`-leading
        // element is exactly where a variadic option would absorb it as a flag.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("tags".to_string(), json!(["ok", "-K/etc/shadow"]));
        let result = build_arguments(&kwargs, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_build_arguments_allows_shell_metacharacters() {
        // No shell runs on the execution path, so these are ordinary argv
        // bytes. Rejecting them used to make curl unable to POST a JSON body
        // and jq unable to receive any filter at all.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!(r#"{"name":"apexe"}"#));
        let args = build_arguments(&kwargs, None).expect("a JSON body must be passable");
        assert_eq!(args, vec![r#"--data={"name":"apexe"}"#]);

        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!("name=apexe&lang=rust"));
        let args = build_arguments(&kwargs, None).expect("a form body must be passable");
        assert_eq!(args, vec!["--data=name=apexe&lang=rust"]);

        let mut kwargs = serde_json::Map::new();
        kwargs.insert("filter".to_string(), json!(r#".items[] | select(.n > $x)"#));
        let args = build_arguments(&kwargs, None).expect("a jq filter must be passable");
        assert_eq!(args, vec![r#"--filter=.items[] | select(.n > $x)"#]);
    }

    #[test]
    fn test_build_arguments_rejects_control_characters() {
        for bad in ["a\nb", "a\rb", "a\0b"] {
            let mut kwargs = serde_json::Map::new();
            kwargs.insert("msg".to_string(), json!(bad));
            let result = build_arguments(&kwargs, None);
            assert!(
                result.is_err(),
                "control character in {bad:?} must be rejected"
            );
            assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
        }
    }

    #[test]
    fn test_build_arguments_negative_number_allowed() {
        // A JSON number is type-evidence that the caller meant a value, so a
        // legitimate negative number survives the `-` prefix rule.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("offset".to_string(), json!(-5));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--offset=-5"]);
    }

    #[test]
    fn test_build_arguments_negative_number_as_string_blocked() {
        // ...but the same thing typed as a string is not: the caller asked for
        // text, and a leading `-` in text is the ambiguity being closed.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("offset".to_string(), json!("-5"));
        assert!(build_arguments(&kwargs, None).is_err());
    }

    /// A schema shaped like the scanner emits, for the given properties.
    fn schema_with(properties: Value) -> Value {
        json!({ "type": "object", "properties": properties })
    }

    #[test]
    fn test_build_arguments_short_flag_uses_single_dash() {
        // 45 of `ls`'s 47 properties are single-character short options. The
        // key `l` used to render as `--l`, which BSD ls rejects outright, so
        // the module could not pass any of them.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l" },
            "a": { "type": "boolean", "x-apexe-flag": "-a" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("a".to_string(), json!(true));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-l", "-a"]);
    }

    #[test]
    fn test_build_arguments_rejects_conflicting_flags() {
        // Both would render, and which one the tool honours would come down to
        // the order the caller wrote the keys in.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["1"] },
            "1": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("1".to_string(), json!(true));

        let err = build_arguments(&kwargs, Some(&schema)).unwrap_err();
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(
            err.message.contains('l') && err.message.contains('1'),
            "the error should name both flags: {}",
            err.message
        );
    }

    #[test]
    fn test_build_arguments_conflict_is_order_independent() {
        // The rejection must not depend on which key came first -- that is the
        // whole defect being closed.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["1"] },
            "1": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
        }));

        for keys in [["l", "1"], ["1", "l"]] {
            let mut kwargs = serde_json::Map::new();
            for key in keys {
                kwargs.insert(key.to_string(), json!(true));
            }
            assert!(
                build_arguments(&kwargs, Some(&schema)).is_err(),
                "order {keys:?} must be rejected too"
            );
        }
    }

    #[test]
    fn test_build_arguments_disabled_flag_is_not_a_conflict() {
        // `false` renders nothing, so naming it is not sending it -- this is a
        // legitimate way to say "long listing, not single-column".
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["1"] },
            "1": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("1".to_string(), json!(false));

        assert_eq!(build_arguments(&kwargs, Some(&schema)).unwrap(), vec!["-l"]);
    }

    #[test]
    fn test_build_arguments_without_conflict_annotations_is_unaffected() {
        // Only curated overlays state mutual exclusion; a schema that says
        // nothing must keep rendering exactly as before.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l" },
            "1": { "type": "boolean", "x-apexe-flag": "-1" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("1".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-l", "-1"]
        );
    }

    #[test]
    fn test_build_arguments_long_flag_value_uses_equals() {
        // GNU `ls --classify never` lists a file called `never` and fails;
        // only `--classify=never` works, because the option's value is
        // optional. Options with a required value accept both spellings, and
        // the schema does not say which kind a flag is, so `=` is the only
        // form that is always correct.
        let schema = schema_with(json!({
            "classify": { "type": "string", "x-apexe-flag": "--classify" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("classify".to_string(), json!("never"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["--classify=never"]);
    }

    #[test]
    fn test_build_arguments_short_flag_value_stays_separate() {
        // The mirror image: `-I=PATTERN` would pass a value beginning with `=`,
        // so short options keep the value as its own argv entry.
        let schema = schema_with(json!({
            "I": { "type": "string", "x-apexe-flag": "-I" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("I".to_string(), json!("*.tmp"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-I", "*.tmp"]);
    }

    #[test]
    fn test_build_arguments_multi_character_single_dash_flag() {
        // `find -daystart` is spelled with one dash despite being many
        // characters, so no "one character means one dash" rule recovers it.
        let schema = schema_with(json!({
            "daystart": { "type": "boolean", "x-apexe-flag": "-daystart" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("daystart".to_string(), json!(true));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-daystart"]);
    }

    #[test]
    fn test_build_arguments_positional_is_passed_bare() {
        let schema = schema_with(json!({
            "file": {
                "type": "array",
                "items": { "type": "string" },
                "x-apexe-positional": 0,
            },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!(["/tmp", "/var"]));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["/tmp", "/var"]);
    }

    #[test]
    fn test_build_arguments_positional_order_ignores_key_order() {
        // `cp source target` inverts its meaning if the two are swapped, and a
        // JSON object carries no ordering the renderer can rely on -- serde's
        // preserve_order means the caller's key order would otherwise decide.
        let schema = schema_with(json!({
            "source": { "type": "string", "x-apexe-positional": 0 },
            "target": { "type": "string", "x-apexe-positional": 1 },
        }));

        let mut kwargs = serde_json::Map::new();
        kwargs.insert("target".to_string(), json!("/dst"));
        kwargs.insert("source".to_string(), json!("/src"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["/src", "/dst"]);
    }

    #[test]
    fn test_build_arguments_flags_precede_positionals() {
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l" },
            "file": { "type": "string", "x-apexe-positional": 0 },
        }));

        // Deliberately insert the operand first.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!("/tmp"));
        kwargs.insert("l".to_string(), json!(true));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-l", "/tmp"]);
    }

    #[test]
    fn test_build_arguments_boolean_positional_is_skipped() {
        // There is no token to emit for a bare operand set to `true`, and no
        // way at all to express `false`.
        let schema = schema_with(json!({
            "file": { "type": "string", "x-apexe-positional": 0 },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!(true));

        assert!(build_arguments(&kwargs, Some(&schema)).unwrap().is_empty());
    }

    #[test]
    fn test_build_arguments_falls_back_without_schema_annotations() {
        // Bindings generated before the annotations existed must keep working
        // exactly as they did, rather than silently rendering nothing.
        let schema = schema_with(json!({ "no_cache": { "type": "boolean" } }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("no_cache".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["--no-cache"]
        );
        assert_eq!(
            build_arguments(&kwargs, None).unwrap(),
            vec!["--no-cache"],
            "no schema at all must behave the same as an unannotated one"
        );
    }

    #[test]
    fn test_build_arguments_long_flag_literal_survives_underscore() {
        // `_` -> `-` is lossy: a long flag that genuinely contains an
        // underscore cannot be recovered from the property key.
        let schema = schema_with(json!({
            "foo_bar": { "type": "string", "x-apexe-flag": "--foo_bar" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("foo_bar".to_string(), json!("v"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["--foo_bar=v"]);
    }

    #[test]
    fn test_validate_no_injection_clean() {
        let result = validate_no_injection("file", "hello world");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_no_injection_semicolon() {
        // Binding-sourced values keep the conservative set: nothing legitimate
        // puts a shell metacharacter in `json_flag` or `command_parts`, so it
        // stays useful as a tamper signal even though no shell would see it.
        let result = validate_no_injection("arg", "a;b");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_validate_argument_value_trust_classes_differ() {
        // The same string is fine as a runtime parameter and rejected as a
        // binding value -- that asymmetry is the point.
        assert!(validate_argument_value("data", "a;b|c", false).is_ok());
        assert!(validate_no_injection("json_flag", "a;b|c").is_err());
    }

    #[test]
    fn test_is_allowed_env() {
        // Base runtime vars pass; secrets do not.
        assert!(is_allowed_env("PATH"));
        assert!(is_allowed_env("HOME"));
        assert!(is_allowed_env("LC_CTYPE")); // LC_ prefix
        assert!(!is_allowed_env("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_allowed_env("GH_TOKEN"));
        assert!(!is_allowed_env("OPENAI_API_KEY"));
        assert!(!is_allowed_env("CARGO_PKG_NAME"));
    }

    #[tokio::test]
    async fn test_execute_subprocess_scrubs_environment() {
        // A wrapped subprocess must NOT inherit apexe's full environment (which
        // may hold secrets). Only allowlisted vars (plus a few sh injects) reach
        // it; PATH must survive so tools can still run.
        let out = execute_subprocess(
            "sh",
            &["-c".to_string(), "env".to_string()],
            None,
            5_000,
            DEFAULT_MAX_OUTPUT_BYTES,
        )
        .await
        .unwrap();

        assert!(
            out.stdout.lines().any(|l| l.starts_with("PATH=")),
            "PATH should pass through: {}",
            out.stdout
        );
        // The test process itself has many vars (cargo sets numerous CARGO_*);
        // a scrubbed child stays tiny, proving it did not inherit them.
        let count = out.stdout.lines().count();
        assert!(
            count <= 20,
            "environment was not scrubbed, child sees {count} vars: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("CARGO_"),
            "cargo/apexe env leaked to the wrapped subprocess: {}",
            out.stdout
        );
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

    #[tokio::test]
    async fn test_read_up_to_does_not_eagerly_allocate_full_cap() {
        // Regression for the CRITICAL finding: read_up_to used to
        // `vec![0u8; max_len]` upfront, so a tiny actual output still cost a
        // full max_len allocation (x2 for stdout+stderr, x concurrency).
        // A capped-but-small actual output must not grow the buffer anywhere
        // near the cap.
        let data = b"tiny output";
        let mut cursor = std::io::Cursor::new(data.to_vec());
        let huge_cap = 64 * 1024 * 1024;
        let result = read_up_to(&mut cursor, huge_cap).await.unwrap();
        assert_eq!(result, data);
        assert!(
            result.capacity() < 1024 * 1024,
            "capacity {} should stay proportional to the {}-byte output, not the {}-byte cap",
            result.capacity(),
            data.len(),
            huge_cap
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
