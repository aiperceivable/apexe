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

**Table updated for apcore 0.27's `ErrorCode` names** — apcore renamed several
variants after this table was first written (`InternalError` →
`GeneralInternalError`, `Timeout` → `ModuleTimeout`, `ValidationFailed` →
`GeneralInvalidInput`, `Unauthorized` → `ACLDenied`), and `SerializationError`
was removed entirely, so `Yaml`/`Json` map to `GeneralInternalError` like the
other internal-failure variants.

| ApexeError Variant | ErrorCode | retryable | ai_guidance |
|---|---|---|---|
| `ToolNotFound { tool_name }` | `ModuleNotFound` | false | "The tool '{tool_name}' is not installed. Install it and try again." |
| `ScanError(msg)` | `GeneralInternalError` | false | "An internal scanning error occurred: {msg}" |
| `ScanTimeout { command, timeout }` | `ModuleTimeout` | true | "The command took too long. Try with simpler arguments or increase timeout." |
| `ScanPermission { command }` | `ACLDenied`¹ | false | "Permission denied. Check file permissions or run with appropriate privileges." |
| `CommandInjection { param_name, chars }` | `GeneralInvalidInput` | false | "Remove shell metacharacters ({chars:?}) from parameter '{param_name}'." |
| `ParseError(msg)` | `GeneralInternalError` | false | "Help text parsing failed: {msg}. The tool may use a non-standard help format." |
| `Io(err)` | `GeneralInternalError` | false | "I/O error: {err}" |
| `Yaml(err)` | `GeneralInternalError` | false | "YAML processing error: {err}" |
| `Json(err)` | `GeneralInternalError` | false | "JSON processing error: {err}" |

¹ `ACLDenied` is also the code `apcore::ACL`'s own governance-denial path
writes to the audit trail (see F5 §4), so a filesystem permission error
during a scan and an actual ACL policy denial currently render with the
same code. Tracked as a known mismatch, not yet corrected.

---

## 4. Implementation

### 4.1 From Trait Implementation

**`ModuleError` in apcore 0.27 is constructed through a builder
(`ModuleError::new(code, message).with_details(...).with_retryable(...)
.with_ai_guidance(...)`), not the struct-literal shape shown in earlier
drafts of this section. The full, current match arm for every variant lives
in `src/errors.rs`'s `impl From<ApexeError> for ModuleError`; §3's table
above is the up-to-date summary of what each arm produces. Representative
arm, for the shape:**

```rust
// src/errors.rs
use apcore::{ErrorCode, ModuleError};

impl From<ApexeError> for ModuleError {
    fn from(err: ApexeError) -> ModuleError {
        match err {
            ApexeError::ToolNotFound { ref tool_name } => {
                ModuleError::new(ErrorCode::ModuleNotFound, err.to_string())
                    .with_details(details([("tool_name", serde_json::json!(tool_name))]))
                    .with_retryable(false)
                    .with_ai_guidance(format!(
                        "The tool '{}' is not installed. Install it and try again.",
                        tool_name
                    ))
            }
            // ... one arm per ApexeError variant; see §3 for the full mapping.
        }
    }
}
```

### 4.2 Convenience Constructor

```rust
// src/errors.rs
impl ApexeError {
    /// Convert to ModuleError with an attached trace_id.
    pub fn into_module_error_with_trace(self, trace_id: String) -> ModuleError {
        let module_err: ModuleError = self.into();
        module_err.with_trace_id(trace_id)
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
