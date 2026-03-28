# F5: Governance -- Replace with apcore ACL + apcore-cli Audit/Sandbox

| Field | Value |
|---|---|
| **Feature ID** | F5 |
| **Tech Design Section** | 5.5 |
| **Priority** | P1 (Security) |
| **Dependencies** | F2 (Module Executor) |
| **Depended On By** | F7 (Config Integration) |
| **New Files** | `src/governance/acl.rs` (rewritten), `src/governance/audit.rs` (rewritten), `src/governance/sandbox.rs` (new), `src/governance/mod.rs` (updated) |
| **Deleted Files** | `src/governance/annotations.rs` (moved to F1 adapter) |
| **Estimated LOC** | ~350 |
| **Estimated Tests** | ~20 |

---

## 1. Purpose

Replace apexe's custom governance layer (ACL generator, annotation logic, audit logger) with wrappers around apcore ecosystem primitives: `apcore::ACL` for access control, `apcore_cli::AuditLogger` for audit logging, and `apcore_cli::Sandbox` for subprocess isolation. This gains rule conditions, structured JSONL audit format, and timeout-enforced sandboxing.

---

## 2. Module Structure

### 2.1 `src/governance/mod.rs`

```rust
pub mod acl;
pub mod audit;
pub mod sandbox;

pub use acl::AclManager;
pub use audit::AuditManager;
pub use sandbox::SandboxManager;
```

---

## 3. AclManager

### 3.1 Type Definition

```rust
// src/governance/acl.rs
use std::path::Path;
use apcore::{ACL, ACLRule, ModuleAnnotations, ModuleError, ErrorCode};
use apcore_toolkit::ScannedModule;

/// Manages access control for CLI modules using apcore's ACL system.
pub struct AclManager {
    acl: ACL,
}
```

### 3.2 Public Methods

```rust
impl AclManager {
    /// Load ACL rules from a YAML configuration file.
    ///
    /// File format:
    /// ```yaml
    /// default_effect: deny
    /// rules:
    ///   - callers: ["@external", "*"]
    ///     targets: ["cli.git.status", "cli.git.log"]
    ///     effect: allow
    ///     description: "Auto-allow readonly CLI commands"
    ///   - callers: ["@external", "*"]
    ///     targets: ["cli.git.push"]
    ///     effect: deny
    ///     description: "Block destructive commands by default"
    ///     conditions:
    ///       require_approval: true
    /// ```
    pub fn from_config(config_path: &Path) -> Result<Self, ModuleError>;

    /// Generate a default ACL from scanned modules based on their annotations.
    ///
    /// Logic:
    /// 1. Collect all module_ids where annotations.readonly == true.
    ///    Create rule: allow @external/* to access these modules.
    /// 2. Collect all module_ids where annotations.destructive == true.
    ///    Create rule: deny @external/* with require_approval condition.
    /// 3. All remaining modules: deny by default.
    /// 4. Return ACL with default_effect = Deny.
    pub fn generate_default(modules: &[ScannedModule]) -> ACL;

    /// Write ACL configuration to a YAML file.
    pub fn write_config(acl: &ACL, path: &Path) -> Result<(), ModuleError>;

    /// Consume the manager and return the inner ACL for use with Executor.
    pub fn into_inner(self) -> ACL;

    /// Check if a caller has access to a target module.
    pub fn check(
        &self,
        caller_id: &str,
        caller_roles: &[String],
        target_module: &str,
    ) -> bool;
}
```

### 3.3 Rule Generation Logic (from generate_default)

```rust
pub fn generate_default(modules: &[ScannedModule]) -> ACL {
    let mut rules = Vec::new();

    // Group 1: Readonly modules -> allow
    let readonly_ids: Vec<String> = modules.iter()
        .filter(|m| m.annotations.readonly)
        .map(|m| m.module_id.clone())
        .collect();

    if !readonly_ids.is_empty() {
        rules.push(ACLRule {
            callers: vec!["@external".into(), "*".into()],
            targets: readonly_ids,
            effect: Effect::Allow,
            description: Some("Auto-allow readonly CLI commands".into()),
            conditions: None,
        });
    }

    // Group 2: Destructive modules -> deny with approval
    let destructive_ids: Vec<String> = modules.iter()
        .filter(|m| m.annotations.destructive)
        .map(|m| m.module_id.clone())
        .collect();

    if !destructive_ids.is_empty() {
        rules.push(ACLRule {
            callers: vec!["@external".into(), "*".into()],
            targets: destructive_ids,
            effect: Effect::Deny,
            description: Some("Block destructive CLI commands by default".into()),
            conditions: Some(serde_json::json!({"require_approval": true})),
        });
    }

    // Group 3: Write modules (non-readonly, non-destructive) -> deny
    let write_ids: Vec<String> = modules.iter()
        .filter(|m| !m.annotations.readonly && !m.annotations.destructive)
        .map(|m| m.module_id.clone())
        .collect();

    if !write_ids.is_empty() {
        rules.push(ACLRule {
            callers: vec!["@external".into(), "*".into()],
            targets: write_ids,
            effect: Effect::Deny,
            description: Some("Deny write CLI commands by default".into()),
            conditions: None,
        });
    }

    ACL::new(rules, Effect::Deny)
}
```

---

## 4. AuditManager

### 4.1 Type Definition

```rust
// src/governance/audit.rs
use std::path::Path;
use apcore_cli::AuditLogger;
use serde_json::Value;

/// Manages append-only JSONL audit logging for CLI module executions.
pub struct AuditManager {
    logger: AuditLogger,
}
```

### 4.2 Public Methods

```rust
impl AuditManager {
    /// Create a new AuditManager writing to the given file path.
    ///
    /// The file is created if it does not exist.
    /// Entries are appended (never truncated).
    pub fn new(audit_path: &Path) -> Self {
        Self {
            logger: AuditLogger::new(audit_path),
        }
    }

    /// Log a module execution event.
    ///
    /// Writes a JSONL entry with:
    /// - timestamp (ISO 8601)
    /// - module_id
    /// - input (JSON)
    /// - output (JSON, truncated if large)
    /// - duration_ms
    /// - exit_code (extracted from output)
    /// - success (exit_code == 0)
    pub fn log_execution(
        &self,
        module_id: &str,
        input: &Value,
        output: &Value,
        duration_ms: u64,
    );

    /// Return the path to the audit log file.
    pub fn log_path(&self) -> &Path;
}
```

### 4.3 Integration with CliModule

The `AuditManager` is called inside `CliModule::execute()` after subprocess completion:

```rust
// Inside CliModule::execute()
let start = std::time::Instant::now();
let result = execute_subprocess(...).await?;
let duration_ms = start.elapsed().as_millis() as u64;

if let Some(ref audit) = self.audit {
    audit.log_execution(&self.module_id, &input, &result, duration_ms);
}
```

---

## 5. SandboxManager

### 5.1 Type Definition

```rust
// src/governance/sandbox.rs
use apcore::ModuleError;
use apcore_cli::Sandbox;
use serde_json::Value;

/// Manages subprocess isolation using apcore-cli's Sandbox.
pub struct SandboxManager {
    sandbox: Sandbox,
}
```

### 5.2 Public Methods

```rust
impl SandboxManager {
    /// Create a new SandboxManager.
    ///
    /// - enabled: Whether sandboxing is active (if false, execute() is a pass-through).
    /// - timeout_ms: Maximum execution time for sandboxed processes.
    pub fn new(enabled: bool, timeout_ms: u64) -> Self {
        Self {
            sandbox: Sandbox::new(enabled, timeout_ms),
        }
    }

    /// Execute a module in the sandbox.
    ///
    /// If sandboxing is enabled:
    /// - Subprocess runs in an isolated environment.
    /// - Timeout is enforced (kills process after timeout_ms).
    /// - Returns output or ModuleError on timeout/failure.
    ///
    /// If sandboxing is disabled:
    /// - Pass-through to normal subprocess execution.
    pub fn execute(
        &self,
        module_id: &str,
        input: &Value,
    ) -> Result<Value, ModuleError>;

    /// Check if sandboxing is enabled.
    pub fn is_enabled(&self) -> bool;
}
```

### 5.3 Integration with CliModule

The `SandboxManager` is called in `CliModule::execute()` as an alternative execution path:

```rust
// Inside CliModule::execute()
let result = if let Some(ref sandbox) = self.sandbox {
    sandbox.execute(&self.module_id, &input)?
} else {
    let args = build_arguments(&input_map)?;
    execute_subprocess(&self.binary_path, &args, self.json_flag.as_deref(), None, self.timeout_ms).await?
};
```

---

## 6. Test Scenarios

### 6.1 AclManager Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_acl_generate_default_readonly_allowed` | 2 readonly modules | Rule with effect=Allow for those module_ids |
| `test_acl_generate_default_destructive_denied` | 1 destructive module | Rule with effect=Deny and require_approval |
| `test_acl_generate_default_write_denied` | 1 write module | Rule with effect=Deny |
| `test_acl_generate_default_mixed` | 3 modules (1 each type) | 3 rules |
| `test_acl_generate_default_empty` | No modules | ACL with only default deny |
| `test_acl_from_config_valid_yaml` | Well-formed YAML | ACL loaded with rules |
| `test_acl_from_config_missing_file` | Nonexistent path | Err(ModuleError) |
| `test_acl_write_config_creates_file` | Write and re-read | File exists, content matches |
| `test_acl_check_readonly_allowed` | Check @external -> readonly module | true |
| `test_acl_check_destructive_denied` | Check @external -> destructive module | false |

### 6.2 AuditManager Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_audit_log_creates_file` | Log one execution | File exists |
| `test_audit_log_appends_jsonl` | Log two executions | File has 2 lines |
| `test_audit_log_entry_format` | Log and parse | Valid JSON with timestamp, module_id, duration_ms |
| `test_audit_log_large_output_truncated` | Output > 10KB | Truncated in log entry |
| `test_audit_log_path_returns_path` | Create manager | Returns configured path |

### 6.3 SandboxManager Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_sandbox_enabled_timeout` | Enabled, command exceeds timeout | Err with Timeout |
| `test_sandbox_disabled_passthrough` | Disabled, normal command | Ok with output |
| `test_sandbox_is_enabled_true` | Created with enabled=true | is_enabled() == true |
| `test_sandbox_is_enabled_false` | Created with enabled=false | is_enabled() == false |
| `test_sandbox_execute_normal_command` | Enabled, echo hello | Ok with stdout |

---

## 7. Migration from v0.1.x

### What Changes

| v0.1.x | v0.2.0 | Change Type |
|---|---|---|
| `generate_acl()` free function | `AclManager::generate_default()` method | Restructured |
| `serde_json::Map` ACL format | `apcore::ACL` type | Type change |
| Custom `write_acl()` | `AclManager::write_config()` | Simplified |
| `annotate_bindings()` | Moved to F1 `adapter::annotations::infer()` | Relocated |
| Custom audit JSONL writer | `apcore_cli::AuditLogger` | Replaced |
| No sandbox support | `apcore_cli::Sandbox` | New capability |

### ACL YAML Format Change

v0.1.x format:
```yaml
default_effect: deny
rules:
  - callers: ["@external", "*"]
    targets: ["cli.git.status"]
    effect: allow
    description: "Auto-allow readonly CLI commands"
```

v0.2.0 format (apcore ACL):
```yaml
default_effect: deny
rules:
  - callers: ["@external", "*"]
    targets: ["cli.git.status"]
    effect: allow
    description: "Auto-allow readonly CLI commands"
    conditions: null
```

The format is nearly identical. The `conditions` field is new (nullable). Existing v0.1.x ACL files are forward-compatible.

### Annotation Logic Relocation

The `annotate_bindings()` function from `src/governance/annotations.rs` is not rewritten here. Its logic moves to `src/adapter/annotations.rs` (F1) where it produces `ModuleAnnotations` instead of `HashMap<String, JsonValue>`. The governance module only consumes annotations, it does not generate them.
