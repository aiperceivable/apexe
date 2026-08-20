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
pub(crate) struct ExecutionRecord<'a> {
    /// RFC 3339 UTC, matching the ACL entry's format so one parser reads both.
    pub(crate) timestamp: String,
    /// Discriminates the record kinds sharing this file. ACL decisions carry
    /// `"acl_decision"`; this writer emits `"execution"` or `"refusal"`.
    pub(crate) event: &'static str,
    /// Correlates with the ACL entry's `trace_id` for the same call.
    ///
    /// Omitted rather than blank when the writer has none — the approval gate's
    /// `check_approval` path receives only an approval id, no context. A
    /// present-but-empty `trace_id` would claim the one join an audit trail
    /// exists to support while silently grouping every context-less refusal
    /// together, which is why `caller_id` is treated the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) trace_id: Option<&'a str>,
    /// Authenticated principal, or `None` for an unauthenticated caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) caller_id: Option<&'a str>,
    pub(crate) module_id: &'a str,
    /// `"success"`, `"error"` (the binary ran and failed) or `"refused"` (the
    /// call never reached the binary).
    pub(crate) status: &'a str,
    /// Absent on a refusal: no process existed to exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i32>,
    /// Failure class for a refusal, e.g. `ACL_DENIED`. Serialized through
    /// apcore's own `Serialize`, so the audit trail spells a code exactly as
    /// the protocol does. Never the error's *message*, which quotes the value
    /// it rejected and would put caller payload into a file that deliberately
    /// holds none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<ErrorCode>,
    pub(crate) duration_ms: u64,
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
            trace_id: (!trace_id.is_empty()).then_some(trace_id),
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
            trace_id: (!trace_id.is_empty()).then_some(trace_id),
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

    /// Append one already-serialized line as a single write.
    ///
    /// `what` names the record kind for the failure message. Best-effort
    /// throughout: an audit write that cannot land is logged and dropped rather
    /// than failing the caller's request, because refusing to serve because the
    /// log is unwritable is a worse outcome than serving unlogged — and the
    /// warning is what tells an operator to fix it.
    ///
    /// **One `write_all`, never `writeln!`.** `writeln!` on an unbuffered
    /// `File` issues two `write(2)` calls — the body, then the newline — and
    /// this manager takes `&self`, holds no lock, and is shared through one
    /// `Arc` by every `CliModule`, the failure-log middleware, the approval
    /// gate and the ACL callback. Two concurrent calls therefore emitted
    /// `bodyA bodyB \n \n`: one line holding two concatenated records, then a
    /// blank. Measured at 400 concurrent writes: 107 unparseable lines and 133
    /// blank ones. A single `write(2)` under `O_APPEND` is atomic for a regular
    /// file, which is the guarantee append-only JSONL relies on.
    fn append_line(&self, line: &str, what: &str) {
        // Keep the audit log owner-only so caller identities and denied targets
        // aren't world-readable on shared hosts. `mode()` applies only when the
        // file is created, which closes the window a post-open `set_permissions`
        // left open — and, unlike a per-write chmod, leaves an operator's own
        // later mode change (group-read for a log shipper, say) alone.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&self.path) {
            Ok(mut file) => {
                let mut record = String::with_capacity(line.len() + 1);
                record.push_str(line);
                record.push('\n');
                if let Err(e) = file.write_all(record.as_bytes()) {
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_concurrent_writers_each_get_their_own_line() {
        // One `Arc<AuditManager>` is shared by every `CliModule`, the failure-log
        // middleware, the approval gate and the ACL callback, and it takes
        // `&self` with no lock. `writeln!` on an unbuffered `File` is two
        // `write(2)` calls, so concurrent records used to interleave as
        // `bodyA bodyB \n \n` — a line holding two JSON objects, then a blank.
        // 107 of 400 lines were unparseable before the single-write fix. The
        // trail becoming unreadable under load is the one condition an audit
        // exists for, so this is pinned rather than left to inspection.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let audit = std::sync::Arc::new(AuditManager::new(&path));

        const WRITERS: usize = 400;
        let mut handles = Vec::with_capacity(WRITERS);
        for i in 0..WRITERS {
            let audit = audit.clone();
            handles.push(tokio::spawn(async move {
                audit.log_execution(
                    "cli.probe",
                    &format!("trace{i:040}"),
                    Some("@external"),
                    "success",
                    0,
                    1,
                );
            }));
        }
        for handle in handles {
            handle.await.expect("no writer task may panic");
        }

        let content = std::fs::read_to_string(&path).expect("the trail exists");
        assert!(
            content.lines().all(|line| !line.trim().is_empty()),
            "a blank line means a record's newline landed apart from its body"
        );
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            WRITERS,
            "every record must occupy exactly one line"
        );
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("every line must parse ({e}): {line}"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_the_trail_is_owner_only_from_its_very_first_write() {
        // The mode is set through `OpenOptions::mode`, which applies at
        // creation. A post-open `set_permissions` left the file world-readable
        // for the window in between — long enough for a local process on a
        // shared host to open it and keep a readable descriptor for its whole
        // life, which is exactly what the doc says the mode prevents.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        AuditManager::new(&path).log_execution("cli.ls", "t", None, "success", 0, 1);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the trail must never be group- or world-readable"
        );
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
