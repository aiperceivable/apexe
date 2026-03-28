# F6: Error Migration -- Migrate to apcore ModuleError

| Field | Value |
|---|---|
| **Feature ID** | F6 |
| **Tech Design Section** | 5.7 |
| **Priority** | P0 (Foundation) |
| **Dependencies** | None |
| **Depended On By** | F1 (Scanner Adapter), F2 (Module Executor) |
| **Modified Files** | `src/errors.rs` |
| **Deleted Files** | None |
| **Estimated LOC** | +80 (modification) |
| **Estimated Tests** | ~12 (modify existing + add conversion tests) |

---

## 1. Purpose

Add a `From<ApexeError> for ModuleError` conversion so that apexe's internal scanner errors can propagate through apcore's error system. The existing `ApexeError` enum is preserved for scanner-internal use. The conversion layer maps each variant to the appropriate `ErrorCode` with structured details and AI guidance.

---

## 2. Design Decision: Keep ApexeError

`ApexeError` is NOT deleted. Rationale:
- The scanner engine (138 tests) uses `ApexeError` throughout. Changing it would require modifying all scanner code.
- `ApexeError` has CLI-specific semantics (tool not found on PATH, command injection) that are richer than generic `ModuleError`.
- The `From` trait provides zero-cost conversion at module boundaries.

The boundary rule: **scanner code produces `ApexeError`, module/output/governance code consumes `ModuleError`**. The `?` operator handles conversion automatically.

---

## 3. ErrorCode Mapping

| ApexeError Variant | ErrorCode | retryable | ai_guidance |
|---|---|---|---|
| `ToolNotFound { tool_name }` | `ModuleNotFound` | false | "The tool '{tool_name}' is not installed. Install it and try again." |
| `ScanError(msg)` | `InternalError` | false | "An internal scanning error occurred: {msg}" |
| `ScanTimeout { command, timeout }` | `Timeout` | true | "The command took too long. Try with simpler arguments or increase timeout." |
| `ScanPermission { command }` | `Unauthorized` | false | "Permission denied. Check file permissions or run with appropriate privileges." |
| `CommandInjection { param_name, chars }` | `ValidationFailed` | false | "Remove shell metacharacters ({chars:?}) from parameter '{param_name}'." |
| `ParseError(msg)` | `InternalError` | false | "Help text parsing failed: {msg}. The tool may use a non-standard help format." |
| `Io(err)` | `InternalError` | false | "I/O error: {err}" |
| `Yaml(err)` | `SerializationError` | false | "YAML processing error: {err}" |
| `Json(err)` | `SerializationError` | false | "JSON processing error: {err}" |

---

## 4. Implementation

### 4.1 From Trait Implementation

```rust
// src/errors.rs (additions)

use apcore::{ErrorCode, ModuleError};

impl From<ApexeError> for ModuleError {
    fn from(err: ApexeError) -> ModuleError {
        match err {
            ApexeError::ToolNotFound { ref tool_name } => ModuleError {
                code: ErrorCode::ModuleNotFound,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "tool_name": tool_name,
                })),
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!(
                    "The tool '{}' is not installed. Install it and try again.",
                    tool_name
                )),
            },

            ApexeError::ScanError(ref msg) => ModuleError {
                code: ErrorCode::InternalError,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "scan_error": msg,
                })),
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!(
                    "An internal scanning error occurred: {}",
                    msg
                )),
            },

            ApexeError::ScanTimeout { ref command, timeout } => ModuleError {
                code: ErrorCode::Timeout,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "command": command,
                    "timeout_seconds": timeout,
                })),
                trace_id: None,
                retryable: true,
                ai_guidance: Some(
                    "The command took too long. Try with simpler arguments or increase timeout."
                        .into(),
                ),
            },

            ApexeError::ScanPermission { ref command } => ModuleError {
                code: ErrorCode::Unauthorized,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "command": command,
                })),
                trace_id: None,
                retryable: false,
                ai_guidance: Some(
                    "Permission denied. Check file permissions or run with appropriate privileges."
                        .into(),
                ),
            },

            ApexeError::CommandInjection {
                ref param_name,
                ref chars,
            } => ModuleError {
                code: ErrorCode::ValidationFailed,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "param_name": param_name,
                    "prohibited_chars": chars.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                })),
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!(
                    "Remove shell metacharacters ({:?}) from parameter '{}'.",
                    chars, param_name
                )),
            },

            ApexeError::ParseError(ref msg) => ModuleError {
                code: ErrorCode::InternalError,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "parse_error": msg,
                })),
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!(
                    "Help text parsing failed: {}. The tool may use a non-standard help format.",
                    msg
                )),
            },

            ApexeError::Io(ref e) => ModuleError {
                code: ErrorCode::InternalError,
                message: err.to_string(),
                details: Some(serde_json::json!({
                    "io_error_kind": format!("{:?}", e.kind()),
                })),
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!("I/O error: {}", e)),
            },

            ApexeError::Yaml(_) => ModuleError {
                code: ErrorCode::SerializationError,
                message: err.to_string(),
                details: None,
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!("YAML processing error: {}", err)),
            },

            ApexeError::Json(_) => ModuleError {
                code: ErrorCode::SerializationError,
                message: err.to_string(),
                details: None,
                trace_id: None,
                retryable: false,
                ai_guidance: Some(format!("JSON processing error: {}", err)),
            },
        }
    }
}
```

### 4.2 Convenience Constructor

```rust
// src/errors.rs (additions)

impl ApexeError {
    /// Convert to ModuleError with an attached trace_id.
    pub fn into_module_error_with_trace(self, trace_id: String) -> ModuleError {
        let mut err: ModuleError = self.into();
        err.trace_id = Some(trace_id);
        err
    }
}
```

---

## 5. Usage Pattern

### Before (v0.1.x)

```rust
// In binding generator
fn generate(&self, tool: &ScannedCLITool) -> Result<GeneratedBindingFile, ApexeError> {
    // ...
}
```

### After (v0.2.0)

```rust
// In adapter (uses ApexeError internally, converts at boundary)
fn convert(&self, tool: &ScannedCLITool) -> Result<Vec<ScannedModule>, ModuleError> {
    let scanned = self.scanner.scan(tool)?; // ApexeError auto-converts via From
    // ...
}
```

The `?` operator triggers `From<ApexeError> for ModuleError` automatically at the boundary between scanner code and module/output code.

---

## 6. Test Scenarios

### 6.1 Existing Tests (Modified)

The 10 existing `ApexeError` tests in `src/errors.rs` remain unchanged. They test the `Display` trait output which is preserved.

### 6.2 New Conversion Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_tool_not_found_to_module_error` | Convert ToolNotFound | code = ModuleNotFound, retryable = false |
| `test_scan_error_to_module_error` | Convert ScanError | code = InternalError |
| `test_scan_timeout_to_module_error` | Convert ScanTimeout | code = Timeout, retryable = true |
| `test_scan_permission_to_module_error` | Convert ScanPermission | code = Unauthorized |
| `test_command_injection_to_module_error` | Convert CommandInjection | code = ValidationFailed, details has param_name |
| `test_parse_error_to_module_error` | Convert ParseError | code = InternalError |
| `test_io_error_to_module_error` | Convert Io | code = InternalError, details has io_error_kind |
| `test_yaml_error_to_module_error` | Convert Yaml | code = SerializationError |
| `test_json_error_to_module_error` | Convert Json | code = SerializationError |
| `test_all_variants_have_ai_guidance` | Convert each variant | ai_guidance is Some for all |
| `test_into_module_error_with_trace` | Convert with trace_id | trace_id = Some("abc-123") |
| `test_question_mark_operator_converts` | Use ? in function returning Result<_, ModuleError> | Compiles and converts |

---

## 7. anyhow Removal

With `ModuleError` as the error type for all non-scanner code, the `anyhow` dependency can be removed from `Cargo.toml`. The CLI entry point (`Cli::run()`) changes its return type:

**Before**: `pub fn run(self) -> anyhow::Result<()>`
**After**: `pub fn run(self) -> Result<(), ModuleError>`

The `main.rs` error handling changes accordingly:

```rust
fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli.run() {
        eprintln!("Error: {}", e.message);
        if let Some(guidance) = &e.ai_guidance {
            eprintln!("Suggestion: {}", guidance);
        }
        std::process::exit(1);
    }
}
```

This is a slight improvement over v0.1.x because errors now include structured guidance.

---

## 8. Edge Cases

- **Nested errors**: `ApexeError::Io` wraps `std::io::Error`. The conversion preserves the original error message via `Display` and adds the error kind to `details`.
- **Serde errors**: `Yaml` and `Json` variants are transparent wrappers. The conversion uses `Display` for the message since serde error internals are not structured.
- **trace_id propagation**: The basic `From` conversion sets `trace_id = None`. Use `into_module_error_with_trace()` when a trace_id is available from `Context`.
