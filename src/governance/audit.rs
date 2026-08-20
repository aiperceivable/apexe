use apcore::{AuditEntry, ErrorCode};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One line of `audit.jsonl`, whichever kind of event produced it.
///
/// # Why apexe writes this instead of delegating
///
/// The execution half used to come from `apcore_cli::AuditLogger`, whose record
/// is `{timestamp, user, module_id, input_hash, status, exit_code, duration_ms}`.
/// Three of those fields did not survive scrutiny, and the shape as a whole
/// could not be joined to the ACL decisions apexe writes into the same file:
///
/// * **No `trace_id`.** The ACL entry carries one and the execution record did
///   not, so "which execution did this allow-decision permit?" had no answer —
///   the one join an audit trail exists to support.
/// * **Two timestamp formats** in one file (`…+00:00` for ACL entries, `…Z` for
///   executions), which a consumer had to special-case per record kind.
/// * **`user` was the wrong actor.** apcore-cli resolves it
///   `getlogin() → getpwuid(geteuid) → $USER → …`, and `getlogin()` names the
///   controlling terminal's owner — reported as `root` on a host whose process
///   ran as uid 501. On a served surface the OS user is the *server process*
///   owner in any case, never the caller, so the field answered a question
///   nobody asked while looking like it answered "who did this".
/// * **`input_hash` was unverifiable.** `hash_input` salts with 16 fresh random
///   bytes per call and then discards them, so the digest is reproducible from
///   a known input by nobody — not even apcore-cli — and two records of the
///   same input never match. It is dropped rather than kept as misleading
///   precision; `audit.jsonl` deliberately holds no argument values, so there
///   is nothing for a hash to stand in for.
///
/// What replaces them is `caller_id` — the authenticated principal, which is
/// the actor an operator is actually asking about — and `trace_id` on every
/// record, in one timestamp format.
///
/// Note this does *not* change apcore's `/usage` `unique_callers`, which counts
/// `Context::caller_id`. That field names the calling *module* in a nested call
/// chain and is `None` (reported as `@external`) for every inbound request by
/// apcore's own contract, so it stays anonymous by design and not for want of
/// this record.
#[derive(Debug, Serialize)]
pub struct ExecutionRecord<'a> {
    /// RFC 3339 UTC, matching the ACL entry's format so one parser reads both.
    pub timestamp: String,
    /// Discriminates the record kinds sharing this file. ACL decisions carry
    /// `"acl_decision"`; this writer emits `"execution"` or `"refusal"`.
    pub event: &'static str,
    /// Correlates with the ACL entry's `trace_id` for the same call.
    pub trace_id: &'a str,
    /// Authenticated principal, or `None` for an unauthenticated caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<&'a str>,
    pub module_id: &'a str,
    /// `"success"`, `"error"` (the binary ran and failed) or `"refused"` (the
    /// call never reached the binary).
    pub status: &'a str,
    /// Absent on a refusal: no process existed to exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Failure class for a refusal, e.g. `ACL_DENIED`. Serialized through
    /// apcore's own `Serialize`, so the audit trail spells a code exactly as
    /// the protocol does. Never the error's *message*, which quotes the value
    /// it rejected and would put caller payload into a file that deliberately
    /// holds none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    pub duration_ms: u64,
}

/// Appends governance events to a JSONL audit log.
///
/// Both kinds of record — apcore's ACL decisions and apexe's own execution and
/// refusal records — go to one file in one shape family; see
/// [`ExecutionRecord`] for why apexe writes the latter itself.
#[derive(Debug)]
pub struct AuditManager {
    path: PathBuf,
}

impl AuditManager {
    /// Create a new `AuditManager` that writes to `audit_path`.
    pub fn new(audit_path: &Path) -> Self {
        Self {
            path: audit_path.to_path_buf(),
        }
    }

    /// Record a call that reached the wrapped binary.
    pub fn log_execution(
        &self,
        module_id: &str,
        trace_id: &str,
        caller_id: Option<&str>,
        status: &str,
        exit_code: i32,
        duration_ms: u64,
    ) {
        self.append(&ExecutionRecord {
            timestamp: Self::now(),
            event: "execution",
            trace_id,
            caller_id,
            module_id,
            status,
            exit_code: Some(exit_code),
            error_code: None,
            duration_ms,
        });
    }

    /// Record a call refused before the wrapped binary ran.
    ///
    /// This is the event the trail most needed and did not have: nothing that
    /// fails before [`CliModule`](crate::module::CliModule) was recorded at
    /// all, so someone probing the argv guards — trying `--output=/etc/passwd`,
    /// then `-K`, then a newline — left no trace of having done so.
    ///
    /// Two callers reach it, because apcore's pipeline refuses in two places
    /// and only one of them is observable from a middleware:
    ///
    /// * [`FailureLogMiddleware`](crate::module::FailureLogMiddleware) for
    ///   anything at or after `middleware_before` — schema rejections, the argv
    ///   guards, timeouts, spawn failures.
    /// * [`ApprovalGate`](crate::module::ApprovalGate) for the
    ///   approval gate, which runs *ahead* of the middleware phase and so never
    ///   reaches a middleware's `on_error` at all.
    ///
    /// An **ACL denial** deliberately does not come through here: it aborts at
    /// the `acl_check` step, equally out of a middleware's reach, but apcore
    /// already reports it through [`log_acl_decision`](Self::log_acl_decision)
    /// with the same `trace_id` and a richer field set (`matched_rule`,
    /// `matched_rule_index`, `roles`). Emitting a second row for it would
    /// double-count denials for anyone tallying the file.
    pub fn log_refusal(
        &self,
        module_id: &str,
        trace_id: &str,
        caller_id: Option<&str>,
        error_code: ErrorCode,
        duration_ms: u64,
    ) {
        self.append(&ExecutionRecord {
            timestamp: Self::now(),
            event: "refusal",
            trace_id,
            caller_id,
            module_id,
            status: "refused",
            exit_code: None,
            error_code: Some(error_code),
            duration_ms,
        });
    }

    /// RFC 3339 UTC with a trailing `Z`, matching apcore's ACL entry format.
    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Serialize and append one record, best-effort.
    fn append<T: Serialize>(&self, record: &T) {
        let line = match serde_json::to_string(record) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize audit record");
                return;
            }
        };
        self.append_line(&line, "audit record");
    }

    /// Append an ACL allow/deny decision to the same audit log (JSONL).
    ///
    /// apcore's `ACL::set_audit_logger` hands us an [`AuditEntry`] on every
    /// governance decision; recording it here is what lets an operator answer
    /// "who was denied which module, and when". Best-effort: a write failure is
    /// logged and dropped rather than failing the request.
    pub fn log_acl_decision(&self, entry: &AuditEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize ACL audit entry");
                return;
            }
        };
        self.append_line(&line, "ACL audit entry");
    }

    /// Append one already-serialized line, hardening the file's mode first.
    ///
    /// `what` names the record kind for the failure message. Best-effort
    /// throughout: an audit write that cannot land is logged and dropped rather
    /// than failing the caller's request, because refusing to serve because the
    /// log is unwritable is a worse outcome than serving unlogged — and the
    /// warning is what tells an operator to fix it.
    fn append_line(&self, line: &str, what: &str) {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                // Keep the audit log owner-only so caller identities and denied
                // targets aren't world-readable on shared hosts. Either record
                // kind can be the file's first write, so this must happen here
                // rather than at one privileged creation site.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &self.path,
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
                if let Err(e) = writeln!(file, "{line}") {
                    tracing::warn!(error = %e, "Failed to append {what}");
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to open audit log for {what}"),
        }
    }

    /// Return the configured log file path.
    pub fn log_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the audit file back as one parsed record per line.
    fn records(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .expect("audit file should exist")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line must be valid JSON"))
            .collect()
    }

    #[test]
    fn test_audit_manager_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");

        AuditManager::new(&path).log_execution("cli.git.status", "t1", None, "success", 0, 10);

        assert!(path.exists());
    }

    #[test]
    fn test_audit_manager_appends_jsonl() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        mgr.log_execution("cli.git.status", "t1", None, "success", 0, 10);
        mgr.log_execution("cli.git.commit", "t2", None, "error", 1, 25);

        assert_eq!(records(&path).len(), 2);
    }

    #[test]
    fn test_log_execution_names_the_call_and_its_trace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");

        AuditManager::new(&path).log_execution(
            "cli.git.push",
            "trace-abc",
            Some("apexe-token"),
            "success",
            0,
            42,
        );

        let entry = records(&path).remove(0);
        assert_eq!(entry["event"], "execution");
        assert_eq!(entry["module_id"], "cli.git.push");
        assert_eq!(entry["status"], "success");
        assert_eq!(entry["exit_code"], 0);
        assert_eq!(entry["duration_ms"], 42);
        // The two fields the previous record shape lacked, and the reason it
        // could not be joined to the ACL decision that permitted the call.
        assert_eq!(entry["trace_id"], "trace-abc");
        assert_eq!(entry["caller_id"], "apexe-token");
        assert!(entry["timestamp"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn test_log_refusal_records_a_call_that_never_ran() {
        // Regression for the audit gap: a schema rejection, an option-injection
        // refusal, an ACL denial and an approval denial all end before
        // `CliModule`, so the trail used to hold nothing at all for them.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");

        AuditManager::new(&path).log_refusal(
            "cli.cp",
            "trace-xyz",
            Some("u1"),
            ErrorCode::ACLDenied,
            3,
        );

        let entry = records(&path).remove(0);
        assert_eq!(entry["event"], "refusal");
        assert_eq!(entry["status"], "refused");
        assert_eq!(entry["module_id"], "cli.cp");
        assert_eq!(entry["trace_id"], "trace-xyz");
        assert_eq!(entry["caller_id"], "u1");
        // Spelled the way the protocol spells it, via apcore's own Serialize.
        assert_eq!(entry["error_code"], "ACL_DENIED");
        // No process ran, so there is no exit status to invent.
        assert!(entry["exit_code"].is_null());
    }

    #[test]
    fn test_anonymous_caller_omits_the_field_rather_than_naming_nobody() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");

        AuditManager::new(&path).log_execution("cli.ls", "t1", None, "success", 0, 1);

        let entry = records(&path).remove(0);
        assert!(
            entry.get("caller_id").is_none(),
            "an unauthenticated call must leave the field absent, not record a \
             placeholder that reads like an identity: {entry}"
        );
    }

    #[test]
    fn test_both_record_kinds_share_one_file_and_one_timestamp_format() {
        // The join an audit trail exists to support: an ACL decision and the
        // execution it permitted, correlated by `trace_id`, parsed by one
        // reader.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        // `AuditEntry` is apcore's own wire type and has no `Default`; build it
        // through serde so this test cannot drift from the shape apcore emits.
        let decision: AuditEntry = serde_json::from_value(serde_json::json!({
            "timestamp": "2026-08-20T00:00:00.000Z",
            "caller_id": "u1",
            "target_id": "cli.ls",
            "decision": "allow",
            "reason": "matched rule 0",
            "trace_id": "shared-trace",
        }))
        .expect("AuditEntry should deserialize from its own wire shape");
        mgr.log_acl_decision(&decision);
        mgr.log_execution("cli.ls", "shared-trace", Some("u1"), "success", 0, 7);

        let entries = records(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["trace_id"], entries[1]["trace_id"]);
        for entry in &entries {
            assert!(
                entry["timestamp"].as_str().unwrap().ends_with('Z'),
                "one timestamp format across both record kinds: {entry}"
            );
        }
    }

    #[test]
    fn test_audit_manager_log_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let mgr = AuditManager::new(&path);

        assert_eq!(mgr.log_path(), path);
    }
}
