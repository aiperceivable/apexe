# F5: Governance -- apcore ACL + apcore-cli Audit + always-on subprocess isolation

| Field | Value |
|---|---|
| **Feature ID** | F5 |
| **Tech Design Section** | 5.5 |
| **Priority** | P1 (Security) |
| **Dependencies** | F2 (Module Executor) |
| **Depended On By** | F7 (Config Integration) |
| **New Files** | `src/governance/acl.rs` (rewritten), `src/governance/audit.rs` (rewritten), `src/governance/mod.rs` (updated). Subprocess isolation lives in `src/module/executor.rs` (always-on), not a separate sandbox module — see §5. |
| **Deleted Files** | `src/governance/annotations.rs` (moved to F1 adapter) |
| **Estimated LOC** | ~350 |
| **Estimated Tests** | ~20 |

---

## 1. Purpose

Replace apexe's custom governance layer (ACL generator, annotation logic, audit logger) with wrappers around apcore ecosystem primitives: `apcore::ACL` for access control and `apcore_cli::AuditLogger` for audit logging. This gains rule conditions and a structured JSONL audit format. Subprocess isolation (timeout, output cap, no-shell argv, environment scrubbing) is applied unconditionally in the executor — see §5 — rather than via a toggleable sandbox module.

---

## 2. Module Structure

### 2.1 `src/governance/mod.rs`

```rust
pub mod acl;
pub mod audit;

pub use acl::AclManager;
pub use audit::AuditManager;
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
    /// ```
    ///
    /// A deny rule must be **unconditional** to deny. apcore registers exactly
    /// five condition keys (`identity_types`, `roles`, `max_call_depth`,
    /// `$or`, `$not`); any other key is treated as unsatisfied, so the rule
    /// never matches and the call falls through to the next rule or to
    /// `default_effect`. There is no condition meaning "ask a human first" —
    /// approval is a separate layer (the Executor's `ApprovalHandler`; see the
    /// user manual's §9.1 and §9.6), not an ACL condition.
    pub fn from_config(config_path: &Path) -> Result<Self, ModuleError>;

    /// Generate a default ACL from scanned modules based on their annotations.
    ///
    /// Logic:
    /// 1. Collect all module_ids where annotations.readonly == true.
    ///    Create rule: allow @external/* to access these modules.
    /// 2. Collect all module_ids where annotations.destructive == true.
    ///    Create rule: unconditional deny for @external/*. (Not a
    ///    `require_approval` condition — apcore does not register that key,
    ///    so the rule would never match and the command would run.)
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

    // Group 2: Destructive modules -> unconditional deny.
    //
    // Deliberately no `conditions`. `require_approval` is not one of apcore's
    // five registered condition keys, and an unknown key is treated as
    // unsatisfied — the rule would never match, so a destructive command
    // would fall through to the next rule or to `default_effect` and run.
    // Approval gating lives in the Executor's ApprovalHandler, not the ACL.
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
            conditions: None,
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

## 5. Subprocess Isolation (always-on)

> **Design note (revised):** apexe does **not** expose a toggleable
> `SandboxManager`. There is no safe "unsandboxed" mode to opt out of, so
> isolation is applied unconditionally to every wrapped-tool execution in
> `execute_subprocess` (`src/module/executor.rs`). This supersedes the earlier
> `SandboxManager` / `--sandbox` design (which only wrapped a timeout that is
> now always on).

Every subprocess execution applies:

- **Timeout + kill** — the subprocess runs under a wall-clock deadline and is
  killed (`kill_on_drop`) if it elapses, so a hung tool cannot stall a request.
- **Output cap** — stdout/stderr are each bounded (`max_output_bytes`) to guard
  against OOM from runaway output.
- **No shell** — arguments are passed as direct argv (no shell interpolation);
  client-supplied values are additionally rejected if they contain shell
  metacharacters (see F2 injection guard).
- **Environment scrubbing** — the child inherits only a base allowlist
  (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`, `TZ`,
  `TMPDIR`), never apexe's full environment, so secrets in apexe's env (API
  tokens, cloud credentials) cannot leak to an agent-invoked tool. File-based
  credentials under `$HOME` still work.
- **stdin** is connected to `/dev/null` so a tool waiting on input fails fast.

**Not provided (roadmap):** OS-level sandboxing (seccomp/landlock filters,
namespaces, cgroup resource limits) and per-tool credential-env passthrough (an
opt-in allowlist extension). v0.x relies on the governance stack (ACL +
approval + preview/dry-run) plus the process hygiene above.

---

## 6. Test Scenarios

### 6.1 AclManager Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_acl_generate_default_readonly_allowed` | 2 readonly modules | Rule with effect=Allow for those module_ids |
| `test_acl_generate_default_destructive_denied` | 1 destructive module | Rule with effect=Deny and no `conditions` |
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

### 6.3 Subprocess Isolation Tests (always-on)

Isolation is verified where it lives — in the executor
(`src/module/executor.rs`), not a sandbox module.

| Test Name | Scenario | Expected |
|---|---|---|
| `test_is_allowed_env` | Allowlist logic | PATH/HOME/LC_* allowed; secrets denied |
| `test_execute_subprocess_scrubs_environment` | Wrapped subprocess env | Only allowlisted vars reach the child; PATH survives |
| `test_execute_subprocess_timeout_leaves_retryable_unset` | Hung command + short timeout | Err with Timeout |
| `test_execute_subprocess_kills_hung_process_on_timeout` | Hung command | Child killed (no orphan) |
| `test_run_with_timeout_large_output_no_deadlock` | Output > pipe buffer | Captured, no deadlock (scanner exec) |

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
| Ad-hoc subprocess spawn | Always-on isolation in `executor.rs` (timeout, output cap, no-shell, env scrubbing) | Hardened — see §5 |

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
