# F2: Module Executor -- apcore Module Trait for CLI Subprocess Execution

| Field | Value |
|---|---|
| **Feature ID** | F2 |
| **Tech Design Section** | 5.6 |
| **Priority** | P0 (Core) |
| **Dependencies** | F1 (Scanner Adapter), F6 (Error Migration) |
| **Depended On By** | F4 (MCP Server), F5 (Governance) |
| **New Files** | `src/module/mod.rs`, `src/module/cli_module.rs`, `src/module/executor.rs` |
| **Deleted Files** | `src/executor/mod.rs` (absorbed) |
| **Estimated LOC** | ~500 |
| **Estimated Tests** | ~25 |

---

## 1. Purpose

Implement the apcore `Module` trait for CLI subprocess execution. Each scanned CLI command becomes a `CliModule` that can be registered in an apcore `Registry`, executed through an apcore `Executor`, and participate in middleware chains. This is the central integration point that connects apexe's scanning output to the apcore runtime.

---

## 2. Module Structure

### 2.1 `src/module/mod.rs`

```rust
pub mod cli_module;
pub mod executor;

pub use cli_module::CliModule;
```

### 2.2 `src/module/cli_module.rs` -- CliModule

```rust
use std::sync::Arc;
use apcore::{Context, Module, ModuleAnnotations, ModuleError, SharedData};
use apcore_toolkit::ScannedModule;
use serde_json::Value;

use crate::governance::{AuditManager, SandboxManager};

/// An apcore Module implementation that executes a CLI command as a subprocess.
pub struct CliModule {
    /// Unique module identifier (e.g., "cli.git.commit").
    module_id: String,
    /// Human-readable description.
    description: String,
    /// JSON Schema for valid inputs.
    input_schema: Value,
    /// JSON Schema for expected outputs.
    output_schema: Value,
    /// Module behavioral annotations.
    annotations: ModuleAnnotations,
    /// Absolute path to the CLI binary.
    binary_path: String,
    /// Command parts after the binary (e.g., ["container", "ls"] for docker).
    command_parts: Vec<String>,
    /// Flag to enable structured JSON output (e.g., "--format json").
    json_flag: Option<String>,
    /// Subprocess timeout in milliseconds.
    timeout_ms: u64,
    /// Optional sandbox for subprocess isolation.
    sandbox: Option<Arc<SandboxManager>>,
    /// Optional audit logger.
    audit: Option<Arc<AuditManager>>,
}
```

### 2.3 Construction Methods

```rust
impl CliModule {
    /// Create a CliModule from a ScannedModule and runtime dependencies.
    ///
    /// Parses the `target` field (format: "exec://{binary_path} {command_parts}")
    /// and extracts the json_flag from metadata.
    pub fn from_scanned(
        module: &ScannedModule,
        timeout_ms: u64,
        sandbox: Option<Arc<SandboxManager>>,
        audit: Option<Arc<AuditManager>>,
    ) -> Result<Self, ModuleError>;

    /// Create a CliModule directly with all parameters.
    pub fn new(
        module_id: String,
        description: String,
        input_schema: Value,
        output_schema: Value,
        annotations: ModuleAnnotations,
        binary_path: String,
        command_parts: Vec<String>,
        json_flag: Option<String>,
        timeout_ms: u64,
        sandbox: Option<Arc<SandboxManager>>,
        audit: Option<Arc<AuditManager>>,
    ) -> Self;
}
```

### 2.4 Module Trait Implementation

```rust
#[async_trait::async_trait]
impl Module for CliModule {
    /// Execute the CLI command with the given input.
    ///
    /// Steps:
    /// 1. Extract trace_id from Context for correlation.
    /// 2. Build command arguments from input JSON (see Section 3.1).
    /// 3. If sandbox is enabled, delegate to SandboxManager.
    /// 4. Otherwise, spawn_blocking for subprocess execution (see Section 3.2).
    /// 5. Parse subprocess output into JSON result (see Section 3.3).
    /// 6. If audit is enabled, log the execution.
    /// 7. Return result or ModuleError.
    async fn execute(
        &self,
        ctx: &Context<SharedData>,
        input: Value,
    ) -> Result<Value, ModuleError>;

    /// Return the input JSON Schema.
    fn input_schema(&self) -> Option<Value> {
        Some(self.input_schema.clone())
    }

    /// Return the output JSON Schema.
    fn output_schema(&self) -> Option<Value> {
        Some(self.output_schema.clone())
    }

    /// Return the module description.
    fn description(&self) -> &str {
        &self.description
    }

    /// Pre-execution validation: check for shell injection in input values.
    ///
    /// Returns Err(ModuleError) with ErrorCode::ValidationFailed if
    /// any input value contains shell metacharacters.
    fn preflight(&self, input: &Value) -> Result<(), ModuleError>;
}
```

---

## 3. Execution Logic

### 3.1 Argument Building

Extracted from current `src/executor/mod.rs` `execute_cli()` function.

```rust
// src/module/executor.rs

/// Characters prohibited in command arguments to prevent shell injection.
const SHELL_INJECTION_CHARS: &[char] = &[';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r'];

/// Build a Vec<String> of command-line arguments from JSON input.
///
/// Rules (preserved from v0.1.x):
/// - Null values: skipped
/// - Boolean true: append --{key} (underscores become hyphens)
/// - Boolean false: omit
/// - Array values: append --{key} {item} for each item
/// - Other values: append --{key} {value}
///
/// All string values are validated against SHELL_INJECTION_CHARS.
pub fn build_arguments(
    kwargs: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, ModuleError>;

/// Validate a single string value contains no shell injection characters.
pub fn validate_no_injection(param_name: &str, value: &str) -> Result<(), ModuleError>;

/// Convert a JSON value to its string representation for command arguments.
fn json_value_to_string(value: &Value) -> String;
```

### 3.2 Subprocess Execution

```rust
// src/module/executor.rs

/// Execute a CLI subprocess and return raw output.
///
/// Uses tokio::task::spawn_blocking to avoid blocking the async executor.
/// Applies timeout via tokio::time::timeout.
pub async fn execute_subprocess(
    binary_path: &str,
    args: &[String],
    json_flag: Option<&str>,
    working_dir: Option<&str>,
    timeout_ms: u64,
) -> Result<SubprocessOutput, ModuleError>;

/// Raw subprocess output.
pub struct SubprocessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
```

**Key changes from v0.1.x**:
- Now uses `tokio::task::spawn_blocking` instead of synchronous `Command::output()`.
- Now uses `tokio::time::timeout` instead of ignoring the `_apexe_timeout` parameter.
- Returns `ModuleError` instead of `ApexeError`.

### 3.3 Output Parsing

```rust
// Inside CliModule::execute()

fn parse_output(output: SubprocessOutput, json_flag: &Option<String>) -> Value {
    let mut result = serde_json::Map::new();
    result.insert("stdout".into(), Value::String(output.stdout.clone()));
    result.insert("stderr".into(), Value::String(output.stderr));
    result.insert("exit_code".into(), Value::Number(output.exit_code.into()));

    // Attempt JSON parsing if json_flag was set
    if json_flag.is_some() && !output.stdout.trim().is_empty() {
        if let Ok(parsed) = serde_json::from_str::<Value>(&output.stdout) {
            result.insert("json_output".into(), parsed);
        }
    }

    Value::Object(result)
}
```

### 3.4 Preflight Validation

```rust
// Inside CliModule::preflight()

fn preflight(&self, input: &Value) -> Result<(), ModuleError> {
    if let Value::Object(map) = input {
        for (key, value) in map {
            match value {
                Value::String(s) => validate_no_injection(key, s)?,
                Value::Array(items) => {
                    for item in items {
                        if let Value::String(s) = item {
                            validate_no_injection(key, s)?;
                        }
                    }
                }
                _ => {} // Non-string values cannot contain injection
            }
        }
    }
    Ok(())
}
```

---

## 4. Target Field Parsing

The `ScannedModule.target` field encodes the binary path and command:

```
Format: exec://{binary_path} {command_part_1} {command_part_2} ...
Example: exec:///usr/bin/git commit
Example: exec:///usr/bin/docker container ls
Example: exec:///usr/local/bin/ffmpeg
```

Parsing logic in `CliModule::from_scanned()`:

```rust
fn parse_target(target: &str) -> Result<(String, Vec<String>), ModuleError> {
    let stripped = target.strip_prefix("exec://")
        .ok_or_else(|| ModuleError {
            code: ErrorCode::ValidationFailed,
            message: format!("Invalid target format: {}", target),
            ..Default::default()
        })?;

    let parts: Vec<&str> = stripped.split_whitespace().collect();
    if parts.is_empty() {
        return Err(ModuleError { code: ErrorCode::ValidationFailed, .. });
    }

    let binary_path = parts[0].to_string();
    let command_parts = parts[1..].iter().map(|s| s.to_string()).collect();
    Ok((binary_path, command_parts))
}
```

---

## 5. Test Scenarios

### 5.1 Construction Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_cli_module_from_scanned_basic` | Valid ScannedModule | CliModule created with correct fields |
| `test_cli_module_from_scanned_no_json_flag` | Module without json_flag metadata | json_flag = None |
| `test_cli_module_from_scanned_invalid_target` | target = "invalid" | Err(ModuleError) with ValidationFailed |
| `test_cli_module_from_scanned_empty_target` | target = "exec://" | Err(ModuleError) |
| `test_cli_module_new_direct` | All parameters provided | Fields match inputs |

### 5.2 Trait Method Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_cli_module_input_schema_returns_some` | Module with schema | Some(schema) |
| `test_cli_module_output_schema_returns_some` | Module with schema | Some(schema) |
| `test_cli_module_description_returns_string` | Module with desc | Non-empty string |

### 5.3 Argument Building Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_build_arguments_string_value` | `{"file": "test.txt"}` | `["--file", "test.txt"]` |
| `test_build_arguments_boolean_true` | `{"all": true}` | `["--all"]` |
| `test_build_arguments_boolean_false` | `{"all": false}` | `[]` (omitted) |
| `test_build_arguments_null_skipped` | `{"x": null}` | `[]` |
| `test_build_arguments_array_values` | `{"include": ["a","b"]}` | `["--include", "a", "--include", "b"]` |
| `test_build_arguments_underscore_to_hyphen` | `{"no_cache": true}` | `["--no-cache"]` |
| `test_build_arguments_integer_value` | `{"count": 5}` | `["--count", "5"]` |
| `test_build_arguments_injection_blocked` | `{"msg": "hi; rm"}` | Err(ModuleError) |

### 5.4 Execution Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_execute_echo_returns_stdout` | Execute `echo hello` | stdout contains "hello", exit_code = 0 |
| `test_execute_false_nonzero_exit` | Execute `false` | exit_code != 0 |
| `test_execute_json_output_parsed` | Echo valid JSON with json_flag | json_output key present |
| `test_execute_timeout_returns_error` | Command that hangs, timeout 1ms | Err with Timeout error code |
| `test_execute_nonexistent_binary` | Binary = "/nonexistent" | Err with InternalError |

### 5.5 Preflight Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_preflight_clean_input_passes` | `{"file": "/path/to/file"}` | Ok(()) |
| `test_preflight_injection_semicolon` | `{"arg": "a;b"}` | Err(ValidationFailed) |
| `test_preflight_injection_pipe` | `{"arg": "a|b"}` | Err(ValidationFailed) |
| `test_preflight_injection_in_array` | `{"args": ["ok", "bad$"]}` | Err(ValidationFailed) |
| `test_preflight_non_string_passes` | `{"count": 5}` | Ok(()) |

---

## 6. Migration from v0.1.x

### Code Preserved

The following logic is extracted from `src/executor/mod.rs` into `src/module/executor.rs`:
- `SHELL_INJECTION_CHARS` constant
- `validate_no_injection()` function
- `json_value_to_string()` function
- Argument building loop from `execute_cli()`

### Code Changed

- `execute_cli()` is split into `build_arguments()` + `execute_subprocess()`.
- Timeout is now enforced via `tokio::time::timeout` (was ignored in v0.1.x).
- Error types change from `ApexeError` to `ModuleError` (uses F6 conversions).
- Subprocess runs via `tokio::task::spawn_blocking` (was synchronous).

### Code Deleted

- `src/executor/mod.rs` is deleted entirely. Its logic lives in `src/module/executor.rs` and `src/module/cli_module.rs`.

---

## 7. Thread Safety

`CliModule` is `Send + Sync` because:
- All fields are either owned values or `Arc`-wrapped.
- `execute()` is async and uses `spawn_blocking` for the subprocess call.
- No interior mutability (`&self` only in all methods).

This is required for registration in apcore's `Registry` and use in async handlers.
