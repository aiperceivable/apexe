//! The approval gate apexe can actually honour on a CLI-launched server.
//!
//! apcore-mcp ships [`ElicitationApprovalHandler`], which asks the connected
//! MCP client to prompt a human. Constructing it requires an `ElicitCallback`,
//! and that callback only exists inside apcore-mcp's router, where it is put
//! into a side-channel map of `Box<dyn Any>` keyed by `MCP_ELICIT_KEY`. The
//! `ApprovalRequest` that reaches an [`ApprovalHandler`] carries only the JSON
//! `Context`, whose `data` map holds the string `"available"` rather than the
//! callback itself — apcore-mcp documents this as a Rust-specific limitation
//! (`adapters/approval.rs`, D10-001). There is no path from an
//! externally-constructed handler to the transport, so
//! `ElicitationApprovalHandler::new(None)` — which is all apexe could build —
//! rejects every request with `"No elicitation callback available"`.
//!
//! Fail-closed is the right outcome; the opaque reason is not. An operator who
//! turns the gate on and gets a generic rejection on every destructive tool
//! concludes the flag is broken and turns it back off, landing on the
//! *ungoverned* default — strictly worse than either alternative. So apexe
//! states what happened and what to reach for instead.

use apcore::approval::{ApprovalHandler, ApprovalRequest, ApprovalResult};
use apcore::ModuleError;
use async_trait::async_trait;

/// What a caller is told when a `requires_approval` module is refused.
///
/// Names the module, says the block is unconditional rather than a rejected
/// prompt, and points at the two mechanisms that do work.
fn denial_reason(module_id: &str) -> String {
    format!(
        "Module '{module_id}' is marked `requires_approval` and this server denies every such \
         call: interactive approval is unavailable because apcore-mcp's elicitation callback \
         cannot be reached from an approval handler constructed outside its router. This is a \
         hard block, not a human declining the prompt. Use `--acl` to grant specific callers \
         access to specific modules, or embed apexe as a library with an `ApprovalStore` for \
         out-of-band approvals."
    )
}

/// Build a rejection carrying `reason`.
///
/// [`ApprovalResult`] is `#[non_exhaustive]`, so it can only be assembled from
/// its `Default` rather than by struct literal.
fn rejected(reason: String) -> ApprovalResult {
    let mut result = ApprovalResult::default();
    result.status = "rejected".to_string();
    result.reason = Some(reason);
    result
}

/// Deny every call to a module annotated `requires_approval`, with a reason
/// that says so.
///
/// Installed by [`build_executor`](crate::module::build_executor) when
/// `enable_approval` is set and no [`ApprovalStore`](apcore_mcp::ApprovalStore)
/// was supplied. See the module docs for why no interactive path exists.
#[derive(Debug, Clone, Default)]
pub struct DenyApprovalHandler;

impl DenyApprovalHandler {
    /// Create the handler.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ApprovalHandler for DenyApprovalHandler {
    async fn request_approval(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalResult, ModuleError> {
        tracing::warn!(
            module_id = %request.module_id,
            "Denying approval-gated call: no interactive approval path on this transport"
        );
        Ok(rejected(denial_reason(&request.module_id)))
    }

    async fn check_approval(&self, _approval_id: &str) -> Result<ApprovalResult, ModuleError> {
        // Nothing is ever pending: `request_approval` resolves synchronously.
        Ok(rejected(
            "This server has no pending approvals; every approval-gated call is denied \
             immediately."
                .to_string(),
        ))
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

    #[tokio::test]
    async fn test_request_approval_rejects_with_actionable_reason() {
        let result = DenyApprovalHandler::new()
            .request_approval(&request("cli.cp"))
            .await
            .expect("handler resolves rather than erroring");

        assert_eq!(result.status, "rejected");
        let reason = result.reason.expect("a reason must be attached");
        // The three things an operator needs: which module, that it is
        // unconditional, and what to use instead.
        assert!(
            reason.contains("cli.cp"),
            "reason names the module: {reason}"
        );
        assert!(
            reason.contains("hard block"),
            "reason states the block is unconditional: {reason}"
        );
        assert!(
            reason.contains("--acl"),
            "reason names the alternative: {reason}"
        );
    }

    #[tokio::test]
    async fn test_check_approval_reports_nothing_pending() {
        let result = DenyApprovalHandler::new()
            .check_approval("any-id")
            .await
            .expect("handler resolves rather than erroring");
        assert_eq!(result.status, "rejected");
    }
}
