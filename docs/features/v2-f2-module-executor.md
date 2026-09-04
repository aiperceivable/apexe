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

use crate::governance::AuditManager;

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
        audit: Option<Arc<AuditManager>>,
    ) -> Self;
}
```

### 2.4 Module Trait Implementation

**Signatures below match apcore 0.27's `Module` trait, not the draft this
section was first written against: `input_schema`/`output_schema` return
`Value` directly (not `Option<Value>`), and `execute`'s parameters are
`(inputs: Value, ctx: &Context<Value>)` — value first, context second, and
`Context` is generic over the same `Value` payload type rather than a
separate `SharedData`. There is no `preflight` method (§3.4).**

```rust
#[async_trait::async_trait]
impl Module for CliModule {
    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn output_schema(&self) -> Value {
        self.output_schema.clone()
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// Execute the CLI command with the given input.
    ///
    /// Steps:
    /// 1. Build argv from `inputs` per `input_schema`'s `x-apexe-*`
    ///    placement annotations (§3.1) — returns without spawning anything
    ///    on a rejected value.
    /// 2. Spawn the subprocess via `execute_subprocess` (§3.2).
    /// 3. Parse subprocess output into the result JSON (§3.3).
    /// 4. Log the execution (or the refusal, on the early-return path) if
    ///    an `AuditManager` is configured.
    /// 5. Return the result or a `ModuleError`.
    async fn execute(&self, inputs: Value, ctx: &Context<Value>) -> Result<Value, ModuleError>;
}
```

---

## 3. Execution Logic

### 3.1 Argument Building

**Design changed since this section was first written: no shell is ever
invoked (argv is built as a `Vec<String>` and passed straight to `execve`),
so a "shell injection characters" blocklist is not the guard — shell
metacharacters (`;|&$\`'"`) are inert data to a subprocess spawned without a
shell. The real guard rejects a value that would be parsed as an *option* by
the wrapped binary (starts with `-`, unless the schema's
`x-apexe-end-of-options` says the tool honours `--`) plus a small set of
control characters. There are two distinct validators, for two distinct
trust classes:**

```rust
// src/module/executor.rs

/// Characters rejected in a value sourced from a BINDING FILE — a tamper
/// signal for a `.binding.yaml` edited by hand or in transit, not a runtime
/// argument guard.
const BINDING_INJECTION_CHARS: &[char] =
    &[';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r', '\0', '(', ')', '<', '>'];
pub fn validate_no_injection(param_name: &str, value: &str) -> Result<(), ModuleError>;

/// Characters rejected in *any* runtime argument value, whatever its source.
/// Deliberately tiny: argv reaches the child via `execve`, with no shell in
/// between, so there is no metacharacter to guard against here.
const CONTROL_CHARS: &[char] = &['\0', '\n', '\r', '\u{2028}', '\u{2029}', '\u{0085}'];
pub fn validate_argument_value(
    location: ValueLocation,
    value: &str,
    numeric: bool,
    separator_available: bool,
) -> Result<(), ModuleError>;

/// Build CLI args from JSON kwargs, ordered per `input_schema`'s
/// `x-apexe-*` placement annotations, then flatten. See `build_argv` in the
/// same file for the subcommand-aware form this wraps.
pub fn build_arguments(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
) -> Result<Vec<String>, ModuleError>;
```

### 3.2 Subprocess Execution

```rust
// src/module/executor.rs

/// Execute a CLI subprocess and return raw output, env-scrubbed to a base
/// allowlist, output capped at `max_output_bytes` per stream, killed on
/// timeout (`kill_on_drop`).
pub async fn execute_subprocess(
    binary_path: &str,
    args: &[String],
    json_flag: Option<&str>,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<SubprocessOutput, ModuleError>;

/// Raw subprocess output.
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
```

**Key changes from v0.1.x**:
- Now uses `tokio::process::Command` (fully async) instead of synchronous `std::process::Command::output()`. **Not `spawn_blocking`** — an earlier draft of this section said so, but the subprocess is driven by the async runtime directly, with `kill_on_drop(true)` so an expired timeout actually terminates the child.
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

### 3.4 Argument Validation

**There is no separate `preflight()` step or method — that design was
superseded during implementation.** Validation is inline in
`build_argv`/`collect_argv_groups` (§3.1): every scalar value and every
array element is passed through `validate_argument_value` (control
characters, leading-`-`) as it is rendered into a token, before
`execute_subprocess` ever spawns the child. `CliModule::execute` calls
`build_argv` first and returns its `Err` without spawning anything, which is
what gives the same effect preflight validation was meant to have — no
subprocess starts on a rejected input — without a distinct method.

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
    let stripped = target.strip_prefix("exec://").ok_or_else(|| {
        ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!("Invalid target format: {target}"),
        )
    })?;

    let parts: Vec<&str> = stripped.split_whitespace().collect();
    if parts.is_empty() {
        return Err(ModuleError::new(ErrorCode::GeneralInvalidInput, "Empty target"));
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
| `test_cli_module_from_scanned_invalid_target` | target = "invalid" | Err(ModuleError) with `GeneralInvalidInput` (apcore 0.27 renamed `ValidationFailed`) |
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
| `test_build_arguments_allows_shell_metacharacters` | `{"msg": "hi; rm"}` | `["--msg", "hi; rm"]` — no shell is invoked, so this is inert data, not injection (see §3.1) |
| `test_build_argv_reports_global_flags_before_the_subcommand` | flag with `x-apexe-flag-position: before-subcommand` | flag token ordered before the subcommand tokens |

### 5.4 Execution Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_execute_echo_returns_stdout` | Execute `echo hello` | stdout contains "hello", exit_code = 0 |
| `test_execute_false_nonzero_exit` | Execute `false` | exit_code != 0 |
| `test_execute_json_output_parsed` | Echo valid JSON with json_flag | json_output key present |
| `test_execute_timeout_returns_error` | Command that hangs, timeout 1ms | Err with `ModuleTimeout` error code |
| `test_execute_nonexistent_binary` | Binary = "/nonexistent" | Err with `ModuleExecuteError` |

### 5.5 Argument Validation Tests

**No separate `preflight()` method exists (§3.4) — these scenarios are
covered inline by `build_argv`'s per-value `validate_argument_value` calls.**

| Test Name | Scenario | Expected |
|---|---|---|
| `test_validate_argument_value_rejects_control_characters` | value containing `\0`/`\n`/`\r` | Err(`GeneralInvalidInput`) |
| `test_validate_argument_value_rejects_a_leading_dash` | `{"arg": "-x"}`, no `--` support declared | Err(`GeneralInvalidInput`) — would be parsed as an option |
| `test_validate_argument_value_trust_classes_differ` | same value via a runtime argument vs. a binding-file field | different validators, different rejected character sets (§3.1) |
| `test_build_arguments_allows_shell_metacharacters` | `{"arg": "a;b"}`, `{"arg": "a|b"}` | passed through verbatim — no shell, so not injection (§3.1) |

---

## 6. Migration from v0.1.x

### Code Preserved

The following logic is extracted from `src/executor/mod.rs` into `src/module/executor.rs` — **`SHELL_INJECTION_CHARS` itself was not preserved as such; see §3.1 for the two validators that replaced its single "block shell metacharacters" premise**:
- `validate_no_injection()` — kept, but narrowed to binding-file-sourced values only (`BINDING_INJECTION_CHARS`), not runtime arguments
- `json_value_to_string()` function
- Argument building loop from `execute_cli()`, now `build_argv`/`collect_argv_groups`

### Code Changed

- `execute_cli()` is split into `build_arguments()` + `execute_subprocess()`.
- Timeout is now enforced via `tokio::time::timeout` (was ignored in v0.1.x).
- Error types change from `ApexeError` to `ModuleError` (uses F6 conversions).
- Subprocess runs on `tokio::process::Command` (was synchronous). No `spawn_blocking` is involved.

### Code Deleted

- `src/executor/mod.rs` is deleted entirely. Its logic lives in `src/module/executor.rs` and `src/module/cli_module.rs`.

---

## 7. Thread Safety

`CliModule` is `Send + Sync` because:
- All fields are either owned values or `Arc`-wrapped.
- `execute()` is async and drives the subprocess through `tokio::process`, which needs no blocking pool.
- No interior mutability (`&self` only in all methods).

This is required for registration in apcore's `Registry` and use in async handlers.
