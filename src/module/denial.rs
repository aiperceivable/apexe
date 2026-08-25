//! Carry a governance refusal's reason through a transport that would
//! otherwise flatten it.
//!
//! apcore-a2a decides what a caller is told from the error code alone
//! (`adapters::errors::ErrorMapper::to_jsonrpc_error`, and the
//! `carries_caller_detail` partition that gates the same text into
//! `status.message`). Three refusal codes lose their reason there:
//!
//! | apcore code        | what an A2A caller is told |
//! |--------------------|----------------------------|
//! | `ACL_DENIED`       | `Task not found` (`-32001`) |
//! | `APPROVAL_DENIED`  | `Internal server error` (`-32603`) |
//! | `APPROVAL_TIMEOUT` | `Internal server error` (`-32603`) |
//!
//! All three are *actionable in the wrong direction*. `Task not found` names
//! the one thing that was fine — the task id — so an agent retries, or resends
//! with a fresh message id, and is refused identically for as long as it keeps
//! trying. `Internal server error` says the server broke, so an agent backs off
//! and retries that too. The correct response to all three is to stop and pick
//! a different skill, and none of the three messages says so.
//!
//! The masking is deliberate upstream — it withholds the reason so an
//! unauthorized caller cannot enumerate what exists, the argument that returns
//! HTTP 404 instead of 403 — and apexe kept it in 0.6.1 rather than diverge
//! from a convention shared with apcore-a2a's Python and TypeScript siblings.
//! That trade does not survive contact with apexe's A2A server:
//!
//! * The enumeration it guards against is already impossible. apcore-a2a
//!   ACL-filters the agent card per caller (`server::handlers::FilteredCard`),
//!   so a denied skill is absent from the card. A caller that names one
//!   already knows the id from somewhere else, and the refusal confirms
//!   nothing the card did not already settle.
//! * `apexe a2a` wires no authenticator, so every caller is the same anonymous
//!   `@external` principal and there is no privileged caller for the masking to
//!   keep a secret *from*. (apcore-a2a does ship one — `Authenticator`,
//!   `JWTAuthenticator` and `async_serve_with_auth` — apexe simply calls the
//!   no-auth `async_serve` and offers no `--auth` on this subcommand. So this
//!   is a property of how apexe serves A2A today, not a limit of the crate.)
//! * The cost is paid by the agent apexe exists to serve, which cannot tell
//!   "you are not allowed" from "that id does not exist" — two failures whose
//!   correct next moves are opposite.
//!
//! So the reason is relayed rather than the mapping overridden: this runs
//! inside the apcore pipeline, before apcore-a2a sees anything, and re-codes
//! the refusal to [`ErrorCode::GeneralInvalidInput`] — the one code upstream
//! forwards a message for that does not additionally assert something false
//! about *where* the problem is. (`MODULE_NOT_FOUND` would claim the skill does
//! not exist, which is the misdirection being fixed; `SCHEMA_VALIDATION_ERROR`
//! would send the caller off to rewrite a payload that was correct.) The
//! original code is preserved verbatim at the front of the message, so the two
//! transports now read alike:
//!
//! ```text
//! MCP: [ACLDenied] Access denied: caller 'None' cannot access module 'cli.ls'
//! A2A: Invalid input: [ACLDenied] Access denied: caller 'None' cannot access
//!      module 'cli.ls' (An access-control rule refuses this call. ...)
//! ```
//!
//! Installed only on the A2A executor
//! ([`ExecutorOptions::relay_denial_reason`](crate::module::ExecutorOptions::relay_denial_reason)).
//! Over MCP the reason already reaches the caller intact, so relaying there
//! would replace an accurate `[ACLDenied]` with a re-coded one for no gain.

use apcore::pipeline::{PipelineState, StepMiddleware};
use apcore::{ErrorCode, ModuleError};
use async_trait::async_trait;
use serde_json::Value;

/// The apcore pipeline step that refuses a call on ACL policy.
const ACL_CHECK_STEP: &str = "acl_check";

/// The apcore pipeline step that refuses a call on a human approval decision.
const APPROVAL_GATE_STEP: &str = "approval_gate";

/// What to tell a caller whose call an ACL rule refused.
///
/// Written for the agent that reads it: the first sentence names the class of
/// failure, the rest rules out the two retries the masked message provoked.
const ACL_DENIED_GUIDANCE: &str = "An access-control rule refuses this call. \
     The task id is fine — retrying it, or resending with a new message id, \
     produces the same refusal. Pick a different skill, or ask the operator to \
     change the ACL policy.";

/// What to tell a caller whose call a human reviewer refused.
const APPROVAL_DENIED_GUIDANCE: &str = "A human reviewer refused this call. \
     The server is healthy and the request was well-formed — retrying produces \
     the same refusal until the decision changes. Pick a different skill, or \
     ask the operator to approve it.";

/// What to tell a caller whose approval request expired unanswered.
///
/// Unlike the two denials this one is genuinely retryable, so the guidance says
/// so rather than telling the agent to give up.
const APPROVAL_TIMEOUT_GUIDANCE: &str = "No reviewer answered the approval \
     request before it expired, so the call never ran. Nothing failed on the \
     server. Retrying is worthwhile only once a reviewer is available.";

/// Guidance for a refusal whose reason apcore-a2a's mapper withholds, or `None`
/// for every code it already reports faithfully.
///
/// Deliberately narrow. `APPROVAL_PENDING` is excluded because apcore-a2a
/// already handles it correctly and differently — `error_to_status` maps it to
/// `TaskState::InputRequired` carrying the error's own message verbatim, which
/// is how a caller learns the `approval_id` it must resume with. Re-coding it
/// would turn a resumable pause into a terminal failure.
fn withheld_reason_guidance(code: ErrorCode) -> Option<&'static str> {
    match code {
        ErrorCode::ACLDenied => Some(ACL_DENIED_GUIDANCE),
        ErrorCode::ApprovalDenied => Some(APPROVAL_DENIED_GUIDANCE),
        ErrorCode::ApprovalTimeout => Some(APPROVAL_TIMEOUT_GUIDANCE),
        _ => None,
    }
}

/// Whether a refusal may be retried, per apcore's cross-language error-recovery
/// contract (`apcore/conformance/fixtures/error_recovery_metadata.json`).
///
/// A denial is terminal; an approval *timeout* is not — nobody answered, so the
/// same request may succeed once a reviewer is available. That distinction is
/// pinned across the SDKs, and apcore-python / apcore-typescript resolve it from
/// a per-class `DEFAULT_RETRYABLE`.
///
/// It is restated here because apcore-rust does not: it has one `ModuleError`
/// keyed by `ErrorCode` rather than an error-class hierarchy, ships a
/// `user_fixable_for_code` policy with no `retryable_for_code` counterpart, and
/// so leaves `retryable` unset on every governance refusal. Relaying a blanket
/// `false` would have contradicted the contract for `APPROVAL_TIMEOUT` — and
/// this file's own guidance string, which tells the caller that one *is* worth
/// retrying.
fn refusal_is_retryable(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::ApprovalTimeout)
}

/// Re-code a governance refusal so its reason survives the A2A error mapper.
///
/// See the [module documentation](self) for why this exists and why it is
/// installed on the A2A executor only.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenialReasonRelay;

impl DenialReasonRelay {
    /// Build the relayed error for `original`, or `None` when the code is one
    /// apcore-a2a already reports faithfully.
    ///
    /// Split out from the hook so the mapping can be asserted directly, without
    /// standing up a pipeline.
    pub fn relay(original: &ModuleError) -> Option<ModuleError> {
        let guidance = withheld_reason_guidance(original.code)?;

        // `{:?}` rather than the serialized `ACL_DENIED` spelling: apcore-mcp
        // prefixes the variant name, and the point of the prefix is that an
        // operator comparing the two transports sees the same token.
        let mut relayed = ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!("[{:?}] {}", original.code, original.message),
        )
        // Carry the recovery contract rather than a blanket `false`: apcore
        // leaves `retryable` unset on these codes in Rust, and an approval
        // timeout is retryable where the two denials are not. See
        // [`refusal_is_retryable`].
        .with_retryable(refusal_is_retryable(original.code))
        .with_cause(format!(
            "{:?} relayed as GeneralInvalidInput so the reason reaches an A2A caller",
            original.code
        ))
        .with_details(original.details.clone());

        relayed.ai_guidance = Some(match original.ai_guidance.as_deref() {
            Some(existing) if !existing.trim().is_empty() => format!("{existing} {guidance}"),
            _ => guidance.to_string(),
        });
        if let Some(trace_id) = original.trace_id.clone() {
            relayed = relayed.with_trace_id(trace_id);
        }
        Some(relayed)
    }
}

#[async_trait]
impl StepMiddleware for DenialReasonRelay {
    /// Replace a masked refusal with a relayed one.
    ///
    /// Returning `Err` here is what substitutes the error:
    /// `PipelineEngine::run_with_options` wraps it as a `PipelineStepError` and
    /// `Executor::execute` unwraps that back to this exact `ModuleError` before
    /// returning it, so apcore-a2a's mapper sees the relayed code. Returning
    /// `Ok(Some(_))` would instead *recover* the call — the refusal would
    /// succeed with a fabricated output, which is the one outcome a governance
    /// control must never produce.
    ///
    /// Scoped to the two governance steps by name so an unrelated step that
    /// happens to raise one of these codes is left alone, and so the
    /// `before_step` unwind path — which reports a `MIDDLEWARE_CHAIN_ERROR`
    /// and ignores whatever this returns — never matches.
    async fn on_step_error(
        &self,
        step_name: &str,
        _state: &PipelineState<'_>,
        error: &ModuleError,
    ) -> Result<Option<Value>, ModuleError> {
        if step_name != ACL_CHECK_STEP && step_name != APPROVAL_GATE_STEP {
            return Ok(None);
        }
        let Some(relayed) = Self::relay(error) else {
            return Ok(None);
        };
        // The relayed error carries a different code, and apcore-a2a logs the
        // struct it is handed. State the real one here so an operator reading
        // the server log is not left to infer it from the message prefix.
        tracing::warn!(
            step = %step_name,
            error_code = ?error.code,
            relayed_as = ?relayed.code,
            trace_id = error.trace_id.as_deref().unwrap_or(""),
            "Governance refusal relayed so the A2A caller learns the reason"
        );
        Err(relayed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(code: ErrorCode, message: &str) -> ModuleError {
        ModuleError::new(code, message)
    }

    #[test]
    fn test_relay_prefixes_an_acl_denial_with_its_original_code() {
        let relayed = DenialReasonRelay::relay(&refusal(
            ErrorCode::ACLDenied,
            "Access denied: caller 'None' cannot access module 'cli.ls'",
        ))
        .expect("an ACL denial must be relayed");

        assert_eq!(relayed.code, ErrorCode::GeneralInvalidInput);
        assert_eq!(
            relayed.message,
            "[ACLDenied] Access denied: caller 'None' cannot access module 'cli.ls'"
        );
        assert_eq!(relayed.retryable, Some(false));
    }

    #[test]
    fn test_relay_matches_apcores_retryable_contract() {
        // `error_recovery_metadata.json` pins these across the SDKs:
        // ACL_DENIED and APPROVAL_DENIED are terminal, APPROVAL_TIMEOUT is not.
        // apcore-rust leaves `retryable` unset on all three (it has no
        // `retryable_for_code` to match its `user_fixable_for_code`), so a
        // blanket `false` here would have contradicted the contract — and the
        // guidance this file attaches to a timeout, which says to retry once a
        // reviewer is available.
        for (code, expected) in [
            (ErrorCode::ACLDenied, Some(false)),
            (ErrorCode::ApprovalDenied, Some(false)),
            (ErrorCode::ApprovalTimeout, Some(true)),
        ] {
            let relayed = DenialReasonRelay::relay(&refusal(code, "refused"))
                .unwrap_or_else(|| panic!("{code:?} must be relayed"));
            assert_eq!(
                relayed.retryable, expected,
                "{code:?} must carry apcore's documented retryable default"
            );
        }
    }

    #[test]
    fn test_relay_tells_an_agent_that_retrying_a_denial_is_pointless() {
        // The whole defect: `Task not found` sends an agent back to retry with
        // a different id. The guidance has to close that door explicitly.
        let relayed = DenialReasonRelay::relay(&refusal(ErrorCode::ACLDenied, "denied"))
            .expect("an ACL denial must be relayed");
        let guidance = relayed.ai_guidance.expect("guidance should be attached");
        assert!(guidance.contains("access-control"), "{guidance}");
        assert!(guidance.contains("task id"), "{guidance}");
    }

    #[test]
    fn test_relay_covers_both_approval_refusals() {
        for (code, expected) in [
            (ErrorCode::ApprovalDenied, "reviewer refused"),
            (ErrorCode::ApprovalTimeout, "expired"),
        ] {
            let relayed = DenialReasonRelay::relay(&refusal(code, "refused"))
                .unwrap_or_else(|| panic!("{code:?} must be relayed"));
            assert_eq!(relayed.code, ErrorCode::GeneralInvalidInput);
            assert!(relayed.message.starts_with(&format!("[{code:?}]")));
            assert!(
                relayed
                    .ai_guidance
                    .as_deref()
                    .is_some_and(|g| g.contains(expected)),
                "{code:?} guidance should say why: {:?}",
                relayed.ai_guidance
            );
        }
    }

    #[test]
    fn test_relay_leaves_approval_pending_alone() {
        // apcore-a2a maps ApprovalPending to TaskState::InputRequired carrying
        // the message verbatim, and the message is how a caller learns the
        // approval_id it resumes with. Re-coding it would make a resumable
        // pause terminal.
        assert!(DenialReasonRelay::relay(&refusal(
            ErrorCode::ApprovalPending,
            "Approval pending for module 'cli.rm': awaiting review"
        ))
        .is_none());
    }

    #[test]
    fn test_relay_leaves_faithfully_reported_codes_alone() {
        for code in [
            ErrorCode::ModuleNotFound,
            ErrorCode::GeneralInvalidInput,
            ErrorCode::SchemaValidationError,
            ErrorCode::ModuleTimeout,
            ErrorCode::GeneralInternalError,
        ] {
            assert!(
                DenialReasonRelay::relay(&refusal(code, "boom")).is_none(),
                "{code:?} must not be relayed"
            );
        }
    }

    #[test]
    fn test_relay_keeps_the_trace_id_and_details_of_the_original() {
        // The relayed error is what an operator correlates with `audit.jsonl`,
        // so the join key has to survive the substitution.
        let mut details = std::collections::HashMap::new();
        details.insert("module_id".to_string(), serde_json::json!("cli.ls"));
        let original = refusal(ErrorCode::ACLDenied, "denied")
            .with_details(details)
            .with_trace_id("25b370279cbd40289ca1917906b6b17f");

        let relayed = DenialReasonRelay::relay(&original).expect("must be relayed");
        assert_eq!(
            relayed.trace_id.as_deref(),
            Some("25b370279cbd40289ca1917906b6b17f")
        );
        assert_eq!(
            relayed.details.get("module_id"),
            Some(&serde_json::json!("cli.ls"))
        );
    }

    #[test]
    fn test_relay_appends_to_existing_guidance_rather_than_replacing_it() {
        let original = refusal(ErrorCode::ApprovalDenied, "denied")
            .with_ai_guidance("The reviewer cited the change window.");
        let relayed = DenialReasonRelay::relay(&original).expect("must be relayed");
        let guidance = relayed.ai_guidance.expect("guidance should be attached");
        assert!(guidance.starts_with("The reviewer cited the change window."));
        assert!(guidance.contains("reviewer refused"), "{guidance}");
    }

    /// The relayed error must land in the half of apcore-a2a's mapper that
    /// forwards a message, or this whole file is a no-op.
    ///
    /// Asserted against the mapper itself rather than a remembered string, so
    /// an `apcore-a2a` bump that narrows the forwarding set fails here instead
    /// of silently restoring `Task not found`.
    #[test]
    fn test_relayed_denial_survives_the_apcore_a2a_error_mapper() {
        let original = refusal(
            ErrorCode::ACLDenied,
            "Access denied: caller 'None' cannot access module 'cli.ls'",
        );

        let masked = apcore_a2a::ErrorMapper::to_jsonrpc_error(&original);
        assert_eq!(
            masked.message, "Task not found",
            "the premise of this fix: upstream still masks an ACL denial"
        );

        let relayed = DenialReasonRelay::relay(&original).expect("must be relayed");
        let mapped = apcore_a2a::ErrorMapper::to_jsonrpc_error(&relayed);
        assert!(
            mapped.message.contains("[ACLDenied]"),
            "the relayed message must reach the caller: {:?}",
            mapped.message
        );
        assert!(
            mapped.message.contains("cli.ls"),
            "the caller must learn which skill was refused: {:?}",
            mapped.message
        );
    }
}
