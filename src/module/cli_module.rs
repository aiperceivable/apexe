use std::sync::Arc;

use apcore::module::ModuleAnnotations;
use apcore::{Change, Context, ErrorCode, Module, ModuleError, PreviewResult};
use apcore_toolkit::ScannedModule;
use async_trait::async_trait;
use serde_json::Value;

use super::executor;
use crate::governance::AuditManager;

/// Configuration for constructing a `CliModule`.
pub struct CliModuleConfig {
    pub module_id: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub annotations: ModuleAnnotations,
    pub binary_path: String,
    pub command_parts: Vec<String>,
    pub json_flag: Option<String>,
    pub timeout_ms: u64,
    /// Cap on captured stdout/stderr bytes; see
    /// [`executor::DEFAULT_MAX_OUTPUT_BYTES`].
    pub max_output_bytes: usize,
}

/// A CLI subprocess module implementing the apcore `Module` trait.
///
/// Each `CliModule` wraps a single CLI command and translates JSON inputs
/// into command-line arguments, executes the subprocess, and returns
/// structured output.
#[derive(Debug)]
pub struct CliModule {
    module_id: String,
    description: String,
    input_schema: Value,
    output_schema: Value,
    annotations: ModuleAnnotations,
    binary_path: String,
    command_parts: Vec<String>,
    json_flag: Option<String>,
    timeout_ms: u64,
    max_output_bytes: usize,
    /// Optional execution-audit sink (F5 §4.3). `None` disables audit.
    audit: Option<Arc<AuditManager>>,
}

impl CliModule {
    pub fn new(config: CliModuleConfig) -> Self {
        Self {
            module_id: config.module_id,
            description: config.description,
            input_schema: config.input_schema,
            output_schema: config.output_schema,
            annotations: config.annotations,
            binary_path: config.binary_path,
            command_parts: config.command_parts,
            json_flag: config.json_flag,
            timeout_ms: config.timeout_ms,
            max_output_bytes: config.max_output_bytes,
            audit: None,
        }
    }

    /// Attach (or clear) the execution-audit sink. Returns `self` for chaining
    /// at registration time so `CliModuleConfig` and its many call sites stay
    /// unchanged.
    pub fn with_audit(mut self, audit: Option<Arc<AuditManager>>) -> Self {
        self.audit = audit;
        self
    }

    /// Create from a `ScannedModule` by parsing the target field.
    ///
    /// Target format: `exec://{binary_path} {command_parts...}`
    #[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
    pub fn from_scanned(module: &ScannedModule, timeout_ms: u64) -> Result<Self, ModuleError> {
        let target = &module.target;
        let stripped = target.strip_prefix("exec://").ok_or_else(|| {
            ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                format!("Invalid target format: {target}"),
            )
        })?;
        let parts: Vec<&str> = stripped.split_whitespace().collect();
        if parts.is_empty() {
            return Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                "Empty target".to_string(),
            ));
        }
        let binary_path = parts[0].to_string();
        let command_parts: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        let json_flag = module
            .metadata
            .get("json_flag")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Validate json_flag and command_parts at construction time
        // to prevent injection from tampered binding files.
        if let Some(ref flag) = json_flag {
            for part in shell_words::split(flag).unwrap_or_default() {
                executor::validate_no_injection("json_flag", &part)?;
            }
        }
        for (i, part) in command_parts.iter().enumerate() {
            executor::validate_no_injection(&format!("command_part[{i}]"), part)?;
        }

        let annotations = module.annotations.clone().unwrap_or_default();

        Ok(Self::new(CliModuleConfig {
            module_id: module.module_id.clone(),
            description: module.description.clone(),
            input_schema: module.input_schema.clone(),
            output_schema: module.output_schema.clone(),
            annotations,
            binary_path,
            command_parts,
            json_flag,
            timeout_ms,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        }))
    }

    pub fn module_id(&self) -> &str {
        &self.module_id
    }

    /// Interleave the rendered arguments with this module's subcommand tokens.
    ///
    /// The subcommand path lives in `command_parts`, parsed out of the module
    /// target (`exec:///usr/bin/git cat-file`), so this is the only place in the
    /// pipeline that can put a token *ahead* of it. That is why
    /// [`executor::build_argv`] reports its `before_subcommand` group separately
    /// rather than emitting it: a tool's global options belong to the tool's own
    /// parser, which has already dispatched by the time the subcommand token is
    /// read. `git -C <dir> cat-file` honours `-C`; `git cat-file -C <dir>` hands
    /// it to a parser that reads `-C` as something else entirely.
    fn assemble_arguments(&self, rendered: executor::RenderedArgv) -> Vec<String> {
        let mut args = rendered.before_subcommand;
        args.extend(self.command_parts.iter().cloned());
        args.extend(rendered.after_subcommand);
        args
    }

    /// Record one completed or abandoned run in the governance audit trail.
    ///
    /// F5 §4.3. No raw argument values are persisted — the record names the
    /// call, not what was in it — and `trace_id` is what joins it to the ACL
    /// decision that permitted it.
    fn audit_execution(
        &self,
        ctx: &Context<Value>,
        status: &str,
        exit_code: i32,
        duration_ms: u64,
    ) {
        if let Some(ref audit) = self.audit {
            audit.log_execution(
                &self.module_id,
                &ctx.trace_id,
                ctx.identity.as_ref().map(|id| id.id()),
                status,
                exit_code,
                duration_ms,
            );
        }
    }

    /// Turn a subprocess failure into the error the caller sees, auditing it.
    ///
    /// Only a timeout is marked retryable, and only when the module is
    /// annotated idempotent: a killed non-idempotent command (e.g. `rm -rf`)
    /// may have partially applied its side effect, so blindly retrying it
    /// (e.g. via `RetryMiddleware`) would be unsafe.
    ///
    /// A killed or failed attempt — timeout, spawn failure, output overflow —
    /// must still appear in the audit trail; the `rm -rf`-that-timed-out case
    /// is exactly what an operator needs to see. The exit code is unknown, so
    /// it is recorded as -1.
    fn abandon_run(
        &self,
        error: ModuleError,
        ctx: &Context<Value>,
        elapsed_ms: u64,
    ) -> ModuleError {
        self.audit_execution(ctx, "error", -1, elapsed_ms);
        if error.code == ErrorCode::ModuleTimeout {
            error.with_retryable(self.annotations.idempotent)
        } else {
            error
        }
    }

    /// Assemble the result object a completed run returns.
    ///
    /// `json_output` is added only when the module declares a JSON flag and the
    /// output parses; `ai_guidance` only on a non-zero exit, where a caller has
    /// something to self-correct from.
    fn build_result(
        &self,
        output: &executor::SubprocessOutput,
        ctx: &Context<Value>,
        duration_ms: u64,
    ) -> serde_json::Map<String, Value> {
        let mut result = serde_json::Map::new();
        result.insert("stdout".into(), Value::String(output.stdout.clone()));
        result.insert("stderr".into(), Value::String(output.stderr.clone()));
        result.insert("exit_code".into(), Value::Number(output.exit_code.into()));
        if output.stdout_truncated {
            result.insert("stdout_truncated".into(), Value::Bool(true));
        }
        if output.stderr_truncated {
            result.insert("stderr_truncated".into(), Value::Bool(true));
        }

        if self.json_flag.is_some() && !output.stdout.trim().is_empty() {
            if let Ok(parsed) = serde_json::from_str::<Value>(&output.stdout) {
                result.insert("json_output".into(), parsed);
            }
        }

        // Execution metadata, for observability.
        result.insert("trace_id".into(), Value::String(ctx.trace_id.clone()));
        result.insert("duration_ms".into(), Value::Number(duration_ms.into()));

        if output.exit_code != 0 {
            let guidance = format!(
                "Command '{}' exited with code {}. stderr: {}",
                self.module_id,
                output.exit_code,
                output.stderr.chars().take(200).collect::<String>()
            );
            result.insert("ai_guidance".into(), Value::String(guidance));
        }
        result
    }
}

#[async_trait]
impl Module for CliModule {
    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn output_schema(&self) -> Value {
        self.output_schema.clone()
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// Live annotations, sourced directly from the scanned binding.
    ///
    /// apcore's `BuiltinApprovalGate` reads annotations from the resolved
    /// module instance (not the registry descriptor) when deciding whether
    /// a call requires approval, so this must mirror what was registered.
    fn annotations(&self) -> ModuleAnnotations {
        self.annotations.clone()
    }

    /// Best-effort preview for destructive commands.
    ///
    /// apexe cannot statically analyze the side effects of an arbitrary CLI
    /// binary, so this does not attempt to predict `before`/`after` state.
    /// It only surfaces the exact command line that would run, so a caller
    /// deciding whether to approve a destructive call can at least see what
    /// they're approving before it executes. Readonly/non-destructive
    /// modules return `None` (preview isn't applicable at all).
    ///
    /// A destructive module with unparseable inputs still returns
    /// `Some(PreviewResult)`, with a summary describing *why* no command
    /// line could be resolved — the apcore `Module::preview` signature only
    /// allows `Option`, so `None` is the sole "not applicable" signal;
    /// collapsing "not destructive" and "destructive but invalid input"
    /// into the same `None` would hide a validation failure a caller needs
    /// to see before approving.
    fn preview(&self, inputs: &Value, _ctx: Option<&Context<Value>>) -> Option<PreviewResult> {
        if !self.annotations.destructive {
            return None;
        }

        // Change/PreviewResult are #[non_exhaustive]; build via Default and
        // assign fields rather than a struct literal.
        let mut change = Change::default();
        change.action = "execute".to_string();
        change.target = self.binary_path.clone();
        change.summary = match inputs.as_object() {
            None => "Cannot preview: input must be a JSON object.".to_string(),
            Some(kwargs) => match executor::build_argv(kwargs, Some(&self.input_schema)) {
                Err(e) => format!("Cannot preview: invalid arguments ({}).", e.message),
                Ok(user_args) => {
                    let mut full_command = vec![self.binary_path.clone()];
                    full_command.extend(self.assemble_arguments(user_args));
                    format!(
                        "This will run: {}. Destructive command — apexe cannot statically \
                         predict the exact effects of an arbitrary CLI tool.",
                        full_command.join(" ")
                    )
                }
            },
        };

        let mut preview = PreviewResult::default();
        preview.changes = vec![change];
        Some(preview)
    }

    async fn execute(&self, inputs: Value, ctx: &Context<Value>) -> Result<Value, ModuleError> {
        let kwargs = inputs.as_object().ok_or_else(|| {
            ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                "Input must be a JSON object".to_string(),
            )
        })?;

        tracing::info!(
            module_id = %self.module_id,
            trace_id = %ctx.trace_id,
            caller = ctx.identity.as_ref().map_or("anonymous", |i| i.id()),
            "Executing CLI module"
        );

        let start = std::time::Instant::now();
        let user_args = executor::build_argv(kwargs, Some(&self.input_schema))?;
        let args: Vec<String> = self.assemble_arguments(user_args);

        let output = executor::execute_subprocess(
            &self.binary_path,
            &args,
            self.json_flag.as_deref(),
            self.timeout_ms,
            self.max_output_bytes,
        )
        .await
        .map_err(|e| self.abandon_run(e, ctx, start.elapsed().as_millis() as u64))?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let result = self.build_result(&output, ctx, duration_ms);

        tracing::info!(
            module_id = %self.module_id,
            trace_id = %ctx.trace_id,
            exit_code = output.exit_code,
            duration_ms,
            "CLI module execution completed"
        );

        let status = if output.exit_code == 0 {
            "success"
        } else {
            "error"
        };
        self.audit_execution(ctx, status, output.exit_code, duration_ms);

        Ok(Value::Object(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_scanned_module(target: &str) -> ScannedModule {
        ScannedModule::new(
            "test.echo".to_string(),
            "Test echo command".to_string(),
            json!({"type": "object", "properties": {"all": {"type": "boolean"}}}),
            json!({"type": "object"}),
            vec!["test".to_string()],
            target.to_string(),
        )
    }

    fn make_echo_module(command_parts: Vec<String>, json_flag: Option<String>) -> CliModule {
        make_echo_module_with_annotations(command_parts, json_flag, ModuleAnnotations::default())
    }

    fn make_echo_module_with_annotations(
        command_parts: Vec<String>,
        json_flag: Option<String>,
        annotations: ModuleAnnotations,
    ) -> CliModule {
        CliModule::new(CliModuleConfig {
            module_id: "test.echo".to_string(),
            description: "Echo test".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations,
            binary_path: "echo".to_string(),
            command_parts,
            json_flag,
            timeout_ms: 5000,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        })
    }

    /// A module whose schema marks `C` as a tool-level global option, the way
    /// `build_input_schema` marks the globals it merges into a subcommand.
    fn make_git_like_module() -> CliModule {
        CliModule::new(CliModuleConfig {
            module_id: "cli.git.cat_file".to_string(),
            description: "Git cat-file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "C": {
                        "type": "string",
                        "x-apexe-flag": "-C",
                        "x-apexe-flag-position": "before-subcommand",
                    },
                    "t": {"type": "boolean", "x-apexe-flag": "-t"},
                }
            }),
            output_schema: json!({"type": "object"}),
            annotations: ModuleAnnotations::default(),
            binary_path: "/usr/bin/git".to_string(),
            command_parts: vec!["cat-file".to_string()],
            json_flag: None,
            timeout_ms: 5000,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        })
    }

    #[test]
    fn test_assemble_arguments_places_global_options_before_the_subcommand() {
        // Regression for #39 item 1. `git`'s global options must precede the
        // subcommand token: `git -C <dir> cat-file` honours `-C`, while
        // `git cat-file -C <dir>` hands it to a parser that reads it as
        // something else and reports "not a git repository".
        //
        // `build_argv` reports the group; this is the only place that puts it
        // ahead of `command_parts`. Without this test, swapping the two
        // `extend` calls in `assemble_arguments` leaves the whole suite green.
        let module = make_git_like_module();
        let kwargs = json!({"C": "/repo", "t": true})
            .as_object()
            .expect("object literal")
            .clone();

        let rendered = executor::build_argv(&kwargs, Some(&module.input_schema)).unwrap();
        let args = module.assemble_arguments(rendered);

        assert_eq!(args, vec!["-C", "/repo", "cat-file", "-t"]);
    }

    #[test]
    fn test_assemble_arguments_keeps_the_subcommand_first_without_the_marker() {
        // The ordinary shape must be untouched: a module with no
        // `before-subcommand` flag still renders `<subcommand> <flags>`.
        let module = make_echo_module(vec!["hello".to_string()], None);
        let kwargs = json!({"n": true})
            .as_object()
            .expect("object literal")
            .clone();

        let rendered = executor::build_argv(&kwargs, Some(&module.input_schema)).unwrap();
        let args = module.assemble_arguments(rendered);

        assert_eq!(args, vec!["hello", "--n"]);
    }

    #[test]
    fn test_cli_module_from_scanned_basic() {
        let scanned = make_scanned_module("exec:///usr/bin/echo hello");
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();
        assert_eq!(module.module_id(), "test.echo");
        assert_eq!(module.binary_path, "/usr/bin/echo");
        assert_eq!(module.command_parts, vec!["hello"]);
        assert_eq!(module.timeout_ms, 5000);
    }

    #[test]
    fn test_cli_module_from_scanned_invalid_target() {
        let scanned = make_scanned_module("invalid");
        let result = CliModule::from_scanned(&scanned, 5000);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_cli_module_from_scanned_empty_target() {
        let scanned = make_scanned_module("exec://");
        let result = CliModule::from_scanned(&scanned, 5000);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_cli_module_from_scanned_with_json_flag() {
        let mut scanned = make_scanned_module("exec:///usr/bin/mycli");
        scanned
            .metadata
            .insert("json_flag".to_string(), json!("--format json"));
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();
        assert_eq!(module.json_flag, Some("--format json".to_string()));
    }

    #[test]
    fn test_cli_module_input_schema() {
        let scanned = make_scanned_module("exec:///usr/bin/echo");
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();
        let schema = module.input_schema();
        assert_eq!(
            schema,
            json!({"type": "object", "properties": {"all": {"type": "boolean"}}})
        );
    }

    #[test]
    fn test_cli_module_output_schema() {
        let scanned = make_scanned_module("exec:///usr/bin/echo");
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();
        let schema = module.output_schema();
        assert_eq!(schema, json!({"type": "object"}));
    }

    #[test]
    fn test_cli_module_description() {
        let scanned = make_scanned_module("exec:///usr/bin/echo");
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();
        assert_eq!(module.description(), "Test echo command");
    }

    #[test]
    fn test_cli_module_annotations_mirrors_scanned() {
        let mut scanned = make_scanned_module("exec:///usr/bin/rm");
        scanned.annotations = Some(ModuleAnnotations {
            destructive: true,
            requires_approval: true,
            ..Default::default()
        });
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();

        let annotations = module.annotations();
        assert!(annotations.destructive);
        assert!(annotations.requires_approval);
    }

    #[test]
    fn test_cli_module_annotations_default_when_unset() {
        let scanned = make_scanned_module("exec:///usr/bin/echo");
        let module = CliModule::from_scanned(&scanned, 5000).unwrap();

        let annotations = module.annotations();
        assert!(!annotations.destructive);
        assert!(!annotations.requires_approval);
    }

    #[tokio::test]
    async fn test_cli_module_execute_echo() {
        let module = make_echo_module(vec!["hello".to_string()], None);

        let ctx = Context::anonymous();
        let result = module.execute(json!({}), &ctx).await.unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("hello"));
        assert_eq!(result["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_cli_module_execute_with_args() {
        let module = make_echo_module(vec![], None);

        let ctx = Context::anonymous();
        let result = module.execute(json!({"all": true}), &ctx).await.unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("--all"));
    }

    #[tokio::test]
    async fn test_execute_json_output_parsed() {
        // F2 §5.4: when json_flag is set and stdout is valid JSON, the result
        // carries a parsed `json_output` object.
        let module = CliModule::new(CliModuleConfig {
            module_id: "test.jsontool".to_string(),
            description: "json test".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: ModuleAnnotations::default(),
            binary_path: "sh".to_string(),
            command_parts: vec![
                "-c".to_string(),
                "echo '{\"status\":\"ok\",\"count\":2}'".to_string(),
            ],
            json_flag: Some("--json".to_string()),
            timeout_ms: 5000,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        });

        let ctx = Context::anonymous();
        let result = module.execute(json!({}), &ctx).await.unwrap();
        let json_out = result
            .get("json_output")
            .expect("json_output must be present when json_flag is set and stdout is JSON");
        assert_eq!(json_out["status"], "ok");
        assert_eq!(json_out["count"], 2);
    }

    #[tokio::test]
    async fn test_execute_writes_audit_entry() {
        // F5 §4.3: with an audit sink attached, each execution appends a JSONL
        // entry (module_id + status), and raw input is not stored.
        let tmp = tempfile::TempDir::new().unwrap();
        let audit_path = tmp.path().join("audit.jsonl");
        let audit = Arc::new(AuditManager::new(&audit_path));
        let module = make_echo_module(vec!["hello".to_string()], None).with_audit(Some(audit));

        let ctx = Context::anonymous();
        module
            .execute(json!({"secret": "value"}), &ctx)
            .await
            .unwrap();

        let content = std::fs::read_to_string(&audit_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["event"], "execution");
        assert_eq!(entry["module_id"], "test.echo");
        assert_eq!(entry["status"], "success");
        assert_eq!(entry["exit_code"], 0);
        // The record names the call, never its arguments. `input_hash` used to
        // stand in for them and was dropped: apcore-cli salted it with 16 fresh
        // random bytes per call and discarded the salt, so nobody — itself
        // included — could reproduce the digest from a known input.
        assert!(!content.contains("secret"));
        assert!(entry.get("input_hash").is_none());
        // `trace_id` is what joins this record to the ACL decision for the same
        // call; the previous shape had none.
        assert_eq!(entry["trace_id"], ctx.trace_id);
    }

    #[tokio::test]
    async fn test_execute_without_audit_writes_nothing() {
        // No audit sink -> no file created, execution still succeeds.
        let module = make_echo_module(vec!["hi".to_string()], None);
        let ctx = Context::anonymous();
        let result = module.execute(json!({}), &ctx).await.unwrap();
        assert_eq!(result["exit_code"], 0);
    }

    #[tokio::test]
    async fn test_execute_audits_failed_run() {
        // F5 §4.3: a failed/aborted attempt (here: spawn failure) is still
        // recorded with status "error", not lost.
        let tmp = tempfile::TempDir::new().unwrap();
        let audit_path = tmp.path().join("audit.jsonl");
        let audit = Arc::new(AuditManager::new(&audit_path));
        let module = CliModule::new(CliModuleConfig {
            module_id: "test.fail".to_string(),
            description: "fail".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: ModuleAnnotations::default(),
            binary_path: "zzz_no_such_binary_xyz".to_string(),
            command_parts: vec![],
            json_flag: None,
            timeout_ms: 5000,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        })
        .with_audit(Some(audit));

        let ctx = Context::anonymous();
        let result = module.execute(json!({}), &ctx).await;
        assert!(result.is_err());

        let content = std::fs::read_to_string(&audit_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["module_id"], "test.fail");
        assert_eq!(entry["status"], "error");
    }

    #[tokio::test]
    async fn test_cli_module_timeout_retryable_only_when_idempotent() {
        let idempotent = CliModule::new(CliModuleConfig {
            module_id: "test.sleep".to_string(),
            description: "Sleep test".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: ModuleAnnotations {
                idempotent: true,
                ..Default::default()
            },
            binary_path: "sleep".to_string(),
            command_parts: vec!["1".to_string()],
            json_flag: None,
            timeout_ms: 10,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        });
        let err = idempotent
            .execute(json!({}), &Context::anonymous())
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ModuleTimeout);
        assert_eq!(err.retryable, Some(true));

        let non_idempotent = CliModule::new(CliModuleConfig {
            module_id: "test.sleep".to_string(),
            description: "Sleep test".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: ModuleAnnotations::default(), // idempotent: false
            binary_path: "sleep".to_string(),
            command_parts: vec!["1".to_string()],
            json_flag: None,
            timeout_ms: 10,
            max_output_bytes: executor::DEFAULT_MAX_OUTPUT_BYTES,
        });
        let err = non_idempotent
            .execute(json!({}), &Context::anonymous())
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ModuleTimeout);
        assert_eq!(err.retryable, Some(false));
    }

    #[test]
    fn test_cli_module_preview_none_for_non_destructive() {
        let module = make_echo_module(vec!["hello".to_string()], None);
        assert!(module.preview(&json!({}), None).is_none());
    }

    #[test]
    fn test_cli_module_preview_describes_destructive_command() {
        let module = make_echo_module_with_annotations(
            vec!["-rf".to_string()],
            None,
            ModuleAnnotations {
                destructive: true,
                requires_approval: true,
                ..Default::default()
            },
        );

        let preview = module
            .preview(&json!({"path": "/tmp/x"}), None)
            .expect("destructive modules should return a preview");

        assert_eq!(preview.changes.len(), 1);
        let change = &preview.changes[0];
        assert_eq!(change.action, "execute");
        assert_eq!(change.target, "echo");
        assert!(change.summary.contains("echo -rf"));
        assert!(change.summary.contains("--path"));
    }

    #[test]
    fn test_cli_module_preview_surfaces_non_object_input() {
        // Regression for the WARNING finding: preview() used to collapse
        // "not destructive" and "destructive but invalid input" into the
        // same `None`, so a caller couldn't tell a validation failure from
        // "no preview available". A destructive module must still return
        // Some(..) with an explanatory summary when the input isn't even a
        // JSON object.
        let module = make_echo_module_with_annotations(
            vec!["-rf".to_string()],
            None,
            ModuleAnnotations {
                destructive: true,
                requires_approval: true,
                ..Default::default()
            },
        );

        let preview = module
            .preview(&json!("not an object"), None)
            .expect("destructive modules must surface a preview even on invalid input");
        assert!(preview.changes[0].summary.contains("must be a JSON object"));
    }

    #[test]
    fn test_cli_module_preview_surfaces_argument_validation_failure() {
        // Same as above, but for the build_arguments() failure path (here: a
        // value the wrapped command would parse as an option). Any rejected
        // input works -- this covers preview's error branch, not the specific
        // validation rule.
        let module = make_echo_module_with_annotations(
            vec!["-rf".to_string()],
            None,
            ModuleAnnotations {
                destructive: true,
                requires_approval: true,
                ..Default::default()
            },
        );

        let preview = module
            .preview(&json!({"msg": "--no-preserve-root"}), None)
            .expect("destructive modules must surface a preview even on invalid arguments");
        assert!(preview.changes[0].summary.contains("Cannot preview"));
        assert!(preview.changes[0].summary.contains("invalid arguments"));
    }
}
