//! The approval gate `--enable-approval` installs.
//!
//! apcore-mcp 0.18 made a real human-in-the-loop prompt reachable from a
//! handler built outside its router, which is the shape apexe has: the CLI
//! entry point constructs the `Executor` long before any session exists. The
//! router now registers the connection's `ElicitCallback` per tool call and
//! writes its id into `Context.data` under `MCP_ELICIT_CALL_ID_KEY`, and
//! [`ElicitationApprovalHandler`] exchanges that id for the live callback. An
//! id is a `serde_json::Value` where a closure is not, which is what the
//! earlier design could not get past — apexe's `Context` could learn that
//! elicitation existed (`MCP_ELICIT_KEY` holds the string `"available"`) and
//! never perform one.
//!
//! So this wraps the upstream handler rather than replacing it, for two things
//! upstream cannot do from where it sits:
//!
//! * **Audit.** apcore runs `approval_gate` *before* `middleware_before`, so a
//!   denial here never reaches
//!   [`FailureLogMiddleware`](crate::module::FailureLogMiddleware). Nothing
//!   else in the stack observes it either — unlike an ACL denial, which apcore
//!   records itself. Without this the refusal a governed deployment produces
//!   most often would leave no trace.
//! * **A reason an operator can act on.** When the connected client declared no
//!   elicitation support there is no prompt to deliver, and upstream says so as
//!   `"Elicitation returned no response"`. That names the mechanism, not the
//!   remedy. An operator who turns the gate on and reads that on every
//!   destructive tool concludes the flag is broken and turns it back off,
//!   landing on the *ungoverned* default — strictly worse than either
//!   alternative. A human's own "no" is left verbatim; it is already clear.

use std::sync::Arc;

use apcore::approval::{ApprovalHandler, ApprovalRequest, ApprovalResult};
use apcore::{ErrorCode, ModuleError};
use apcore_mcp::ElicitationApprovalHandler;
use async_trait::async_trait;

/// The reasons upstream gives when no prompt could be delivered at all.
///
/// Distinct from a human declining, which needs no rewriting. Matching on
/// upstream's literals is deliberate but not load-bearing: if they change, the
/// gate falls through to reporting upstream's own reason, which is exactly
/// today's behaviour rather than something wrong.
/// `test_no_elicitation_path_is_reported_with_a_remedy` drives the real
/// handler, so drift fails the suite rather than passing silently.
const NO_PROMPT_REASONS: [&str; 2] = [
    "No context available for elicitation",
    "No elicitation callback available",
];

/// Upstream's reason when a prompt was sent and no answer came back.
///
/// Kept apart from [`NO_PROMPT_REASONS`] because it does **not** mean the client
/// declared no elicitation support: apcore-mcp returns it whenever the router's
/// callback yields `None`, which covers a transport failure, the client
/// disconnecting mid-prompt, and an unparseable answer. Telling an
/// elicitation-capable operator that their client lacks the capability sends
/// them to the wrong remedy — the same give-up-and-revert failure this module
/// exists to prevent.
const NO_ANSWER_REASON: &str = "Elicitation returned no response";

/// `module_id` recorded for a [`ApprovalGate::check_approval`] refusal.
///
/// `check_approval` receives only a caller-supplied, unvalidated token —
/// apcore dispatches to it straight from `tools/call` arguments, ahead of
/// `input_validation` — so that token must never become the audit record's
/// `module_id`: doing so would let any caller write an arbitrary string into
/// the field a governance record's identity depends on. The token itself is
/// still recorded, in `ExecutionRecord::approval_id`.
const APPROVAL_TOKEN_LOOKUP_MODULE_ID: &str = "<approval-token-lookup>";

/// What the gate does with one outcome from the upstream handler.
#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    /// A human approved. Pass it through untouched and audit nothing: the
    /// execution it permits is recorded on its own under the same `trace_id`.
    Granted,
    /// The decision is still out — an `ApprovalStore` recorded the request and
    /// someone will resolve it later. Not a refusal, so nothing is audited
    /// here; apcore raises `ApprovalPending` and the caller retries.
    Pending,
    /// No prompt could be delivered at all. Replace the reason with one naming
    /// the remedy, and audit the refusal.
    NoPromptAvailable,
    /// A prompt went out and nothing came back. Say so without asserting a
    /// cause, and audit the refusal.
    NoAnswer,
    /// A human refused, or something else went wrong. Their reason already says
    /// what happened; audit the refusal and leave it alone.
    Refused,
}

/// Decide what to do with `result`.
///
/// Split out from [`ApprovalGate::request_approval`] so the `Granted` arm is
/// reachable from a test. Driving the real handler can only ever produce a
/// rejection — a bare request carries no elicitation callback — so an
/// approval that was mistakenly audited as a refusal, which would put
/// `APPROVAL_DENIED` in the trail for a call that actually ran, was
/// undetectable while this was an `if` inside the trait method.
fn classify(result: &ApprovalResult) -> Disposition {
    match result.status.as_str() {
        "approved" => return Disposition::Granted,
        "pending" => return Disposition::Pending,
        _ => {}
    }
    match result.reason.as_deref() {
        Some(reason) if NO_PROMPT_REASONS.contains(&reason) => Disposition::NoPromptAvailable,
        Some(NO_ANSWER_REASON) => Disposition::NoAnswer,
        _ => Disposition::Refused,
    }
}

/// What a caller is told when a prompt went out and no answer came back.
fn no_answer_reason(module_id: &str) -> String {
    format!(
        "Module '{module_id}' is marked `requires_approval` and no answer to the approval prompt \
         came back. The client may not support MCP elicitation, or the connection may have \
         dropped before the prompt was answered — this is not a human declining. Retry, connect a \
         client that supports elicitation, use `--acl` to grant specific callers access to \
         specific modules, or embed apexe as a library with an `ApprovalStore` for out-of-band \
         approvals."
    )
}

/// What a caller is told when the gate is on and no prompt can be delivered.
fn no_prompt_reason(module_id: &str) -> String {
    format!(
        "Module '{module_id}' is marked `requires_approval` and this connection cannot be \
         prompted: the client declared no MCP elicitation support when it initialized, so there \
         is nobody to ask. This is a refusal for want of a prompt, not a human declining one. \
         Connect a client that supports elicitation, use `--acl` to grant specific callers access \
         to specific modules, or embed apexe as a library with an `ApprovalStore` for \
         out-of-band approvals."
    )
}

/// Prompt for approval through the connected MCP client, and record the answer.
///
/// Installed by [`build_executor`](crate::module::build_executor) when
/// `enable_approval` is set and no [`ApprovalStore`](apcore_mcp::ApprovalStore)
/// was supplied. See the module docs for what this adds over
/// [`ElicitationApprovalHandler`] alone.
pub struct ApprovalGate {
    /// The handler that actually decides. Boxed so the same audit-and-reword
    /// wrapper serves both handlers `build_executor` can install: the
    /// elicitation gate, and the `ApprovalStore`-backed one whose refusals
    /// reach no middleware either.
    inner: Box<dyn ApprovalHandler>,
    /// Governance audit sink. `None` disables auditing.
    ///
    /// Only refusals are written. A *granted* approval is followed by the
    /// execution it permitted, which `CliModule` records under the same
    /// `trace_id`, so the grant is already in the trail; a refusal is the
    /// outcome that would otherwise leave none.
    audit: Option<Arc<crate::governance::AuditManager>>,
}

impl ApprovalGate {
    /// Create the gate with no audit sink.
    pub fn new() -> Self {
        Self::with_audit(None)
    }

    /// Create the gate, recording each refusal to `audit`.
    pub fn with_audit(audit: Option<Arc<crate::governance::AuditManager>>) -> Self {
        // `None`: the callback is per-request and comes from the live router,
        // which is the whole point — see the module docs.
        Self::wrapping(Box::new(ElicitationApprovalHandler::new(None)), audit)
    }

    /// Wrap an arbitrary approval handler, auditing every refusal it returns.
    ///
    /// The audit guarantee is a property of *where the gate runs*, not of which
    /// handler decides: `approval_gate` precedes `middleware_before`, so no
    /// middleware observes any refusal from it. A store-backed deployment — the
    /// one the manual documents as the production answer — was getting the
    /// worse of the two guarantees while the audit sink sat unused one line
    /// away in `install_approval_handler`.
    pub fn wrapping(
        inner: Box<dyn ApprovalHandler>,
        audit: Option<Arc<crate::governance::AuditManager>>,
    ) -> Self {
        Self { inner, audit }
    }

    /// Record one approval-gate refusal.
    ///
    /// `duration_ms` is 0: the gate runs before anything is timed, and inventing
    /// an elapsed time for a call that never started would be worse than
    /// reporting none. `approval_id` is `Some` only from
    /// [`check_approval`](Self::check_approval)'s refusal path — see that
    /// method's doc comment and [`APPROVAL_TOKEN_LOOKUP_MODULE_ID`].
    async fn audit_refusal(
        &self,
        module_id: &str,
        trace_id: &str,
        caller_id: Option<&str>,
        approval_id: Option<&str>,
    ) {
        let Some(ref audit) = self.audit else {
            return;
        };
        audit
            .log_refusal(
                module_id,
                trace_id,
                caller_id,
                approval_id,
                ErrorCode::ApprovalDenied,
                0,
            )
            .await;
    }

    /// Record a refusal for a request that carries its own context.
    async fn audit_request_refusal(&self, request: &ApprovalRequest) {
        let context = request.context.as_ref();
        self.audit_refusal(
            &request.module_id,
            context.map_or("", |ctx| ctx.trace_id.as_str()),
            context
                .and_then(|ctx| ctx.identity.as_ref())
                .map(|id| id.id()),
            None,
        )
        .await;
    }
}

impl std::fmt::Debug for ApprovalGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalGate")
            .field("has_audit", &self.audit.is_some())
            .finish()
    }
}

impl Default for ApprovalGate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalHandler for ApprovalGate {
    async fn request_approval(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalResult, ModuleError> {
        let mut result = self.inner.request_approval(request).await?;
        match classify(&result) {
            Disposition::Granted => {
                tracing::info!(
                    module_id = %request.module_id,
                    "Approval granted"
                );
                return Ok(result);
            }
            Disposition::Pending => {
                tracing::info!(
                    module_id = %request.module_id,
                    "Approval recorded as pending for out-of-band resolution"
                );
                return Ok(result);
            }
            Disposition::NoPromptAvailable => {
                tracing::warn!(
                    module_id = %request.module_id,
                    "Denying approval-gated call: this client declared no elicitation support"
                );
                result.reason = Some(no_prompt_reason(&request.module_id));
            }
            Disposition::NoAnswer => {
                tracing::warn!(
                    module_id = %request.module_id,
                    "Denying approval-gated call: the prompt went out and no answer came back"
                );
                result.reason = Some(no_answer_reason(&request.module_id));
            }
            Disposition::Refused => {
                tracing::info!(
                    module_id = %request.module_id,
                    reason = ?result.reason,
                    "Approval refused"
                );
            }
        }
        self.audit_request_refusal(request).await;
        Ok(result)
    }

    /// Resolve a previously-issued approval id.
    ///
    /// **apcore reaches this from ordinary caller input.** When a `tools/call`'s
    /// arguments carry an `_approval_token` property, the approval gate calls
    /// this instead of [`request_approval`](Self::request_approval) — and it
    /// does so before `input_validation`, so nothing has rejected the extra
    /// property yet. Nothing is ever actually pending here (this gate resolves
    /// synchronously against the live connection), so the answer is always a
    /// refusal.
    ///
    /// That refusal has to be audited like any other. It reaches no middleware —
    /// `approval_gate` runs before `middleware_before`, so
    /// [`FailureLogMiddleware`](crate::module::FailureLogMiddleware) never sees
    /// it — so without this a caller suppressed the record of their own denied
    /// attempt by adding one property to the arguments object. Tool arguments
    /// arriving over MCP are external input.
    ///
    /// The record carries no trace or identity: an `ApprovalHandler` receives
    /// only the id on this path. That id is caller-supplied and unvalidated —
    /// `input_validation` has not run yet (see above) — so it is recorded in
    /// `approval_id`, never in `module_id`: writing unvalidated input into the
    /// field a governance record's identity depends on would let a caller
    /// frame an arbitrary module_id as having been denied.
    /// [`APPROVAL_TOKEN_LOOKUP_MODULE_ID`] is what `module_id` carries
    /// instead, on every record this method produces.
    async fn check_approval(&self, approval_id: &str) -> Result<ApprovalResult, ModuleError> {
        let result = self.inner.check_approval(approval_id).await?;
        if matches!(
            classify(&result),
            Disposition::Granted | Disposition::Pending
        ) {
            return Ok(result);
        }
        tracing::warn!(
            approval_id,
            reason = ?result.reason,
            "Refusing an approval-token lookup: this gate holds no pending approvals"
        );
        self.audit_refusal(APPROVAL_TOKEN_LOOKUP_MODULE_ID, "", None, Some(approval_id))
            .await;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(module_id: &str) -> ApprovalRequest {
        let mut request = ApprovalRequest::default();
        request.module_id = module_id.to_string();
        request
    }

    fn result_with(status: &str, reason: Option<&str>) -> ApprovalResult {
        let mut result = ApprovalResult::default();
        result.status = status.to_string();
        result.reason = reason.map(str::to_string);
        result
    }

    #[tokio::test]
    async fn test_a_granted_approval_is_never_recorded_as_a_refusal() {
        // The failure this guards is a falsified audit trail: an approved call
        // audited as a refusal writes APPROVAL_DENIED for a call that actually
        // ran, so the trail reports the opposite of what happened.
        assert_eq!(
            classify(&result_with("approved", None)),
            Disposition::Granted
        );
    }

    #[tokio::test]
    async fn test_a_humans_refusal_keeps_its_own_reason() {
        // "User action: decline" already says what happened. Rewriting it would
        // replace a true statement with a generic one.
        assert_eq!(
            classify(&result_with("rejected", Some("User action: decline"))),
            Disposition::Refused
        );
    }

    #[tokio::test]
    async fn test_an_undeliverable_prompt_is_distinguished_from_a_refusal() {
        for reason in NO_PROMPT_REASONS {
            assert_eq!(
                classify(&result_with("rejected", Some(reason))),
                Disposition::NoPromptAvailable,
                "{reason} means no prompt reached anyone"
            );
        }
    }

    #[tokio::test]
    async fn test_an_unanswered_prompt_does_not_claim_the_client_lacks_the_capability() {
        // apcore-mcp returns this whenever its callback yields `None`, which
        // covers a transport failure and a mid-prompt disconnect as well as a
        // client with no elicitation support. Folding it in with the other two
        // told an elicitation-capable operator the opposite of what their own
        // `initialize` handshake said, and sent them to the wrong remedy.
        assert_eq!(
            classify(&result_with("rejected", Some(NO_ANSWER_REASON))),
            Disposition::NoAnswer
        );
        let unanswered = no_answer_reason("cli.rm");
        let unpromptable = no_prompt_reason("cli.rm");
        assert_ne!(
            unanswered, unpromptable,
            "the two outcomes must not read as the same diagnosis"
        );
        assert!(
            !unanswered.contains("declared no MCP elicitation support"),
            "an unanswered prompt must not assert a cause it did not observe: {unanswered}"
        );
        assert!(
            unanswered.contains("--acl"),
            "it must still name the alternative: {unanswered}"
        );
    }

    #[tokio::test]
    async fn test_no_elicitation_path_is_reported_with_a_remedy() {
        // A bare `ApprovalRequest` carries no context and therefore no live
        // callback id, which is what a client declaring no elicitation support
        // produces. Upstream answers "No context available for elicitation";
        // that names the mechanism and no way out of it.
        //
        // This drives the real upstream handler, so it is also the guard on
        // `NO_PROMPT_REASONS`: if apcore-mcp rewords its refusal, this fails
        // rather than silently passing the raw string through.
        let result = ApprovalGate::new()
            .request_approval(&request("cli.rm"))
            .await
            .expect("the gate answers rather than erroring");

        assert_eq!(result.status, "rejected");
        let reason = result.reason.expect("a reason must be attached");
        // The three things an operator needs: which module, why no prompt
        // arrived, and what to reach for instead.
        assert!(
            reason.contains("cli.rm"),
            "reason names the module: {reason}"
        );
        assert!(
            reason.contains("no MCP elicitation support"),
            "reason says why no prompt arrived: {reason}"
        );
        assert!(
            reason.contains("--acl"),
            "reason names the alternative: {reason}"
        );
    }

    #[tokio::test]
    async fn test_refusal_reaches_the_audit_trail() {
        // The approval gate runs ahead of the middleware phase, so a refusal is
        // invisible to `FailureLogMiddleware` and apcore emits no audit entry
        // of its own for it — unlike an ACL denial.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let gate =
            ApprovalGate::with_audit(Some(Arc::new(crate::governance::AuditManager::new(&path))));

        gate.request_approval(&request("cli.rm"))
            .await
            .expect("the gate answers rather than erroring");

        let content = std::fs::read_to_string(&path).expect("a refusal must be recorded");
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["event"], "refusal");
        assert_eq!(entry["module_id"], "cli.rm");
        assert_eq!(entry["error_code"], "APPROVAL_DENIED");
    }

    #[tokio::test]
    async fn test_an_approval_token_lookup_is_audited_like_any_other_refusal() {
        // apcore calls `check_approval` instead of `request_approval` whenever
        // the arguments carry `_approval_token`, and it does so before
        // `input_validation` — so nothing has rejected the extra property yet.
        // The refusal reaches no middleware either (`approval_gate` precedes
        // `middleware_before`), so before this a caller suppressed the record of
        // their own denied attempt by adding one property to the object.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");
        let gate =
            ApprovalGate::with_audit(Some(Arc::new(crate::governance::AuditManager::new(&path))));

        let result = gate
            .check_approval("caller-supplied-token")
            .await
            .expect("the gate answers rather than erroring");
        assert_eq!(result.status, "rejected");

        let content = std::fs::read_to_string(&path).expect("a refusal must be recorded");
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["event"], "refusal");
        assert_eq!(entry["error_code"], "APPROVAL_DENIED");
        // Regression: the caller-supplied token used to be written straight
        // into `module_id` -- apcore reads `_approval_token` from `tools/call`
        // arguments and dispatches here *before* `input_validation`, so a
        // caller could write an arbitrary string into the governance trail's
        // `module_id` field (e.g. framing another module as having been
        // denied). `module_id` must stay a fixed sentinel; the caller's token
        // is recorded separately, where it cannot be mistaken for one.
        assert_eq!(
            entry["module_id"], "<approval-token-lookup>",
            "a caller-supplied token must never become the audit record's module_id: {entry}"
        );
        assert_eq!(
            entry["approval_id"], "caller-supplied-token",
            "the token must still be recorded, just not as module_id: {entry}"
        );
        assert!(
            entry.get("trace_id").is_none(),
            "no context reaches this path, so the join key is omitted rather than \
             claimed blank: {entry}"
        );
    }

    #[tokio::test]
    async fn test_refusal_without_an_audit_sink_writes_nothing() {
        // `--audit` off must stay off; the gate still refuses.
        let result = ApprovalGate::new()
            .request_approval(&request("cli.rm"))
            .await
            .expect("the gate answers rather than erroring");
        assert_eq!(result.status, "rejected");
    }

    #[tokio::test]
    async fn test_check_approval_reports_nothing_pending() {
        let result = ApprovalGate::new()
            .check_approval("any-id")
            .await
            .expect("the gate answers rather than erroring");
        assert_eq!(result.status, "rejected");
    }
}
