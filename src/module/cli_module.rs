use apcore::module::ModuleAnnotations;
use apcore::{Context, ErrorCode, Module, ModuleError};
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
    #[allow(dead_code)] // Reserved for runtime ACL checks and middleware annotation routing
    annotations: ModuleAnnotations,
    binary_path: String,
    command_parts: Vec<String>,
    json_flag: Option<String>,
    timeout_ms: u64,
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
            caller = ctx.identity.as_ref().map_or("anonymous", |i| i.id.as_str()),
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
        )
        .await?;

        let duration_ms = start.elapsed().as_millis() as u64;

        let mut result = serde_json::Map::new();
        result.insert("stdout".into(), Value::String(output.stdout.clone()));
        result.insert("stderr".into(), Value::String(output.stderr.clone()));
        result.insert("exit_code".into(), Value::Number(output.exit_code.into()));

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
        CliModule::new(CliModuleConfig {
            module_id: "test.echo".to_string(),
            description: "Echo test".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            annotations: ModuleAnnotations::default(),
            binary_path: "echo".to_string(),
            command_parts,
            json_flag,
            timeout_ms: 5000,
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
}
