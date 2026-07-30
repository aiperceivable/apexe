//! A terminal-failure log record that can never carry the call's payload.
//!
//! apcore's [`LoggingMiddleware`](apcore::middleware::logging::LoggingMiddleware)
//! bundles two different things into one flag triple. Its `on_error` hook is the
//! only place a refused call is announced at all, and it announces it *with*
//! `inputs = ?redacted_inputs` — the complete argument object with only the
//! properties the scanner marked `x-sensitive` masked. That redaction is
//! schema-driven and therefore inherently partial: a `curl --data` body and a
//! key sitting in a URL's query string are exactly the residual gap
//! `adapter::schema` documents and cannot close, because nothing in the wrapped
//! tool's `--help` announces them as secrets.
//!
//! So `--no-log-arguments` had a hole. Handing `false` to apcore's `log_errors`
//! would close it and take the operational record with it: an ACL denial, an
//! approval denial or a schema rejection never reaches
//! [`CliModule::execute`](crate::module::CliModule), which is the only other
//! thing in apexe that emits a per-call event, and those events are `info!`-level
//! success records in any case. A refused call would simply vanish from the log.
//!
//! This middleware is the operational half on its own: one `ERROR` record per
//! terminal failure carrying `module_id`, `trace_id`, `caller_id`, `error_code`
//! and `duration_ms`, and nothing that came from the caller. It is installed
//! exactly when apcore's payload-bearing error record is suppressed, so a
//! failure produces one record either way and that record is never the thing an
//! operator turned logging down to avoid.
//!
//! Note the deliberate omission of `error.message`. A validation message quotes
//! the value it rejected, which is the payload arriving through a second door.
//! `error_code` names the failure class, which is what an alert keys on.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use apcore::context::Context;
use apcore::middleware::Middleware;
use apcore::ModuleError;
use async_trait::async_trait;
use serde_json::Value;

/// Emits a payload-free `ERROR` record for every call that ends in failure.
#[derive(Debug, Default)]
pub struct FailureLogMiddleware {
    /// Per-call start instants keyed by `trace_id:module_id`, mirroring
    /// apcore's own timing key so two concurrent calls to the same module
    /// cannot claim each other's duration.
    start_times: Mutex<HashMap<String, Instant>>,
}

impl FailureLogMiddleware {
    /// Create a middleware with no calls in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Key a call's start instant by trace and module.
    fn timing_key(module_id: &str, ctx: &Context<Value>) -> String {
        format!("{}:{}", ctx.trace_id, module_id)
    }

    /// Run `apply` against the timing map.
    ///
    /// A poisoned lock only says some other task panicked while holding it;
    /// the map itself is still structurally sound, and dropping the log record
    /// for every subsequent failure would be a far worse outcome than reusing
    /// it, so this recovers rather than propagating.
    fn with_start_times<R>(&self, apply: impl FnOnce(&mut HashMap<String, Instant>) -> R) -> R {
        let mut guard = self
            .start_times
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        apply(&mut guard)
    }

    /// Take the recorded start instant for this call, if `before` ran.
    fn take_elapsed_ms(&self, module_id: &str, ctx: &Context<Value>) -> Option<f64> {
        let key = Self::timing_key(module_id, ctx);
        self.with_start_times(|times| times.remove(&key))
            .map(|start| start.elapsed().as_secs_f64() * 1000.0)
    }
}

#[async_trait]
impl Middleware for FailureLogMiddleware {
    fn name(&self) -> &'static str {
        "apexe_failure_log"
    }

    fn priority(&self) -> u16 {
        // apcore reserves 700-799 for logging middleware. Sharing 700 with
        // `LoggingMiddleware` is intentional: they never coexist, and the band
        // is what keeps both outside the governance and resilience middleware.
        700
    }

    async fn before(
        &self,
        module_id: &str,
        _inputs: Value,
        ctx: &Context<Value>,
    ) -> Result<Option<Value>, ModuleError> {
        let key = Self::timing_key(module_id, ctx);
        self.with_start_times(|times| times.insert(key, Instant::now()));
        Ok(None)
    }

    async fn after(
        &self,
        module_id: &str,
        _inputs: Value,
        _output: Value,
        ctx: &Context<Value>,
    ) -> Result<Option<Value>, ModuleError> {
        // Nothing is logged on success — that is `CliModule`'s own info! event
        // — but the timing entry has to be released or the map grows for the
        // lifetime of the server.
        self.take_elapsed_ms(module_id, ctx);
        Ok(None)
    }

    async fn on_error(
        &self,
        module_id: &str,
        _inputs: Value,
        error: &ModuleError,
        ctx: &Context<Value>,
    ) -> Result<Option<Value>, ModuleError> {
        // `duration_ms` is 0 when the failure short-circuited the pipeline
        // ahead of the middleware phase, so no `before` ever ran for it.
        let duration_ms = self.take_elapsed_ms(module_id, ctx).unwrap_or(0.0);
        tracing::error!(
            module_id = module_id,
            trace_id = %ctx.trace_id,
            caller_id = ?ctx.caller_id,
            error_code = ?error.code,
            duration_ms = duration_ms,
            "Module call failed"
        );
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apcore::ErrorCode;
    use serde_json::json;

    fn context() -> Context<Value> {
        Context::anonymous()
    }

    #[tokio::test]
    async fn test_failure_log_middleware_identity() {
        let middleware = FailureLogMiddleware::new();
        assert_eq!(middleware.name(), "apexe_failure_log");
        assert_eq!(middleware.priority(), 700);
    }

    #[tokio::test]
    async fn test_failure_log_middleware_passes_inputs_and_output_through() {
        // A logging middleware must never rewrite the call it observes.
        let middleware = FailureLogMiddleware::new();
        let ctx = context();
        assert!(middleware
            .before("cli.curl", json!({"data": "password=hunter2"}), &ctx)
            .await
            .expect("before never fails")
            .is_none());
        assert!(middleware
            .after("cli.curl", json!({}), json!({"stdout": "x"}), &ctx)
            .await
            .expect("after never fails")
            .is_none());
    }

    #[tokio::test]
    async fn test_failure_log_middleware_does_not_recover_the_error() {
        // Returning Some(..) from on_error would turn a refusal into a
        // successful call. This exists to observe, never to intercept.
        let middleware = FailureLogMiddleware::new();
        let error = ModuleError::new(ErrorCode::SchemaValidationError, "rejected".to_string());
        let outcome = middleware
            .on_error(
                "cli.curl",
                json!({"data": "password=hunter2"}),
                &error,
                &context(),
            )
            .await
            .expect("on_error never fails");
        assert!(outcome.is_none(), "the error must keep propagating");
    }

    #[tokio::test]
    async fn test_failure_log_middleware_releases_timing_entries() {
        // Both terminal paths have to release the entry, or a long-lived
        // server accumulates one Instant per call it ever served.
        let middleware = FailureLogMiddleware::new();
        let ctx = context();
        middleware
            .before("cli.ls", json!({}), &ctx)
            .await
            .expect("before never fails");
        middleware
            .after("cli.ls", json!({}), json!({}), &ctx)
            .await
            .expect("after never fails");
        assert!(middleware.with_start_times(|times| times.is_empty()));

        middleware
            .before("cli.ls", json!({}), &ctx)
            .await
            .expect("before never fails");
        let error = ModuleError::new(ErrorCode::ModuleTimeout, "timed out".to_string());
        middleware
            .on_error("cli.ls", json!({}), &error, &ctx)
            .await
            .expect("on_error never fails");
        assert!(middleware.with_start_times(|times| times.is_empty()));
    }

    #[tokio::test]
    async fn test_failure_log_middleware_measures_a_duration_when_before_ran() {
        let middleware = FailureLogMiddleware::new();
        let ctx = context();
        middleware
            .before("cli.ls", json!({}), &ctx)
            .await
            .expect("before never fails");
        let elapsed = middleware.take_elapsed_ms("cli.ls", &ctx);
        assert!(elapsed.is_some(), "before must record a start instant");
    }

    #[tokio::test]
    async fn test_failure_log_middleware_survives_a_failure_before_the_middleware_phase() {
        // apcore runs `acl_check` and `approval_gate` ahead of
        // `middleware_before`, so a governance refusal reaches on_error with no
        // recorded start instant. That must still produce a record, not panic.
        let middleware = FailureLogMiddleware::new();
        let error = ModuleError::new(ErrorCode::ACLDenied, "denied".to_string());
        let outcome = middleware
            .on_error("cli.rm", json!({}), &error, &context())
            .await
            .expect("on_error never fails without a matching before");
        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn test_failure_log_middleware_keys_concurrent_calls_separately() {
        // Two calls to the same module must not consume each other's timing
        // entry; the trace_id is what separates them.
        let middleware = FailureLogMiddleware::new();
        let first = context();
        let second = context();
        assert_ne!(first.trace_id, second.trace_id);

        middleware
            .before("cli.ls", json!({}), &first)
            .await
            .expect("before never fails");
        middleware
            .before("cli.ls", json!({}), &second)
            .await
            .expect("before never fails");

        assert!(middleware.take_elapsed_ms("cli.ls", &first).is_some());
        assert!(
            middleware.take_elapsed_ms("cli.ls", &second).is_some(),
            "the second call's timing entry must survive the first call's release"
        );
    }
}
