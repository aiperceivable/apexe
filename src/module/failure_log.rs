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
use std::sync::{Arc, Mutex};
use std::time::Instant;

use apcore::context::Context;
use apcore::middleware::Middleware;
use apcore::{ErrorCode, ModuleError};
use async_trait::async_trait;
use serde_json::Value;

/// Entry count past which `before` sweeps timed-out leftovers.
///
/// Well above any plausible in-flight concurrency, so the sweep is a leak
/// backstop rather than part of the hot path.
const STALE_SWEEP_THRESHOLD: usize = 1024;

/// Age past which an unreleased timing entry is treated as abandoned.
///
/// Comfortably beyond apexe's own default per-call timeout, so a slow but live
/// call is never swept out from under itself — the only cost of doing so would
/// be a `duration_ms` of 0 on its eventual failure record.
const STALE_ENTRY_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Whether [`CliModule`](crate::module::CliModule) already wrote an audit row
/// for a failure carrying `code`, so this middleware must not write a second.
///
/// `CliModule::abandon_run` records an `event: "execution"` row with
/// `exit_code: -1` when the subprocess could not be spawned, timed out, or
/// overflowed its output cap, and then re-raises. That error propagates out of
/// the execute step, which is *after* `middleware_before`, so apcore runs the
/// `on_error` chain and this middleware saw it too — producing a second row for
/// the same `trace_id` that said `status: "refused"`, i.e. that the binary never
/// ran. Both cannot be true, and every consumer counting `event == "refusal"`
/// over-counted. This is the same double-count deliberately avoided for ACL
/// denials, arriving through the other door.
///
/// **Residual gap, stated rather than hidden.** apcore's own global-deadline
/// check raises `ModuleTimeout` from inside the execute step *before* the
/// module is called, and `CliModule` never sees it, so that one loses its
/// refusal row here. Losing one rare row is the better side of the trade
/// against corrupting every refusal tally; closing it needs a channel from the
/// module to the middleware that apcore does not currently offer.
fn module_records_its_own_outcome(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::ModuleTimeout | ErrorCode::ModuleExecuteError
    )
}

/// Emits a payload-free record for every call that ends in failure.
///
/// Two records, with different lifetimes:
///
/// * an `ERROR`-level `tracing` event, emitted only when apcore's own
///   payload-bearing one is suppressed, so a failure produces exactly one such
///   line either way;
/// * a `refusal` row in the governance audit trail, emitted whenever an audit
///   path is configured and the failure is one [`CliModule`](crate::module::CliModule)
///   did not already record. Before this the audit trail held only the calls
///   that *ran* — someone probing the argv guards left no trace at all, which
///   is the sequence an audit exists to capture.
#[derive(Debug, Default)]
pub struct FailureLogMiddleware {
    /// Per-call start instants keyed by `trace_id:module_id`, mirroring
    /// apcore's own timing key so two concurrent calls to the same module
    /// cannot claim each other's duration.
    start_times: Mutex<HashMap<String, Instant>>,
    /// Governance audit sink. `None` disables auditing entirely.
    audit: Option<Arc<crate::governance::AuditManager>>,
    /// Whether to emit the `tracing` record. False when apcore's
    /// `LoggingMiddleware` is already emitting one for the same failure.
    emit_tracing_record: bool,
}

impl FailureLogMiddleware {
    /// Create a middleware that only emits the `tracing` record.
    pub fn new() -> Self {
        Self {
            emit_tracing_record: true,
            ..Self::default()
        }
    }

    /// Create a middleware writing `refusal` rows to `audit`.
    ///
    /// `emit_tracing_record` should be false when apcore's `LoggingMiddleware`
    /// still has its error hook enabled, so one failure does not produce two
    /// `ERROR` lines saying the same thing.
    pub fn with_audit(
        audit: Option<Arc<crate::governance::AuditManager>>,
        emit_tracing_record: bool,
    ) -> Self {
        Self {
            start_times: Mutex::new(HashMap::new()),
            audit,
            emit_tracing_record,
        }
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
        self.with_start_times(|times| {
            // A cancelled call releases nothing: apcore short-circuits
            // `ExecutionCancelled` ahead of the on_error chain, so neither
            // `after` nor `on_error` runs and the entry would live as long as
            // the server. Agent clients cancel routinely, so this is a slow
            // leak on every deployment rather than an edge case. Sweeping on
            // insert keeps the cost proportional to traffic and needs no
            // background task.
            if times.len() >= STALE_SWEEP_THRESHOLD {
                times.retain(|_, started| started.elapsed() < STALE_ENTRY_AGE);
            }
            times.insert(key, Instant::now());
        });
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
        if self.emit_tracing_record {
            tracing::error!(
                module_id = module_id,
                trace_id = %ctx.trace_id,
                caller_id = ?ctx.caller_id,
                error_code = ?error.code,
                duration_ms = duration_ms,
                "Module call failed"
            );
        }
        if let Some(ref audit) = self.audit {
            if module_records_its_own_outcome(error.code) {
                // `CliModule` already wrote an `execution`/`error` row for this
                // very trace; a `refusal` row on top would claim the binary
                // never ran. See `module_records_its_own_outcome`.
                return Ok(None);
            }
            // The authenticated principal, not `ctx.caller_id` — that field
            // names the calling *module* in a nested chain and is `None` for
            // every inbound request by apcore's contract, so it would record
            // every refusal against nobody.
            audit.log_refusal(
                module_id,
                &ctx.trace_id,
                ctx.identity.as_ref().map(|id| id.id()),
                None,
                error.code,
                duration_ms as u64,
            );
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apcore::ErrorCode;
    use serde_json::json;
    use std::time::Duration;

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
    async fn test_abandoned_timing_entries_do_not_grow_without_bound() {
        // apcore short-circuits `ExecutionCancelled` ahead of the on_error
        // chain, so a cancelled call runs neither `after` nor `on_error` and
        // its entry is never released. Agent clients cancel routinely, so
        // without the sweep the map grows for the life of the server.
        let middleware = FailureLogMiddleware::new();
        for _ in 0..(STALE_SWEEP_THRESHOLD + 64) {
            middleware
                .before("cli.ls", json!({}), &context())
                .await
                .expect("before never fails");
        }
        // Nothing is old enough to sweep yet, so the map is allowed to hold
        // them; what must hold is that the sweep runs and the map stays bounded
        // once entries age out.
        middleware.with_start_times(|times| {
            for started in times.values_mut() {
                *started = Instant::now() - STALE_ENTRY_AGE - Duration::from_secs(1);
            }
        });
        middleware
            .before("cli.ls", json!({}), &context())
            .await
            .expect("before never fails");
        assert_eq!(
            middleware.with_start_times(|times| times.len()),
            1,
            "aged-out entries must be swept, leaving only the call just started"
        );
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
        // `on_error` can be reached with no matching `before` — apcore runs it
        // over `executed_middlewares`, and a failure raised inside the execute
        // step arrives after this middleware's own `before` may have been
        // released by a concurrent path. `take_elapsed_ms` returning `None`
        // must fall back to 0 rather than panic.
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
