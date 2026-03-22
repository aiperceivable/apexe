# F5: Governance Defaults

| Field | Value |
|-------|-------|
| **Feature** | F5 |
| **Priority** | P1 (enterprise differentiator) |
| **Effort** | Medium (~900 LOC) |
| **Dependencies** | F2, F3 |

---

## 1. Overview

Auto-generate sensible behavioral annotations, ACL rules, and audit trails from CLI scan results. This is the key enterprise differentiator -- the feature that makes apexe EU AI Act-ready and distinguishes it from CLI-Anything and other competitors that offer zero governance.

Three components:
1. **Annotation Inference Engine** -- infers `readonly`, `destructive`, `requires_approval`, `idempotent` from command semantics
2. **ACL Generator** -- produces `acl.yaml` with default rules based on annotations
3. **Audit Logger** -- writes JSONL audit entries for every tool invocation

---

## 2. Module: `src/governance/annotations.rs`

### Constants

```rust
/// Command name patterns that indicate destructive operations.
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "delete", "remove", "rm", "drop", "kill", "destroy",
    "purge", "wipe", "clean", "reset", "uninstall",
    "truncate", "erase", "revoke",
];

/// Command name patterns that indicate read-only operations.
const READONLY_PATTERNS: &[&str] = &[
    "list", "show", "status", "info", "get", "cat", "ls",
    "describe", "inspect", "view", "print", "help", "version",
    "check", "diff", "log", "search", "find", "which", "whoami",
    "count", "stat", "top", "ps", "env", "config",
];

/// Command name patterns that indicate write operations.
const WRITE_PATTERNS: &[&str] = &[
    "create", "add", "write", "push", "send", "set", "update",
    "put", "post", "upload", "install", "init", "apply",
    "commit", "merge", "tag", "publish", "deploy",
];

/// Flag names that indicate the command should require approval.
const APPROVAL_FLAGS: &[&str] = &[
    "--force", "-f", "--hard", "--recursive", "-r",
    "--all", "--prune", "--no-preserve-root",
    "--cascade", "--purge", "--yes", "-y",
];

/// Flag names that indicate the command is idempotent.
const IDEMPOTENT_FLAGS: &[&str] = &[
    "--dry-run", "--check", "--diff", "--noop",
    "--simulate", "--whatif", "--plan",
];
```

### Function: `infer_annotations`

```rust
use serde::{Deserialize, Serialize};

/// Behavioral annotations for a module.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleAnnotations {
    pub readonly: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub requires_approval: bool,
    pub open_world: bool,
}

/// Annotation inference result with confidence metadata.
///
/// Confidence scores:
///   - 0.0-0.3: No strong signals, default assumptions
///   - 0.4-0.6: Weak signal (description match only)
///   - 0.7-0.8: Moderate signal (command name match)
///   - 0.9-1.0: Strong signal (multiple corroborating signals)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredAnnotations {
    pub annotations: ModuleAnnotations,
    pub confidence: f64,
    pub reasoning: String,
}

/// Infer behavioral annotations from command semantics.
///
/// # Algorithm
///
/// 1. Initialize all flags to false
/// 2. Normalize command name and description to lowercase
/// 3. Check DESTRUCTIVE_PATTERNS against command name and description
/// 4. If not destructive, check READONLY_PATTERNS
/// 5. Check APPROVAL_FLAGS against the command's flag list
/// 6. Force approval for destructive commands
/// 7. Check IDEMPOTENT_FLAGS
/// 8. Calculate confidence: base 0.5 + 0.1 per matched reason, capped at 0.95
pub fn infer_annotations(
    command_name: &str,
    full_command: &str,
    flags: &[String],
    description: &str,
) -> InferredAnnotations {
    let mut reasons: Vec<String> = Vec::new();
    let mut readonly = false;
    let mut destructive = false;
    let mut idempotent = false;
    let mut requires_approval = false;

    let cmd_lower = command_name.to_lowercase();
    let desc_lower = description.to_lowercase();

    // Check destructive patterns
    for &pattern in DESTRUCTIVE_PATTERNS {
        if cmd_lower.contains(pattern) || desc_lower.contains(pattern) {
            destructive = true;
            reasons.push(format!(
                "Command/description contains destructive keyword '{pattern}'"
            ));
            break;
        }
    }

    // Check readonly patterns (only if not destructive)
    if !destructive {
        for &pattern in READONLY_PATTERNS {
            if cmd_lower.contains(pattern) || desc_lower.contains(pattern) {
                readonly = true;
                reasons.push(format!(
                    "Command/description contains readonly keyword '{pattern}'"
                ));
                break;
            }
        }
    }

    // Check for approval-requiring flags
    for flag in flags {
        if APPROVAL_FLAGS.contains(&flag.as_str()) {
            requires_approval = true;
            reasons.push(format!("Command has dangerous flag '{flag}'"));
        }
    }

    // Force approval for destructive commands
    if destructive {
        requires_approval = true;
        reasons.push("Destructive commands require approval by default".to_string());
    }

    // Check for idempotent indicators
    for flag in flags {
        if IDEMPOTENT_FLAGS.contains(&flag.as_str()) {
            idempotent = true;
            reasons.push(format!("Command has idempotent indicator flag '{flag}'"));
        }
    }

    // Calculate confidence
    let confidence = if reasons.is_empty() {
        reasons.push("No strong signals detected, using defaults".to_string());
        0.3
    } else {
        (0.5 + 0.1 * reasons.len() as f64).min(0.95)
    };

    InferredAnnotations {
        annotations: ModuleAnnotations {
            readonly,
            destructive,
            idempotent,
            requires_approval,
            open_world: false,
        },
        confidence,
        reasoning: reasons.join("; "),
    }
}
```

### Function: `annotate_bindings`

```rust
use serde_json::json;

use crate::binding::binding_gen::GeneratedBinding;

/// Apply inferred annotations to a list of generated bindings.
///
/// For each binding:
/// 1. Extract command_name from module_id (last segment after "cli.")
/// 2. Extract full_command from metadata
/// 3. Extract flag names from input_schema properties
/// 4. Call infer_annotations()
/// 5. Populate binding.annotations
/// 6. Store confidence and reasoning in binding.metadata
pub fn annotate_bindings(bindings: &mut [GeneratedBinding]) {
    for binding in bindings.iter_mut() {
        let command_name = binding
            .module_id
            .rsplit('.')
            .next()
            .unwrap_or("unknown")
            .to_string();

        let full_command = binding
            .metadata
            .get("apexe_command")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();

        let flags: Vec<String> = binding
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|props| {
                props
                    .keys()
                    .map(|k| format!("--{}", k.replace('_', "-")))
                    .collect()
            })
            .unwrap_or_default();

        let inferred = infer_annotations(
            &command_name,
            &full_command,
            &flags,
            &binding.description,
        );

        binding.annotations.insert("readonly".to_string(), json!(inferred.annotations.readonly));
        binding.annotations.insert("destructive".to_string(), json!(inferred.annotations.destructive));
        binding.annotations.insert("idempotent".to_string(), json!(inferred.annotations.idempotent));
        binding.annotations.insert(
            "requires_approval".to_string(),
            json!(inferred.annotations.requires_approval),
        );
        binding.annotations.insert("open_world".to_string(), json!(false));

        binding.metadata.insert(
            "apexe_annotation_confidence".to_string(),
            json!(inferred.confidence),
        );
        binding.metadata.insert(
            "apexe_annotation_reasoning".to_string(),
            json!(inferred.reasoning),
        );
    }
}
```

### Concrete Annotation Examples

| Command | Module ID | Annotations | Reasoning |
|---------|-----------|-------------|-----------|
| `git status` | `cli.git.status` | `readonly=true` | "status" matches READONLY_PATTERNS |
| `git commit` | `cli.git.commit` | `readonly=false` | "commit" matches WRITE_PATTERNS |
| `git push` | `cli.git.push` | `readonly=false` | "push" matches WRITE_PATTERNS |
| `git push --force` | `cli.git.push` | `requires_approval=true` | "--force" in APPROVAL_FLAGS |
| `docker rm` | `cli.docker.rm` | `destructive=true, requires_approval=true` | "rm" matches DESTRUCTIVE_PATTERNS |
| `docker ps` | `cli.docker.ps` | `readonly=true` | "ps" matches READONLY_PATTERNS |
| `kubectl delete` | `cli.kubectl.delete` | `destructive=true, requires_approval=true` | "delete" matches DESTRUCTIVE_PATTERNS |
| `kubectl get` | `cli.kubectl.get` | `readonly=true` | "get" matches READONLY_PATTERNS |
| `kubectl apply` | `cli.kubectl.apply` | `readonly=false` | "apply" matches WRITE_PATTERNS |
| `terraform plan` | `cli.terraform.plan` | `readonly=true, idempotent=true` | "plan" is readonly; "--plan" in IDEMPOTENT_FLAGS |
| `rm` | `cli.rm` | `destructive=true, requires_approval=true` | "rm" matches DESTRUCTIVE_PATTERNS |

---

## 3. Module: `src/governance/acl.rs`

### Function: `generate_acl`

```rust
use std::path::Path;

use serde_json::{json, Value as JsonValue};
use tracing::{info, warn};

/// Generate default ACL configuration from annotated bindings.
///
/// # Arguments
///
/// * `bindings` - List of binding maps with "module_id" and "annotations" keys.
/// * `default_effect` - Default effect for unmatched rules: "allow" or "deny".
///   Default: "deny" (enterprise-safe).
///
/// # Algorithm
///
/// 1. Categorize bindings into readonly, destructive, and write groups
/// 2. Build rules:
///    - Rule 1: Allow all readonly commands from any caller
///    - Rule 2: Allow destructive commands (approval enforced at module level)
///    - Rule 3: Allow non-destructive write commands
/// 3. Return ACL config dict
pub fn generate_acl(
    bindings: &[serde_json::Map<String, JsonValue>],
    default_effect: &str,
) -> serde_json::Map<String, JsonValue> {
    let mut rules: Vec<JsonValue> = Vec::new();

    let readonly_ids: Vec<&str> = bindings
        .iter()
        .filter(|b| {
            b.get("annotations")
                .and_then(|a| a.get("readonly"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|b| b.get("module_id")?.as_str())
        .collect();

    if !readonly_ids.is_empty() {
        rules.push(json!({
            "callers": ["@external", "*"],
            "targets": readonly_ids,
            "effect": "allow",
            "description": "Auto-allow readonly CLI commands",
        }));
    }

    let destructive_ids: Vec<&str> = bindings
        .iter()
        .filter(|b| {
            b.get("annotations")
                .and_then(|a| a.get("destructive"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|b| b.get("module_id")?.as_str())
        .collect();

    if !destructive_ids.is_empty() {
        rules.push(json!({
            "callers": ["@external", "*"],
            "targets": destructive_ids,
            "effect": "allow",
            "description": "Destructive CLI commands (requires_approval enforced at module level)",
        }));
    }

    let write_ids: Vec<&str> = bindings
        .iter()
        .filter(|b| {
            let annotations = b.get("annotations");
            let is_readonly = annotations
                .and_then(|a| a.get("readonly"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_destructive = annotations
                .and_then(|a| a.get("destructive"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            !is_readonly && !is_destructive
        })
        .filter_map(|b| b.get("module_id")?.as_str())
        .collect();

    if !write_ids.is_empty() {
        rules.push(json!({
            "callers": ["@external", "*"],
            "targets": write_ids,
            "effect": "allow",
            "description": "Non-destructive write CLI commands",
        }));
    }

    let mut acl = serde_json::Map::new();
    acl.insert("$schema".to_string(), json!("https://apcore.dev/acl/v1"));
    acl.insert("version".to_string(), json!("1.0.0"));
    acl.insert("rules".to_string(), JsonValue::Array(rules));
    acl.insert("default_effect".to_string(), json!(default_effect));
    acl.insert("audit".to_string(), json!({
        "enabled": true,
        "log_level": "info",
        "include_denied": true,
    }));
    acl
}
```

### Function: `write_acl`

```rust
/// Write ACL configuration to a YAML file.
///
/// Non-fatal: logs warnings on failure but does not return errors.
pub fn write_acl(acl_config: &serde_json::Map<String, JsonValue>, output_path: &Path) {
    if let Some(parent) = output_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %output_path.display(), "Failed to create ACL directory: {e}");
            return;
        }
    }

    let yaml_value = JsonValue::Object(acl_config.clone());
    let yaml_str = match serde_yaml::to_string(&yaml_value) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to serialize ACL to YAML: {e}");
            return;
        }
    };

    let content = format!("# Auto-generated by apexe. Edit to customize.\n{yaml_str}");
    match std::fs::write(output_path, content) {
        Ok(()) => info!(path = %output_path.display(), "ACL written"),
        Err(e) => warn!(path = %output_path.display(), "Failed to write ACL: {e}"),
    }
}
```

### Generated ACL Example

For `apexe scan git`:

```yaml
# Auto-generated by apexe. Edit to customize.
$schema: https://apcore.dev/acl/v1
version: "1.0.0"
rules:
  - callers: ["@external", "*"]
    targets:
      - cli.git.status
      - cli.git.log
      - cli.git.diff
      - cli.git.branch
      - cli.git.show
      - cli.git.describe
    effect: allow
    description: "Auto-allow readonly CLI commands"

  - callers: ["@external", "*"]
    targets:
      - cli.git.clean
      - cli.git.reset
    effect: allow
    description: "Destructive CLI commands (requires_approval enforced at module level)"

  - callers: ["@external", "*"]
    targets:
      - cli.git.commit
      - cli.git.push
      - cli.git.pull
      - cli.git.merge
      - cli.git.rebase
      - cli.git.tag
      - cli.git.add
    effect: allow
    description: "Non-destructive write CLI commands"

default_effect: deny
audit:
  enabled: true
  log_level: info
  include_denied: true
```

---

## 4. Module: `src/governance/audit.rs`

### Struct: `AuditEntry`

```rust
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

/// Single audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// ISO 8601 UTC timestamp.
    pub timestamp: String,
    /// UUID v4 trace identifier from apcore Context.
    pub trace_id: String,
    /// Canonical module ID (e.g., "cli.git.commit").
    pub module_id: String,
    /// Identity of the caller (e.g., "claude-desktop", "@external").
    pub caller_id: String,
    /// SHA-256 hash of input parameters. Actual values are NOT logged.
    pub inputs_hash: String,
    /// CLI process exit code (0 = success).
    pub exit_code: i32,
    /// Execution wall-clock time in milliseconds.
    pub duration_ms: f64,
    /// Error message string if execution failed. None on success.
    pub error: Option<String>,
}

impl Default for AuditEntry {
    fn default() -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            trace_id: Uuid::new_v4().to_string(),
            module_id: String::new(),
            caller_id: String::new(),
            inputs_hash: String::new(),
            exit_code: 0,
            duration_ms: 0.0,
            error: None,
        }
    }
}
```

### Struct: `AuditLogger`

```rust
/// Append-only JSONL audit logger.
///
/// Thread-safe via file-level atomic append (single write call).
pub struct AuditLogger {
    log_path: PathBuf,
}

impl AuditLogger {
    /// Create a new audit logger.
    ///
    /// File and parent directories are created on first write.
    pub fn new(log_path: PathBuf) -> Self {
        Self { log_path }
    }

    /// Append an audit entry to the log.
    ///
    /// Non-fatal: audit failure must never block tool execution.
    pub fn log(&self, entry: &AuditEntry) {
        if let Some(parent) = self.log_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                warn!(path = %self.log_path.display(), "Failed to create audit directory: {e}");
                return;
            }
        }

        let json = match serde_json::to_string(entry) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to serialize audit entry: {e}");
                return;
            }
        };

        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .and_then(|mut f| writeln!(f, "{json}"));

        if let Err(e) = result {
            warn!(
                module_id = %entry.module_id,
                "Failed to write audit entry: {e}"
            );
        }
    }

    /// Compute SHA-256 hash of input parameters for privacy-preserving audit.
    pub fn hash_inputs(inputs: &serde_json::Map<String, serde_json::Value>) -> String {
        let canonical = serde_json::to_string(inputs).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}
```

### Audit Integration Point

The audit logger is wired into the execution path via a wrapper around `execute_cli()`:

```rust
use std::time::Instant;

use crate::executor::execute_cli;

/// Wrapper around execute_cli that logs audit entries.
pub fn execute_cli_with_audit(
    audit_logger: &AuditLogger,
    apexe_binary: &str,
    apexe_command: &[String],
    apexe_timeout: u64,
    apexe_json_flag: Option<&str>,
    apexe_working_dir: Option<&str>,
    kwargs: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, crate::errors::ApexeError> {
    let start = Instant::now();
    let inputs_hash = AuditLogger::hash_inputs(kwargs);

    let result = execute_cli(
        apexe_binary,
        apexe_command,
        apexe_timeout,
        apexe_json_flag,
        apexe_working_dir,
        kwargs,
    );

    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    let (exit_code, error) = match &result {
        Ok(output) => {
            let code = output
                .get("exit_code")
                .and_then(|v| v.as_i64())
                .unwrap_or(-1) as i32;
            (code, None)
        }
        Err(e) => (-1, Some(e.to_string())),
    };

    let module_id = apexe_command.join(".");

    let entry = AuditEntry {
        timestamp: Utc::now().to_rfc3339(),
        trace_id: Uuid::new_v4().to_string(),
        module_id,
        caller_id: "@external".to_string(),
        inputs_hash,
        exit_code,
        duration_ms,
        error,
    };

    audit_logger.log(&entry);

    result
}
```

### Audit Log Format Example

```jsonl
{"timestamp":"2026-03-21T10:30:00Z","trace_id":"a1b2c3d4-e5f6-7890-abcd-ef1234567890","module_id":"cli.git.status","caller_id":"@external","inputs_hash":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","exit_code":0,"duration_ms":45.2,"error":null}
{"timestamp":"2026-03-21T10:30:05Z","trace_id":"b2c3d4e5-f6a7-8901-bcde-f12345678901","module_id":"cli.git.commit","caller_id":"@external","inputs_hash":"5d41402abc4b2a76b9719d911017c592b1b3c3a75f73e5f9cd3a38","exit_code":0,"duration_ms":120.8,"error":null}
{"timestamp":"2026-03-21T10:30:10Z","trace_id":"c3d4e5f6-a7b8-9012-cdef-123456789012","module_id":"cli.git.push","caller_id":"@external","inputs_hash":"7d793037a0760186574b0282f2f435e7","exit_code":1,"duration_ms":3200.1,"error":"fatal: could not read from remote repository"}
```

---

## 5. Governance Integration in Scan Pipeline

The governance engine is called during the scan-to-binding pipeline:

```
ScannedCLITool
      |
      v
BindingGenerator::generate()
      |
      v
annotate_bindings(&mut bindings)    <-- F5: Annotation inference
      |
      v
BindingYAMLWriter::write()          <-- Annotations embedded in YAML
      |
      v
generate_acl(&bindings)             <-- F5: ACL generation
      |
      v
write_acl(&acl, ~/.apexe/)          <-- ACL file written
```

---

## 6. Test Scenarios

| Test ID | Scenario | Input | Expected |
|---------|----------|-------|----------|
| F5-T01 | Destructive: `rm` | `command_name="rm"` | `destructive=true, requires_approval=true` |
| F5-T02 | Destructive: `delete` | `command_name="delete"` | `destructive=true, requires_approval=true` |
| F5-T03 | Destructive: `kill` | `command_name="kill"` | `destructive=true, requires_approval=true` |
| F5-T04 | Readonly: `status` | `command_name="status"` | `readonly=true` |
| F5-T05 | Readonly: `ls` | `command_name="ls"` | `readonly=true` |
| F5-T06 | Readonly: `get` | `command_name="get"` | `readonly=true` |
| F5-T07 | Write: `commit` | `command_name="commit"` | `readonly=false, destructive=false` |
| F5-T08 | Write: `push` | `command_name="push"` | `readonly=false, destructive=false` |
| F5-T09 | Force flag | `flags=["--force"]` | `requires_approval=true` |
| F5-T10 | Hard flag | `flags=["--hard"]` | `requires_approval=true` |
| F5-T11 | Dry-run flag | `flags=["--dry-run"]` | `idempotent=true` |
| F5-T12 | No signals | `command_name="foo"` | defaults, `confidence=0.3` |
| F5-T13 | Description match | `desc="permanently deletes"` | `destructive=true` |
| F5-T14 | Multiple signals | `name="delete", flags=["--force"]` | `confidence >= 0.7` |
| F5-T15 | ACL: readonly allow | Readonly bindings | Rule with `effect: allow` |
| F5-T16 | ACL: destructive rule | Destructive bindings | Rule present with description |
| F5-T17 | ACL: default deny | `default_effect="deny"` | `default_effect: deny` in output |
| F5-T18 | ACL: YAML valid | Generated ACL | `serde_yaml::from_str()` succeeds |
| F5-T19 | ACL: write to file | `write_acl()` | File exists with correct content |
| F5-T20 | Audit: entry logged | `audit_logger.log(entry)` | JSONL line appended to file |
| F5-T21 | Audit: inputs hashed | `hash_inputs({"message": "hi"})` | SHA-256 hex string, not "hi" |
| F5-T22 | Audit: file created | First log call | File and directories created |
| F5-T23 | Audit: append-only | Multiple log calls | File grows, previous entries preserved |
| F5-T24 | Audit: error resilience | Unwritable path | Warning logged, no panic |
| F5-T25 | Annotation override | User edits binding YAML | User annotation wins over inference |
| F5-T26 | annotate_bindings | Vec of GeneratedBinding | All bindings have annotations map populated |
| F5-T27 | Confidence scoring | Various command patterns | Confidence range [0.3, 0.95] |
| F5-T28 | Reasoning string | Any inference | Non-empty string explaining decision |

### Example Tests

```rust
use rstest::rstest;

#[rstest]
#[case("rm", true, true)]
#[case("delete", true, true)]
#[case("status", false, false)]
#[case("ls", false, false)]
#[case("commit", false, false)]
fn test_annotation_inference(
    #[case] command: &str,
    #[case] expect_destructive: bool,
    #[case] expect_approval: bool,
) {
    let result = infer_annotations(command, command, &[], "");
    assert_eq!(result.annotations.destructive, expect_destructive);
    assert_eq!(result.annotations.requires_approval, expect_approval);
}

#[test]
fn test_readonly_detection() {
    let result = infer_annotations("status", "git status", &[], "Show working tree status");
    assert!(result.annotations.readonly);
    assert!(!result.annotations.destructive);
    assert!(result.confidence > 0.5);
}

#[test]
fn test_audit_hash_deterministic() {
    let mut inputs = serde_json::Map::new();
    inputs.insert("message".to_string(), serde_json::json!("hello"));

    let hash1 = AuditLogger::hash_inputs(&inputs);
    let hash2 = AuditLogger::hash_inputs(&inputs);
    assert_eq!(hash1, hash2);
    assert_ne!(hash1, "hello"); // Hashed, not plaintext
}

#[test]
fn test_audit_append_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log_path = tmp.path().join("audit.jsonl");
    let logger = AuditLogger::new(log_path.clone());

    let entry1 = AuditEntry { module_id: "cli.git.status".into(), ..Default::default() };
    let entry2 = AuditEntry { module_id: "cli.git.commit".into(), ..Default::default() };

    logger.log(&entry1);
    logger.log(&entry2);

    let content = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("cli.git.status"));
    assert!(lines[1].contains("cli.git.commit"));
}
```
