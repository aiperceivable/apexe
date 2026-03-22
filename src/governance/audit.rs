use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use serde_json::Value as JsonValue;
use tracing::warn;
use uuid::Uuid;

use crate::errors::ApexeError;
use crate::executor::execute_cli;

/// Single audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// ISO 8601 UTC timestamp.
    pub timestamp: String,
    /// UUID v4 trace identifier.
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

/// Thread-safe append-only JSONL audit logger.
///
/// Uses a Mutex to ensure concurrent tool calls (e.g., via HTTP serve)
/// do not interleave JSONL lines.
pub struct AuditLogger {
    log_path: PathBuf,
    max_log_size_bytes: u64,
    max_backups: u32,
    /// Guards all file operations to prevent concurrent write corruption.
    write_lock: Mutex<()>,
}

impl AuditLogger {
    /// Create a new audit logger with default rotation settings.
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            max_log_size_bytes: 10 * 1024 * 1024, // 10 MB
            max_backups: 3,
            write_lock: Mutex::new(()),
        }
    }

    /// Create a new audit logger with custom rotation settings.
    pub fn with_rotation(log_path: PathBuf, max_log_size_bytes: u64, max_backups: u32) -> Self {
        Self {
            log_path,
            max_log_size_bytes,
            max_backups,
            write_lock: Mutex::new(()),
        }
    }

    /// Append an audit entry to the log.
    ///
    /// Thread-safe: uses internal Mutex to prevent concurrent write corruption.
    /// Non-fatal: audit failure must never block tool execution.
    pub fn log(&self, entry: &AuditEntry) {
        // Acquire lock; if poisoned, still proceed (audit is best-effort)
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Rotate if needed before writing
        self.rotate_if_needed();

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
    pub fn hash_inputs(inputs: &serde_json::Map<String, JsonValue>) -> String {
        let canonical = serde_json::to_string(inputs).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Rotate the log file if it exceeds the max size.
    fn rotate_if_needed(&self) {
        let metadata = match fs::metadata(&self.log_path) {
            Ok(m) => m,
            Err(_) => return, // File doesn't exist yet, no rotation needed
        };

        if metadata.len() < self.max_log_size_bytes {
            return;
        }

        // Rotate: shift existing backups
        // Delete oldest if exceeding max_backups
        let oldest = self.log_path.with_extension(format!("jsonl.{}", self.max_backups));
        if oldest.exists() {
            let _ = fs::remove_file(&oldest);
        }

        // Shift .N-1 -> .N, .N-2 -> .N-1, etc.
        for i in (1..self.max_backups).rev() {
            let from = self.log_path.with_extension(format!("jsonl.{i}"));
            let to = self.log_path.with_extension(format!("jsonl.{}", i + 1));
            if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }

        // Move current -> .1
        let backup = self.log_path.with_extension("jsonl.1");
        let _ = fs::rename(&self.log_path, &backup);
    }

    /// Get the log file path.
    pub fn log_path(&self) -> &PathBuf {
        &self.log_path
    }
}

/// Wrapper around execute_cli that logs audit entries.
pub fn execute_cli_with_audit(
    audit_logger: &AuditLogger,
    apexe_binary: &str,
    apexe_command: &[String],
    apexe_timeout: u64,
    apexe_json_flag: Option<&str>,
    apexe_working_dir: Option<&str>,
    kwargs: &serde_json::Map<String, JsonValue>,
) -> Result<serde_json::Map<String, JsonValue>, ApexeError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // T9: AuditEntry struct and hashing
    #[test]
    fn test_audit_entry_default_has_timestamp() {
        let entry = AuditEntry::default();
        assert!(!entry.timestamp.is_empty());
        // Should be valid RFC 3339
        assert!(entry.timestamp.contains('T'));
    }

    #[test]
    fn test_audit_entry_default_has_uuid() {
        let entry = AuditEntry::default();
        assert!(!entry.trace_id.is_empty());
        // UUID v4 format: 8-4-4-4-12
        assert_eq!(entry.trace_id.len(), 36);
        assert_eq!(entry.trace_id.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn test_hash_inputs_deterministic() {
        let mut inputs = serde_json::Map::new();
        inputs.insert("message".to_string(), json!("hello"));

        let hash1 = AuditLogger::hash_inputs(&inputs);
        let hash2 = AuditLogger::hash_inputs(&inputs);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_inputs_privacy() {
        let mut inputs = serde_json::Map::new();
        inputs.insert("message".to_string(), json!("hello"));

        let hash = AuditLogger::hash_inputs(&inputs);
        assert_ne!(hash, "hello");
        assert!(!hash.contains("hello"));
    }

    #[test]
    fn test_hash_inputs_64_char_hex() {
        let mut inputs = serde_json::Map::new();
        inputs.insert("key".to_string(), json!("value"));

        let hash = AuditLogger::hash_inputs(&inputs);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_empty_inputs() {
        let inputs = serde_json::Map::new();
        let hash = AuditLogger::hash_inputs(&inputs);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // T10: JSONL append-only logging
    #[test]
    fn test_log_single_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path.clone());

        let entry = AuditEntry {
            module_id: "cli.git.status".into(),
            ..Default::default()
        };
        logger.log(&entry);

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("cli.git.status"));

        // Verify parseable as JSON
        let parsed: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.module_id, "cli.git.status");
    }

    #[test]
    fn test_log_append_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path.clone());

        let entry1 = AuditEntry {
            module_id: "cli.git.status".into(),
            ..Default::default()
        };
        let entry2 = AuditEntry {
            module_id: "cli.git.commit".into(),
            ..Default::default()
        };

        logger.log(&entry1);
        logger.log(&entry2);

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("cli.git.status"));
        assert!(lines[1].contains("cli.git.commit"));
    }

    #[test]
    fn test_log_creates_directories() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("nested").join("dir").join("audit.jsonl");
        let logger = AuditLogger::new(log_path.clone());

        let entry = AuditEntry {
            module_id: "test".into(),
            ..Default::default()
        };
        logger.log(&entry);

        assert!(log_path.exists());
    }

    #[test]
    fn test_log_entry_contains_expected_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path.clone());

        let entry = AuditEntry {
            module_id: "cli.git.push".into(),
            inputs_hash: "abc123".into(),
            ..Default::default()
        };
        logger.log(&entry);

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("module_id"));
        assert!(content.contains("timestamp"));
        assert!(content.contains("inputs_hash"));
        assert!(content.contains("abc123"));
    }

    // T11: Error resilience
    #[test]
    fn test_log_unwritable_path_no_panic() {
        let logger = AuditLogger::new(PathBuf::from(
            "/dev/null/impossible/path/audit.jsonl",
        ));

        let entry = AuditEntry {
            module_id: "test".into(),
            ..Default::default()
        };
        // Should not panic
        logger.log(&entry);
    }

    #[test]
    fn test_logger_functional_after_error() {
        let logger_bad = AuditLogger::new(PathBuf::from(
            "/dev/null/impossible/path/audit.jsonl",
        ));
        let entry = AuditEntry {
            module_id: "test".into(),
            ..Default::default()
        };
        logger_bad.log(&entry);

        // Now use a valid path
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger_good = AuditLogger::new(log_path.clone());
        logger_good.log(&entry);

        assert!(log_path.exists());
    }

    // T12: Audit-wrapped execution
    #[test]
    fn test_execute_cli_with_audit_success() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path.clone());

        let command = vec!["echo".to_string(), "hello".to_string()];
        let kwargs = serde_json::Map::new();

        let result = execute_cli_with_audit(
            &logger,
            "echo",
            &command,
            30,
            None,
            None,
            &kwargs,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.exit_code, 0);
        assert!(parsed.duration_ms >= 0.0);
        assert!(!parsed.inputs_hash.is_empty());
    }

    #[test]
    fn test_execute_cli_with_audit_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path.clone());

        let command = vec!["nonexistent_command_xyz_12345".to_string()];
        let kwargs = serde_json::Map::new();

        let result = execute_cli_with_audit(
            &logger,
            "nonexistent_command_xyz_12345",
            &command,
            30,
            None,
            None,
            &kwargs,
        );
        assert!(result.is_err());

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.exit_code, -1);
        assert!(parsed.error.is_some());
    }

    #[test]
    fn test_execute_cli_with_audit_transparent_passthrough() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path);

        let command = vec!["echo".to_string(), "test".to_string()];
        let kwargs = serde_json::Map::new();

        let audited = execute_cli_with_audit(
            &logger,
            "echo",
            &command,
            30,
            None,
            None,
            &kwargs,
        )
        .unwrap();

        let direct = execute_cli("echo", &command, 30, None, None, &kwargs).unwrap();

        // Both should have the same stdout content
        assert_eq!(audited["stdout"], direct["stdout"]);
        assert_eq!(audited["exit_code"], direct["exit_code"]);
    }

    // T13: Log rotation
    #[test]
    fn test_rotation_trigger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");

        // Create a logger with 100 byte threshold
        let logger = AuditLogger::with_rotation(log_path.clone(), 100, 3);

        // Write enough to exceed threshold
        let entry = AuditEntry {
            module_id: "cli.git.status".into(),
            inputs_hash: "a".repeat(64),
            ..Default::default()
        };
        // First entry
        logger.log(&entry);
        // Should have written first file
        assert!(log_path.exists());

        // Keep writing until rotation happens
        for _ in 0..5 {
            logger.log(&entry);
        }

        // After rotation, backup should exist
        let backup1 = log_path.with_extension("jsonl.1");
        assert!(backup1.exists());
    }

    #[test]
    fn test_rotation_backup_limit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");

        // max_backups = 2, tiny threshold
        let logger = AuditLogger::with_rotation(log_path.clone(), 50, 2);

        let entry = AuditEntry {
            module_id: "cli.test".into(),
            inputs_hash: "a".repeat(64),
            ..Default::default()
        };

        // Write many entries to trigger multiple rotations
        for _ in 0..20 {
            logger.log(&entry);
        }

        // .1 and .2 should exist, but .3 should not (max_backups=2)
        let backup1 = log_path.with_extension("jsonl.1");
        let backup2 = log_path.with_extension("jsonl.2");
        let backup3 = log_path.with_extension("jsonl.3");

        assert!(backup1.exists());
        assert!(backup2.exists());
        assert!(!backup3.exists());
    }

    #[test]
    fn test_no_rotation_when_under_threshold() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");

        // High threshold
        let logger = AuditLogger::with_rotation(log_path.clone(), 10 * 1024 * 1024, 3);

        let entry = AuditEntry {
            module_id: "test".into(),
            ..Default::default()
        };
        logger.log(&entry);

        let backup1 = log_path.with_extension("jsonl.1");
        assert!(!backup1.exists());
    }

    #[test]
    fn test_rotation_first_write_no_rotation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log_path = tmp.path().join("audit.jsonl");

        // File doesn't exist yet, tiny threshold
        let logger = AuditLogger::with_rotation(log_path.clone(), 10, 3);

        let entry = AuditEntry {
            module_id: "test".into(),
            ..Default::default()
        };
        // First write -- file doesn't exist, so no rotation
        logger.log(&entry);
        assert!(log_path.exists());

        let backup1 = log_path.with_extension("jsonl.1");
        assert!(!backup1.exists());
    }
}
