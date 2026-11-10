//! Domain errors for managed execution.
//!
//! The codes are the wire contract from
//! `docs/managed-execution/policy.md#decision-errors` and
//! `docs/managed-execution/session-protocol.md`. They are deliberately separate
//! from [`apcore::ErrorCode`]: a managed refusal has to name the *phase* it was
//! refused in, which the shared module vocabulary does not carry.

use thiserror::Error;

/// Where a managed call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Channel,
    Planning,
    Approval,
    Execution,
}

impl Phase {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::Planning => "planning",
            Self::Approval => "approval",
            Self::Execution => "execution",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The managed-execution decision and protocol codes.
///
/// Serialized in `SCREAMING_SNAKE_CASE` so the value on the wire is the one the
/// specification names (`POLICY_INVALID`, `PLAN_STALE`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ManagedErrorCode {
    // policy.md decision errors
    PolicyInvalid,
    BackendUnsupported,
    FormUnsupported,
    InvalidArgument,
    ResourceDenied,
    DestinationExists,
    // session-protocol.md protocol errors
    ProtocolInvalid,
    PeerDenied,
    CallNotFound,
    PlanExpired,
    PlanStale,
    ApprovalRequired,
    ApprovalDenied,
    AlreadyStarted,
    ResourceLimit,
    IsolationFailed,
    OutcomeUnknown,
}

impl ManagedErrorCode {
    /// The stable wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PolicyInvalid => "POLICY_INVALID",
            Self::BackendUnsupported => "BACKEND_UNSUPPORTED",
            Self::FormUnsupported => "FORM_UNSUPPORTED",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::ResourceDenied => "RESOURCE_DENIED",
            Self::DestinationExists => "DESTINATION_EXISTS",
            Self::ProtocolInvalid => "PROTOCOL_INVALID",
            Self::PeerDenied => "PEER_DENIED",
            Self::CallNotFound => "CALL_NOT_FOUND",
            Self::PlanExpired => "PLAN_EXPIRED",
            Self::PlanStale => "PLAN_STALE",
            Self::ApprovalRequired => "APPROVAL_REQUIRED",
            Self::ApprovalDenied => "APPROVAL_DENIED",
            Self::AlreadyStarted => "ALREADY_STARTED",
            Self::ResourceLimit => "RESOURCE_LIMIT",
            Self::IsolationFailed => "ISOLATION_FAILED",
            Self::OutcomeUnknown => "OUTCOME_UNKNOWN",
        }
    }
}

impl std::fmt::Display for ManagedErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A managed refusal, carrying its code, phase and the field or rule at fault.
///
/// `field` and `rule_id` are optional because the specification requires them
/// only "when safe": a refusal must never reflect a raw protected path or a
/// caller-supplied value back to the agent.
#[derive(Debug, Clone, Error)]
#[error("{code}: {message}")]
pub struct ManagedError {
    pub code: ManagedErrorCode,
    pub phase: Phase,
    pub message: String,
    pub field: Option<String>,
    pub rule_id: Option<String>,
}

impl ManagedError {
    /// Build a refusal in the planning phase.
    pub fn planning(code: ManagedErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, Phase::Planning, message)
    }

    /// Build a refusal in the channel phase.
    pub fn channel(code: ManagedErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, Phase::Channel, message)
    }

    /// Build a refusal in the approval phase.
    pub fn approval(code: ManagedErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, Phase::Approval, message)
    }

    /// Build a refusal in the execution phase.
    pub fn execution(code: ManagedErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, Phase::Execution, message)
    }

    /// Build a refusal with an explicit phase.
    pub fn new(code: ManagedErrorCode, phase: Phase, message: impl Into<String>) -> Self {
        Self {
            code,
            phase,
            message: message.into(),
            field: None,
            rule_id: None,
        }
    }

    /// Name the offending field, when that is safe to reflect.
    #[must_use]
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    /// Name the policy rule that refused the call.
    #[must_use]
    pub fn with_rule(mut self, rule_id: impl Into<String>) -> Self {
        self.rule_id = Some(rule_id.into());
        self
    }

    /// The error code as the wire spelling.
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codes_serialize_to_the_specified_spelling() {
        let json = serde_json::to_string(&ManagedErrorCode::DestinationExists).unwrap();
        assert_eq!(json, "\"DESTINATION_EXISTS\"");
        assert_eq!(
            serde_json::to_string(&ManagedErrorCode::OutcomeUnknown).unwrap(),
            "\"OUTCOME_UNKNOWN\""
        );
    }

    #[test]
    fn test_error_display_names_code_and_message() {
        let error = ManagedError::planning(ManagedErrorCode::InvalidArgument, "bad path")
            .with_field("destination");
        assert_eq!(
            error.to_string(),
            "INVALID_ARGUMENT: bad path",
            "Display must stay machine-greppable"
        );
        assert_eq!(error.field.as_deref(), Some("destination"));
    }
}
