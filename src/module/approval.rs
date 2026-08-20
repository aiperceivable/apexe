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
const NO_PROMPT_REASONS: [&str; 3] = [
    "No context available for elicitation",
    "No elicitation callback available",
    "Elicitation returned no response",
];

/// What the gate does with one outcome from the upstream handler.
#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    /// A human approved. Pass it through untouched and audit nothing: the
    /// execution it permits is recorded on its own under the same `trace_id`.
    Granted,
    /// No prompt could be delivered at all. Replace the reason with one naming
    /// the remedy, and audit the refusal.
    NoPromptAvailable,
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
    if result.status == "approved" {
        return Disposition::Granted;
    }
    match result.reason.as_deref() {
        Some(reason) if NO_PROMPT_REASONS.contains(&reason) => Disposition::NoPromptAvailable,
        _ => Disposition::Refused,
    }
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
#[derive(Debug)]
pub struct ApprovalGate {
    /// The upstream handler that does the prompting.
    inner: ElicitationApprovalHandler,
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
        Self {
            // `None`: the callback is per-request and comes from the live
            // router, which is the whole point — see the module docs.
            inner: ElicitationApprovalHandler::new(None),
            audit,
        }
    }

    /// Record a refusal, using the trace and identity the request carries.
    fn audit_refusal(&self, request: &ApprovalRequest) {
        let Some(ref audit) = self.audit else {
            return;
        };
        // `duration_ms` is 0: the gate runs before anything is timed, and
        // inventing an elapsed time for a call that never started would be
        // worse than reporting none.
        audit.log_refusal(
            &request.module_id,
            request
                .context
                .as_ref()
                .map_or("", |ctx| ctx.trace_id.as_str()),
            request
                .context
                .as_ref()
                .and_then(|ctx| ctx.identity.as_ref())
                .map(|id| id.id()),
            ErrorCode::ApprovalDenied,
            0,
        );
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
                    "Approval granted through the connected MCP client"
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
            Disposition::Refused => {
                tracing::info!(
                    module_id = %request.module_id,
                    reason = ?result.reason,
                    "Approval refused"
                );
            }
        }
        self.audit_refusal(request);
        Ok(result)
    }

    async fn check_approval(&self, approval_id: &str) -> Result<ApprovalResult, ModuleError> {
        // Nothing is ever pending: `request_approval` resolves synchronously
        // against the live connection. Upstream says the same; deferring keeps
        // one answer rather than two.
        self.inner.check_approval(approval_id).await
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
