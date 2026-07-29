//! A circuit breaker that opens on dependency failures, not on caller mistakes.
//!
//! apcore's [`CircuitBreakerMiddleware`] feeds *every* `ModuleError` into its
//! rolling window. For a module wrapping a CLI binary that inverts what the
//! breaker is for:
//!
//! * Five calls rejected by input validation — the binary never spawned — open
//!   the circuit for every caller of that module.
//! * Five calls where the binary really did fail (`ls` on a missing path,
//!   exit 1) leave it closed, because a non-zero exit is a successful
//!   execution as far as the module is concerned.
//!
//! So a tool that is broken keeps its circuit closed while a caller sending a
//! schema-invalid object disables the tool for everyone. A breaker exists to
//! stop calls to an unhealthy dependency; a validation rejection is evidence of
//! the opposite — the module was reachable and its contract was enforced
//! exactly as designed.
//!
//! It also collides with apexe's audience. Schema trial-and-error is the normal
//! behaviour of an LLM caller: an agent that sends `{"l": true, "1": true}`,
//! reads the refusal and corrects itself is the success case for
//! `x-apexe-conflicts-with`. Doing that five times must not disable the tool.
//!
//! This wrapper keeps apcore's state machine and filters what reaches it, so
//! only outcomes that say something about the dependency's health count.

use apcore::context::Context;
use apcore::errors::ErrorCode;
use apcore::middleware::circuit_breaker::{CircuitBreakerMiddleware, CircuitBreakerState};
use apcore::middleware::Middleware;
use apcore::ModuleError;
use async_trait::async_trait;

/// Whether an error says the wrapped dependency is unhealthy.
///
/// The question is not "did the call fail" but "does this failure make the
/// *next* call less likely to succeed". Only failures that reached — or failed
/// to reach — the binary qualify.
///
/// Everything a caller can provoke by sending the wrong arguments is excluded:
/// the module answered correctly and is in no worse shape for having done so.
/// Governance refusals (ACL, approval) are excluded for the same reason, and
/// because letting a denied caller open a circuit shared with allowed callers
/// turns an access-control rule into a denial of service.
fn indicates_unhealthy_dependency(code: ErrorCode) -> bool {
    match code {
        // Caller-fault: argument objects that never became a subprocess.
        ErrorCode::SchemaValidationError
        | ErrorCode::SchemaUnionNoMatch
        | ErrorCode::SchemaUnionAmbiguous
        | ErrorCode::SchemaMaxDepthExceeded
        | ErrorCode::GeneralInvalidInput
        | ErrorCode::ModuleNotFound
        | ErrorCode::InvalidModuleId => false,
        // Governance decisions: deliberate refusals, not failures.
        ErrorCode::ACLDenied
        | ErrorCode::ApprovalDenied
        | ErrorCode::ApprovalTimeout
        | ErrorCode::ApprovalPending
        | ErrorCode::ExecutionCancelled
        | ErrorCode::CallDepthExceeded
        | ErrorCode::CircularCall
        | ErrorCode::CallFrequencyExceeded => false,
        // Everything else — spawn failure, timeout, signal death, internal
        // error — is about the dependency. Listing the healthy cases and
        // defaulting the rest to "unhealthy" keeps the breaker's protective
        // behaviour for any error code apcore adds later.
        _ => true,
    }
}

/// [`CircuitBreakerMiddleware`] with caller-fault errors filtered out.
///
/// `before` and `after` delegate unchanged; only `on_error` is gated.
#[derive(Debug)]
pub struct HealthOnlyCircuitBreaker {
    inner: CircuitBreakerMiddleware,
}

impl HealthOnlyCircuitBreaker {
    /// Wrap an already-configured breaker.
    pub fn new(inner: CircuitBreakerMiddleware) -> Self {
        Self { inner }
    }

    /// Build one with apcore's default thresholds.
    pub fn with_defaults() -> Self {
        Self::new(CircuitBreakerMiddleware::builder().build())
    }

    /// Current state for a `(module_id, caller_id)` pair. Test/observability
    /// accessor; mirrors [`CircuitBreakerMiddleware::state`].
    pub fn state(&self, module_id: &str, caller_id: &str) -> CircuitBreakerState {
        self.inner.state(module_id, caller_id)
    }
}

#[async_trait]
impl Middleware for HealthOnlyCircuitBreaker {
    fn name(&self) -> &'static str {
        // Kept identical to the wrapped middleware: apcore keys middleware
        // ordering and duplicate detection on this name, and callers listing
        // the chain expect to find "circuit_breaker".
        "circuit_breaker"
    }

    fn priority(&self) -> u16 {
        self.inner.priority()
    }

    async fn before(
        &self,
        module_id: &str,
        inputs: serde_json::Value,
        ctx: &Context<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, ModuleError> {
        self.inner.before(module_id, inputs, ctx).await
    }

    async fn after(
        &self,
        module_id: &str,
        inputs: serde_json::Value,
        output: serde_json::Value,
        ctx: &Context<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, ModuleError> {
        self.inner.after(module_id, inputs, output, ctx).await
    }

    async fn on_error(
        &self,
        module_id: &str,
        inputs: serde_json::Value,
        error: &ModuleError,
        ctx: &Context<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, ModuleError> {
        if !indicates_unhealthy_dependency(error.code) {
            tracing::debug!(
                module_id,
                error_code = ?error.code,
                "Not counting caller-fault error against the circuit breaker"
            );
            self.release_probe_slot(module_id, ctx);
            return Ok(None);
        }
        self.inner.on_error(module_id, inputs, error, ctx).await
    }
}

impl HealthOnlyCircuitBreaker {
    /// Hand back the HALF_OPEN probe slot that a suppressed call was holding.
    ///
    /// Not counting an error is not the same as the call never happening. In
    /// HALF_OPEN, apcore admits exactly one probe and sets `probe_in_flight`
    /// (`before`), then clears it in exactly two places: `after` on success,
    /// and the `on_error`-that-opens branch — which in HALF_OPEN is *every*
    /// `on_error`. Returning early from `on_error` therefore skips the only
    /// release path this call had, and the slot stays held until apcore's
    /// stale-probe reclaim fires a full recovery window later.
    ///
    /// That reintroduces the defect this whole type exists to remove, just
    /// moved from CLOSED into HALF_OPEN: apcore runs middleware `before`
    /// *ahead of* input validation, so a caller sending an invalid argument
    /// object is admitted as the probe and then rejected, and every other
    /// caller is refused with `CircuitBreakerOpen` for the next 30 seconds.
    ///
    /// Forcing the state back to HALF_OPEN clears `probe_in_flight` and
    /// `probe_started_at` while leaving the rolling window untouched, so the
    /// next call is admitted as a fresh probe and the circuit's health record
    /// still reflects only real dependency outcomes.
    fn release_probe_slot(&self, module_id: &str, ctx: &Context<serde_json::Value>) {
        let caller_id = ctx.caller_id.clone().unwrap_or_default();
        if self.inner.state(module_id, &caller_id) != CircuitBreakerState::HalfOpen {
            return;
        }
        tracing::debug!(
            module_id,
            caller_id = %caller_id,
            "Releasing the half-open probe slot held by a suppressed caller-fault error"
        );
        self.inner
            .force_state(module_id, &caller_id, CircuitBreakerState::HalfOpen, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A top-level call context — no `caller_id`, which is the bucket the
    /// reported failure landed in ("Circuit open for module 'cli.ls' (caller '')").
    fn context() -> Context<serde_json::Value> {
        Context::anonymous()
    }

    /// Drive `on_error` `count` times with `code`, as the executor would.
    async fn report_errors(breaker: &HealthOnlyCircuitBreaker, code: ErrorCode, count: usize) {
        let ctx = context();
        for _ in 0..count {
            let error = ModuleError::new(code, "boom".to_string());
            breaker
                .on_error("cli.ls", json!({}), &error, &ctx)
                .await
                .expect("on_error never fails");
        }
    }

    #[tokio::test]
    async fn test_validation_errors_do_not_open_the_circuit() {
        // Regression (#22): five conflict refusals — the binary never ran —
        // opened `cli.ls` for every caller, and the next valid call was
        // rejected outright.
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::GeneralInvalidInput, 10).await;

        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Closed);
        assert!(breaker
            .before("cli.ls", json!({}), &context())
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_suppressed_error_releases_the_half_open_probe_slot() {
        // Regression: filtering the error out of the breaker's window is only
        // half the job. In HALF_OPEN, apcore's ONLY release path for the single
        // probe slot is the `on_error`-that-opens branch, which is exactly what
        // suppression skips — so one invalid call during a probe window pinned
        // the module closed to every other caller for a full recovery window.
        // That is the same defect as #22, moved from CLOSED into HALF_OPEN.
        let breaker = HealthOnlyCircuitBreaker::new(
            CircuitBreakerMiddleware::builder()
                .recovery_window_ms(60_000) // long, so no stale-probe reclaim can mask the fix
                .build(),
        );

        // Open it with real dependency failures, then hand-drive it to HALF_OPEN
        // rather than sleeping out the recovery window.
        report_errors(&breaker, ErrorCode::ModuleTimeout, 10).await;
        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Open);
        breaker
            .inner
            .force_state("cli.ls", "", CircuitBreakerState::HalfOpen, None);

        // The probe is admitted, and it is a caller-fault failure.
        breaker
            .before("cli.ls", json!({}), &context())
            .await
            .expect("half-open admits one probe");
        let refusal = ModuleError::new(ErrorCode::GeneralInvalidInput, "bad args".to_string());
        breaker
            .on_error("cli.ls", json!({}), &refusal, &context())
            .await
            .expect("on_error never fails");

        // The next caller must be admitted as a fresh probe, not refused.
        breaker
            .before("cli.ls", json!({}), &context())
            .await
            .expect(
                "a suppressed caller-fault error must hand the probe slot back, \
             otherwise it disables the module for every other caller",
            );
    }

    #[tokio::test]
    async fn test_releasing_the_probe_slot_does_not_erase_the_failure_history() {
        // The slot is handed back, but the circuit stays HALF_OPEN and the
        // rolling window keeps the real failures — a caller mistake must not
        // launder a genuinely unhealthy dependency back to CLOSED.
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::ModuleTimeout, 10).await;
        breaker
            .inner
            .force_state("cli.ls", "", CircuitBreakerState::HalfOpen, None);
        breaker
            .before("cli.ls", json!({}), &context())
            .await
            .expect("half-open admits one probe");

        let refusal = ModuleError::new(ErrorCode::GeneralInvalidInput, "bad args".to_string());
        breaker
            .on_error("cli.ls", json!({}), &refusal, &context())
            .await
            .expect("on_error never fails");

        assert_eq!(
            breaker.state("cli.ls", ""),
            CircuitBreakerState::HalfOpen,
            "suppression must not close a circuit that real failures opened"
        );
    }

    #[tokio::test]
    async fn test_a_real_failure_during_a_probe_still_reopens_the_circuit() {
        // The protective direction must survive the release path: an unhealthy
        // outcome on the probe goes to the inner breaker unchanged.
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::ModuleTimeout, 10).await;
        breaker
            .inner
            .force_state("cli.ls", "", CircuitBreakerState::HalfOpen, None);
        breaker
            .before("cli.ls", json!({}), &context())
            .await
            .expect("half-open admits one probe");

        report_errors(&breaker, ErrorCode::ModuleTimeout, 1).await;

        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Open);
        assert_eq!(
            breaker
                .before("cli.ls", json!({}), &context())
                .await
                .expect_err("an open circuit rejects")
                .code,
            ErrorCode::CircuitBreakerOpen
        );
    }

    #[tokio::test]
    async fn test_schema_validation_errors_do_not_open_the_circuit() {
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::SchemaValidationError, 10).await;

        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Closed);
    }

    #[tokio::test]
    async fn test_acl_denials_do_not_open_the_circuit() {
        // A denied caller must not be able to disable the module for the
        // callers the ACL allows.
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::ACLDenied, 10).await;

        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Closed);
    }

    #[tokio::test]
    async fn test_timeouts_still_open_the_circuit() {
        // The behaviour the breaker exists for has to survive the filter.
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::ModuleTimeout, 10).await;

        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Open);
        let rejected = breaker.before("cli.ls", json!({}), &context()).await;
        assert_eq!(
            rejected.expect_err("an open circuit rejects").code,
            ErrorCode::CircuitBreakerOpen
        );
    }

    #[tokio::test]
    async fn test_execution_errors_still_open_the_circuit() {
        let breaker = HealthOnlyCircuitBreaker::with_defaults();
        report_errors(&breaker, ErrorCode::ModuleExecuteError, 10).await;

        assert_eq!(breaker.state("cli.ls", ""), CircuitBreakerState::Open);
    }

    #[test]
    fn test_unknown_error_codes_default_to_unhealthy() {
        // A code apcore adds later must keep the breaker protective rather
        // than silently stop counting.
        assert!(indicates_unhealthy_dependency(ErrorCode::ModuleLoadError));
        assert!(indicates_unhealthy_dependency(
            ErrorCode::GeneralInternalError
        ));
        assert!(!indicates_unhealthy_dependency(
            ErrorCode::GeneralInvalidInput
        ));
    }
}
