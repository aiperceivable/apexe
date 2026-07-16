use apcore::module::ModuleAnnotations;
use apcore::{Change, Context, ErrorCode, Module, ModuleError, PreviewResult};
use apcore_toolkit::ScannedModule;
use async_trait::async_trait;
use serde_json::Value;

use super::executor;

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
        }
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
            Some(kwargs) => match executor::build_arguments(kwargs) {
                Err(e) => format!("Cannot preview: invalid arguments ({}).", e.message),
                Ok(user_args) => {
                    let mut full_command = vec![self.binary_path.clone()];
                    full_command.extend(self.command_parts.clone());
                    full_command.extend(user_args);
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

        let mut args: Vec<String> = self.command_parts.clone();
        let user_args = executor::build_arguments(kwargs)?;
        args.extend(user_args);

        let output = executor::execute_subprocess(
            &self.binary_path,
            &args,
            self.json_flag.as_deref(),
            self.timeout_ms,
            self.max_output_bytes,
        )
        .await
        .map_err(|e| {
            // Only mark a timeout retryable when the module is annotated
            // idempotent: a killed non-idempotent command (e.g. `rm -rf`)
            // may have partially applied its side effect, so blindly
            // retrying it (e.g. via RetryMiddleware) would be unsafe.
            if e.code == ErrorCode::ModuleTimeout {
                e.with_retryable(self.annotations.idempotent)
            } else {
                e
            }
        })?;

        let duration_ms = start.elapsed().as_millis() as u64;

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

        // Attach execution metadata for observability
        result.insert("trace_id".into(), Value::String(ctx.trace_id.clone()));
        result.insert("duration_ms".into(), Value::Number(duration_ms.into()));

        // Generate ai_guidance on non-zero exit for AI self-correction
        if output.exit_code != 0 {
            let guidance = format!(
                "Command '{}' exited with code {}. stderr: {}",
                self.module_id,
                output.exit_code,
                output.stderr.chars().take(200).collect::<String>()
            );
            result.insert("ai_guidance".into(), Value::String(guidance));
        }

        tracing::info!(
            module_id = %self.module_id,
            trace_id = %ctx.trace_id,
            exit_code = output.exit_code,
            duration_ms,
            "CLI module execution completed"
        );

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
        // Same as above, but for the build_arguments() failure path (e.g. a
        // shell-injection character in a parameter value).
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
            .preview(&json!({"msg": "hi; rm -rf /"}), None)
            .expect("destructive modules must surface a preview even on invalid arguments");
        assert!(preview.changes[0].summary.contains("Cannot preview"));
        assert!(preview.changes[0].summary.contains("invalid arguments"));
    }
}
