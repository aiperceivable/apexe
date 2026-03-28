use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};

/// Manages audit logging using apcore-cli's AuditLogger.
///
/// Wraps `apcore_cli::AuditLogger` for append-only JSONL audit logging
/// with salted SHA-256 input hashing for privacy.
pub struct AuditManager {
    logger: apcore_cli::AuditLogger,
    path: PathBuf,
}

impl AuditManager {
    /// Create a new `AuditManager` that writes to `audit_path`.
    pub fn new(audit_path: &Path) -> Self {
        Self {
            logger: apcore_cli::AuditLogger::new(Some(audit_path.to_path_buf())),
            path: audit_path.to_path_buf(),
        }
    }

    /// Log a module execution event.
    pub fn log_execution(
        &self,
        module_id: &str,
        input: &JsonValue,
        status: &str,
        exit_code: i32,
        duration_ms: u64,
    ) {
        self.logger
            .log_execution(module_id, input, status, exit_code, duration_ms);
    }

    /// Return the configured log file path.
    pub fn log_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_audit_manager_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        mgr.log_execution("cli.git.status", &json!({}), "success", 0, 10);

        assert!(path.exists());
    }

    #[test]
    fn test_audit_manager_appends_jsonl() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        mgr.log_execution("cli.git.status", &json!({}), "success", 0, 10);
        mgr.log_execution("cli.git.commit", &json!({"m": "hi"}), "error", 1, 25);

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_audit_manager_entry_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        mgr.log_execution("cli.git.push", &json!({"branch": "main"}), "success", 0, 42);

        let content = std::fs::read_to_string(&path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["module_id"], "cli.git.push");
        assert_eq!(entry["status"], "success");
        assert_eq!(entry["exit_code"], 0);
        assert_eq!(entry["duration_ms"], 42);
        assert!(entry["timestamp"].is_string());
        assert!(entry["input_hash"].is_string());
    }

    #[test]
    fn test_audit_manager_log_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        assert_eq!(mgr.log_path(), path);
    }
}
