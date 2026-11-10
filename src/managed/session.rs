//! The session state machine: preparation, approval, claim and terminal state.
//!
//! This is the launcher-side authority described by
//! `docs/managed-execution/session-protocol.md#execution-and-approval`. The
//! transport (sockets, peer credentials) is deliberately not here: the state
//! machine is the part that must be testable without a kernel, and it is the
//! part every rule in SES-03/SES-04/SES-05 talks about.
//!
//! One rule is worth restating because it is easy to lose: **a prepared plan
//! launches at most one execution**, the approval nonce is consumed in the same
//! transition that claims the call, and no error triggers an automatic rerun.

use std::collections::BTreeMap;
use std::time::Instant;

use super::backend::BackendId;
use super::effect::Effect;
use super::error::{ManagedError, ManagedErrorCode};
use super::form::{Approval, Args, FormId};
use super::json::hex_lower;
use super::plan::{Budget, CallId, Plan, SessionId};
use super::policy::CompiledPolicy;

/// A monotonic clock. Injected so expiry is testable without sleeping.
pub trait Clock: Send + Sync {
    /// Milliseconds since an arbitrary, monotonic origin.
    fn now_ms(&self) -> u64;
}

/// The process's monotonic clock.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Start a clock at the current instant.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// Finite session limits (isolation-domain.md ISO-5).
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    pub plan_ttl_ms: u64,
    pub approval_ttl_ms: u64,
    pub max_calls: usize,
    /// V1 admits one in-flight call. The field exists so a later version raises
    /// it deliberately rather than by editing a constant.
    pub max_in_flight: usize,
    pub budget: Budget,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            plan_ttl_ms: 300_000,
            approval_ttl_ms: 60_000,
            max_calls: 128,
            max_in_flight: 1,
            budget: Budget::default(),
        }
    }
}

impl SessionConfig {
    /// Reject a non-finite or zero limit.
    pub fn validate(&self) -> Result<(), ManagedError> {
        if self.plan_ttl_ms == 0
            || self.approval_ttl_ms == 0
            || self.max_calls == 0
            || self.max_in_flight == 0
        {
            return Err(ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                "session limits must be positive",
            ));
        }
        self.budget.validate()
    }
}

/// The call lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Ready,
    PendingApproval,
    Approved,
    Starting,
    Running,
    Succeeded,
    ToolFailed,
    Denied,
    Cancelled,
    ResourceLimit,
    PartialFailure,
    OutcomeUnknown,
    Expired,
    Stale,
}

impl CallState {
    /// The wire spelling used in `status` replies.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::PendingApproval => "pending_approval",
            Self::Approved => "approved",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::ToolFailed => "tool_failed",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
            Self::ResourceLimit => "resource_limit",
            Self::PartialFailure => "partial_failure",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Expired => "expired",
            Self::Stale => "stale",
        }
    }

    /// Whether the call reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::ToolFailed
                | Self::Denied
                | Self::Cancelled
                | Self::ResourceLimit
                | Self::PartialFailure
                | Self::OutcomeUnknown
                | Self::Expired
                | Self::Stale
        )
    }
}

/// The terminal status vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Succeeded,
    ToolFailed,
    Denied,
    Cancelled,
    ResourceLimit,
    PartialFailure,
    OutcomeUnknown,
}

impl Status {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::ToolFailed => "tool_failed",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
            Self::ResourceLimit => "resource_limit",
            Self::PartialFailure => "partial_failure",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}

/// Whether the call changed anything, as far as trusted evidence shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    None,
    Complete,
    Partial,
    Unknown,
}

impl Mutation {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

/// The terminal outcome of a call.
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub status: Status,
    pub mutation: Mutation,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

impl Outcome {
    /// A successful call that changed nothing.
    pub fn succeeded() -> Self {
        Self {
            status: Status::Succeeded,
            mutation: Mutation::None,
            exit_code: Some(0),
            signal: None,
        }
    }
}

/// The agent-facing half of a prepared plan.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub call_id: CallId,
    pub plan_digest: String,
    pub effects: Vec<Effect>,
    pub approval_required: bool,
    pub expires_in_ms: u64,
}

/// The launcher-internal approval ticket. Never sent to the agent.
#[derive(Debug, Clone)]
pub struct ApprovalTicket {
    pub call_id: CallId,
    pub nonce: String,
    pub expires_at_ms: u64,
}

/// What a claimed call hands to the executor.
#[derive(Debug, Clone)]
pub struct RunGrant {
    pub call_id: CallId,
    pub plan: Plan,
    pub budget: Budget,
}

/// One call's launcher-side record.
#[derive(Debug, Clone)]
struct Call {
    plan: Plan,
    state: CallState,
    nonce: Option<String>,
    approved_at_ms: Option<u64>,
    outcome: Option<Outcome>,
}

/// A managed session.
pub struct Session {
    pub id: SessionId,
    config: SessionConfig,
    policy_digest: String,
    isolation_digest: String,
    runtime_image: String,
    backend: BackendId,
    clock: Box<dyn Clock>,
    calls: BTreeMap<CallId, Call>,
    in_flight: Option<CallId>,
}

impl Session {
    /// Start a session. The caller has already compiled the policy and probed
    /// the backend; the digests are pinned here so a later policy change cannot
    /// reach an outstanding plan.
    pub fn new(
        id: SessionId,
        config: SessionConfig,
        policy_digest: impl Into<String>,
        isolation_digest: impl Into<String>,
        runtime_image: impl Into<String>,
        backend: BackendId,
        clock: Box<dyn Clock>,
    ) -> Result<Self, ManagedError> {
        config.validate()?;
        Ok(Self {
            id,
            config,
            policy_digest: policy_digest.into(),
            isolation_digest: isolation_digest.into(),
            runtime_image: runtime_image.into(),
            backend,
            clock,
            calls: BTreeMap::new(),
            in_flight: None,
        })
    }

    /// The configured budget.
    pub fn budget(&self) -> Budget {
        self.config.budget
    }

    /// Prepare a plan for `form` and `args`.
    ///
    /// `view` is the launcher-assigned absolute path of the native operand
    /// inside the worker's view; builtin forms ignore it.
    pub fn prepare(
        &mut self,
        form: FormId,
        args: Args,
        compiled: &CompiledPolicy,
        executable: Option<String>,
        view: Option<&std::path::Path>,
    ) -> Result<(Prepared, Option<ApprovalTicket>), ManagedError> {
        if self.calls.len() >= self.config.max_calls {
            return Err(ManagedError::planning(
                ManagedErrorCode::ResourceLimit,
                format!(
                    "the session retains at most {} call IDs",
                    self.config.max_calls
                ),
            ));
        }
        if args.form() != form {
            return Err(ManagedError::planning(
                ManagedErrorCode::InvalidArgument,
                "args do not belong to the requested form",
            )
            .with_field("form"));
        }
        let effects = args.effects()?;
        let rule = compiled.authorize(form, &effects)?;
        let call_id = CallId::generate()?;
        let now = self.clock.now_ms();
        let expires_at_ms = now.saturating_add(self.config.plan_ttl_ms);
        let argv = view.and_then(|path| args.fixed_argv(path));
        let plan = Plan {
            call_id: call_id.clone(),
            session_id: self.id.clone(),
            form,
            args,
            effects: effects.clone(),
            argv,
            policy_digest: self.policy_digest.clone(),
            isolation_digest: self.isolation_digest.clone(),
            backend: self.backend,
            runtime_image: self.runtime_image.clone(),
            executable,
            budget: self.config.budget,
            approval: rule.approval,
            prepared_at_ms: now,
            expires_at_ms,
        };
        let plan_digest = plan.digest()?;

        let (state, nonce, ticket) = if rule.approval == Approval::Required {
            let nonce = random_nonce()?;
            let ticket = ApprovalTicket {
                call_id: call_id.clone(),
                nonce: nonce.clone(),
                expires_at_ms: expires_at_ms.min(now.saturating_add(self.config.approval_ttl_ms)),
            };
            (CallState::PendingApproval, Some(nonce), Some(ticket))
        } else {
            (CallState::Ready, None, None)
        };
        self.calls.insert(
            call_id.clone(),
            Call {
                plan,
                state,
                nonce,
                approved_at_ms: None,
                outcome: None,
            },
        );
        Ok((
            Prepared {
                call_id,
                plan_digest,
                effects,
                approval_required: rule.approval == Approval::Required,
                expires_in_ms: self.config.plan_ttl_ms,
            },
            ticket,
        ))
    }

    /// Record a human approval for `call_id`, consuming the nonce.
    pub fn approve(&mut self, call_id: &CallId, nonce: &str) -> Result<(), ManagedError> {
        let now = self.clock.now_ms();
        let call = self.call_mut(call_id)?;
        if call.state != CallState::PendingApproval {
            return Err(ManagedError::approval(
                ManagedErrorCode::ApprovalDenied,
                "the call is not awaiting approval",
            ));
        }
        if now >= call.plan.expires_at_ms {
            call.state = CallState::Expired;
            return Err(ManagedError::approval(
                ManagedErrorCode::PlanExpired,
                "the plan expired before the decision arrived",
            ));
        }
        if call.nonce.as_deref() != Some(nonce) {
            return Err(ManagedError::approval(
                ManagedErrorCode::ApprovalDenied,
                "the approval nonce does not match this plan",
            ));
        }
        call.nonce = None;
        call.state = CallState::Approved;
        call.approved_at_ms = Some(now);
        Ok(())
    }

    /// Claim a prepared call for execution.
    pub fn run(&mut self, call_id: &CallId, plan_digest: &str) -> Result<RunGrant, ManagedError> {
        let now = self.clock.now_ms();
        let (state, plan, approved_at) = {
            let call = self.calls.get(call_id).ok_or_else(call_not_found)?;
            (call.state, call.plan.clone(), call.approved_at_ms)
        };
        if state.is_terminal() || matches!(state, CallState::Starting | CallState::Running) {
            return Err(ManagedError::execution(
                ManagedErrorCode::AlreadyStarted,
                "the call has already been claimed",
            ));
        }
        if now >= plan.expires_at_ms {
            self.expire(call_id);
            return Err(ManagedError::execution(
                ManagedErrorCode::PlanExpired,
                "the plan expired before it was run",
            ));
        }
        if plan.digest()? != plan_digest {
            return Err(ManagedError::execution(
                ManagedErrorCode::PlanStale,
                "the supplied plan digest does not match this plan",
            ));
        }
        match state {
            CallState::PendingApproval => {
                return Err(ManagedError::approval(
                    ManagedErrorCode::ApprovalRequired,
                    "the plan is awaiting a human decision",
                ));
            }
            CallState::Ready | CallState::Approved => {}
            other => {
                return Err(ManagedError::execution(
                    ManagedErrorCode::AlreadyStarted,
                    format!("the call is in state '{}'", other.as_str()),
                ));
            }
        }
        if let Some(approved_at) = approved_at {
            if now >= approved_at.saturating_add(self.config.approval_ttl_ms) {
                self.expire(call_id);
                return Err(ManagedError::execution(
                    ManagedErrorCode::PlanExpired,
                    "the approval grant expired before execution",
                ));
            }
        }
        if self.in_flight.is_some() {
            return Err(ManagedError::execution(
                ManagedErrorCode::ResourceLimit,
                "the session already has an in-flight call",
            ));
        }

        let call = self.call_mut(call_id)?;
        // Consume the approval in the same transition that claims the call, so
        // a crash after spawn cannot leave a reusable nonce.
        call.nonce = None;
        call.state = CallState::Starting;
        self.in_flight = Some(call_id.clone());
        Ok(RunGrant {
            call_id: call_id.clone(),
            plan,
            budget: self.config.budget,
        })
    }

    /// Mark a claimed call as actually running.
    pub fn mark_running(&mut self, call_id: &CallId) -> Result<(), ManagedError> {
        let call = self.call_mut(call_id)?;
        if call.state == CallState::Starting {
            call.state = CallState::Running;
        }
        Ok(())
    }

    /// Record the terminal outcome of a call.
    pub fn finish(&mut self, call_id: &CallId, outcome: Outcome) -> Result<(), ManagedError> {
        let call = self.call_mut(call_id)?;
        if call.state.is_terminal() {
            return Err(ManagedError::execution(
                ManagedErrorCode::AlreadyStarted,
                "the call already has a terminal outcome",
            ));
        }
        call.state = match outcome.status {
            Status::Succeeded => CallState::Succeeded,
            Status::ToolFailed => CallState::ToolFailed,
            Status::Denied => CallState::Denied,
            Status::Cancelled => CallState::Cancelled,
            Status::ResourceLimit => CallState::ResourceLimit,
            Status::PartialFailure => CallState::PartialFailure,
            Status::OutcomeUnknown => CallState::OutcomeUnknown,
        };
        call.outcome = Some(outcome);
        if self.in_flight.as_ref() == Some(call_id) {
            self.in_flight = None;
        }
        Ok(())
    }

    /// Cancel a call that has not reached a terminal state.
    pub fn cancel(&mut self, call_id: &CallId) -> Result<CallState, ManagedError> {
        let running = self.in_flight.as_ref() == Some(call_id);
        let call = self.call_mut(call_id)?;
        if call.state.is_terminal() {
            return Ok(call.state);
        }
        let mutation = if running {
            Mutation::Unknown
        } else {
            Mutation::None
        };
        call.state = CallState::Cancelled;
        call.outcome = Some(Outcome {
            status: Status::Cancelled,
            mutation,
            exit_code: None,
            signal: None,
        });
        let state = call.state;
        if running {
            self.in_flight = None;
        }
        Ok(state)
    }

    /// The current state and, once terminal, the outcome.
    pub fn status(&self, call_id: &CallId) -> Result<(CallState, Option<Outcome>), ManagedError> {
        let call = self.calls.get(call_id).ok_or_else(call_not_found)?;
        Ok((call.state, call.outcome))
    }

    /// The call ID currently holding the in-flight slot, if any.
    pub fn in_flight(&self) -> Option<&CallId> {
        self.in_flight.as_ref()
    }

    fn call_mut(&mut self, call_id: &CallId) -> Result<&mut Call, ManagedError> {
        self.calls.get_mut(call_id).ok_or_else(call_not_found)
    }

    fn expire(&mut self, call_id: &CallId) {
        if let Some(call) = self.calls.get_mut(call_id) {
            call.state = CallState::Expired;
        }
    }
}

/// A call that does not belong to this session, or never existed.
fn call_not_found() -> ManagedError {
    ManagedError::channel(ManagedErrorCode::CallNotFound, "unknown call ID")
}

/// A 32-byte CSPRNG nonce, hex encoded.
fn random_nonce() -> Result<String, ManagedError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        ManagedError::approval(
            ManagedErrorCode::IsolationFailed,
            format!("the OS CSPRNG is unavailable: {error}"),
        )
    })?;
    Ok(hex_lower(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::effect::{Grant, Op, RootId};
    use crate::managed::form::ResourceRef;
    use crate::managed::path::RelPath;
    use crate::managed::policy::CompiledForm;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    struct FakeClock(Arc<AtomicU64>);

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn compiled() -> CompiledPolicy {
        let root = RootId::parse("workspace").unwrap();
        let grants = vec![Grant {
            root: root.clone(),
            prefix: RelPath::root(),
            ops: [Op::InspectMetadata, Op::ReadFile, Op::CreateEntry]
                .into_iter()
                .collect(),
        }];
        let mut forms = BTreeMap::new();
        for (form, approval, id) in [
            (FormId::CatFile, Approval::None, "read"),
            (FormId::HeadLines, Approval::None, "head"),
            (FormId::CopyToPath, Approval::Required, "copy"),
        ] {
            forms.insert(
                form,
                CompiledForm {
                    rule_id: id.into(),
                    form,
                    grants: grants.clone(),
                    approval,
                },
            );
        }
        CompiledPolicy {
            digest: "d".repeat(64),
            forms,
        }
    }

    fn cat_args() -> Args {
        Args::CatFile {
            path: ResourceRef {
                root: RootId::parse("workspace").unwrap(),
                path: RelPath::parse("a.txt").unwrap(),
            },
        }
    }

    fn copy_args() -> Args {
        Args::CopyToPath {
            source: ResourceRef {
                root: RootId::parse("workspace").unwrap(),
                path: RelPath::parse("a.txt").unwrap(),
            },
            destination: ResourceRef {
                root: RootId::parse("workspace").unwrap(),
                path: RelPath::parse("b.txt").unwrap(),
            },
        }
    }

    fn session(ticks: Arc<AtomicU64>) -> Session {
        Session::new(
            SessionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            SessionConfig::default(),
            "p".repeat(64),
            "i".repeat(64),
            "sha256:image",
            BackendId::Linux,
            Box::new(FakeClock(ticks)),
        )
        .unwrap()
    }

    fn view() -> Option<&'static std::path::Path> {
        Some(std::path::Path::new("/view/a.txt"))
    }

    #[test]
    fn test_prepare_and_run_without_approval() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let (prepared, ticket) = session
            .prepare(
                FormId::CatFile,
                cat_args(),
                &compiled,
                Some("cli.cat".into()),
                view(),
            )
            .unwrap();
        assert!(!prepared.approval_required);
        assert!(ticket.is_none());
        let grant = session
            .run(&prepared.call_id, &prepared.plan_digest)
            .unwrap();
        assert_eq!(grant.plan.form, FormId::CatFile);
        assert_eq!(session.in_flight(), Some(&prepared.call_id));
        session
            .finish(&prepared.call_id, Outcome::succeeded())
            .unwrap();
        assert!(session.in_flight().is_none());
        let (state, outcome) = session.status(&prepared.call_id).unwrap();
        assert_eq!(state, CallState::Succeeded);
        assert_eq!(outcome.unwrap().status, Status::Succeeded);
    }

    #[test]
    fn test_a_second_run_of_the_same_call_is_refused() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let (prepared, _) = session
            .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
            .unwrap();
        session
            .run(&prepared.call_id, &prepared.plan_digest)
            .unwrap();
        let error = session
            .run(&prepared.call_id, &prepared.plan_digest)
            .expect_err("one plan launches at most one execution");
        assert_eq!(error.code, ManagedErrorCode::AlreadyStarted);
    }

    #[test]
    fn test_a_different_digest_is_stale_and_changes_nothing() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let (prepared, _) = session
            .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
            .unwrap();
        let error = session
            .run(&prepared.call_id, &"0".repeat(64))
            .expect_err("a different digest must be refused");
        assert_eq!(error.code, ManagedErrorCode::PlanStale);
        let (state, _) = session.status(&prepared.call_id).unwrap();
        assert_eq!(
            state,
            CallState::Ready,
            "a stale request must not consume the plan"
        );
    }

    #[test]
    fn test_approval_is_required_and_the_nonce_is_one_time() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let (prepared, ticket) = session
            .prepare(FormId::CopyToPath, copy_args(), &compiled, None, None)
            .unwrap();
        assert!(prepared.approval_required);
        let ticket = ticket.expect("a required approval has a ticket");

        let error = session
            .run(&prepared.call_id, &prepared.plan_digest)
            .expect_err("an unapproved plan must not run");
        assert_eq!(error.code, ManagedErrorCode::ApprovalRequired);

        let error = session
            .approve(&prepared.call_id, "not-the-nonce")
            .expect_err("a wrong nonce must be refused");
        assert_eq!(error.code, ManagedErrorCode::ApprovalDenied);

        session.approve(&prepared.call_id, &ticket.nonce).unwrap();
        session
            .run(&prepared.call_id, &prepared.plan_digest)
            .unwrap();
        let error = session
            .approve(&prepared.call_id, &ticket.nonce)
            .expect_err("the nonce is consumed by the claim");
        assert_eq!(error.code, ManagedErrorCode::ApprovalDenied);
    }

    #[test]
    fn test_one_in_flight_call_per_session() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let (first, _) = session
            .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
            .unwrap();
        let (second, _) = session
            .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
            .unwrap();
        session.run(&first.call_id, &first.plan_digest).unwrap();
        let error = session
            .run(&second.call_id, &second.plan_digest)
            .expect_err("v1 admits one in-flight call");
        assert_eq!(error.code, ManagedErrorCode::ResourceLimit);
    }

    #[test]
    fn test_expiry_is_terminal() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks.clone());
        let compiled = compiled();
        let (prepared, _) = session
            .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
            .unwrap();
        ticks.store(300_000, Ordering::SeqCst);
        let error = session
            .run(&prepared.call_id, &prepared.plan_digest)
            .expect_err("an expired plan must not run");
        assert_eq!(error.code, ManagedErrorCode::PlanExpired);
        assert_eq!(
            session.status(&prepared.call_id).unwrap().0,
            CallState::Expired
        );
    }

    #[test]
    fn test_an_approval_grant_expires() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks.clone());
        let compiled = compiled();
        let (prepared, ticket) = session
            .prepare(FormId::CopyToPath, copy_args(), &compiled, None, None)
            .unwrap();
        session
            .approve(&prepared.call_id, &ticket.unwrap().nonce)
            .unwrap();
        ticks.store(60_000, Ordering::SeqCst);
        let error = session
            .run(&prepared.call_id, &prepared.plan_digest)
            .expect_err("the approval grant expired");
        assert_eq!(error.code, ManagedErrorCode::PlanExpired);
    }

    #[test]
    fn test_the_call_budget_is_finite() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        for _ in 0..session.config.max_calls {
            session
                .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
                .unwrap();
        }
        let error = session
            .prepare(FormId::CatFile, cat_args(), &compiled, None, view())
            .expect_err("the call budget is finite");
        assert_eq!(error.code, ManagedErrorCode::ResourceLimit);
    }

    #[test]
    fn test_args_must_belong_to_the_requested_form() {
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let error = session
            .prepare(FormId::CatFile, copy_args(), &compiled, None, None)
            .expect_err("mismatched args must be refused");
        assert_eq!(error.code, ManagedErrorCode::InvalidArgument);
    }

    #[test]
    fn test_a_head_plan_carries_the_line_count_in_its_argv() {
        // The form renders `head -n N -- PATH`; this pins that the session does
        // not lose the operand on the way to the plan.
        let ticks = Arc::new(AtomicU64::new(0));
        let mut session = session(ticks);
        let compiled = compiled();
        let args = Args::HeadLines {
            path: ResourceRef {
                root: RootId::parse("workspace").unwrap(),
                path: RelPath::parse("a.txt").unwrap(),
            },
            lines: 7,
        };
        let (prepared, _) = session
            .prepare(FormId::HeadLines, args, &compiled, None, view())
            .unwrap();
        let grant = session
            .run(&prepared.call_id, &prepared.plan_digest)
            .unwrap();
        assert_eq!(
            grant.plan.argv.unwrap(),
            vec!["-n", "7", "--", "/view/a.txt"]
        );
    }
}
