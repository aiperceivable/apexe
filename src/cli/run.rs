use std::path::PathBuf;

use apcore::Context;
use serde_json::{json, Value};

use crate::config::ApexeConfig;
use crate::module::{CliModule, ExecutorOptions, ModuleFilter};

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum AclVerdict {
    Allowed,
    Denied,
    NoAclLoaded,
    NotEvaluated,
}

fn acl_verdict(preflight: &apcore::PreflightResult, loaded: bool) -> AclVerdict {
    if !loaded {
        return AclVerdict::NoAclLoaded;
    }
    match preflight
        .checks
        .iter()
        .find(|check| check.check == apcore::module::ACL_CHECK_NAME)
    {
        Some(check) if check.passed => AclVerdict::Allowed,
        Some(_) => AclVerdict::Denied,
        None => AclVerdict::NotEvaluated,
    }
}

/// Execute one governed CLI module with captured output.
#[derive(Debug, clap::Args, serde::Serialize, serde::Deserialize)]
pub struct RunArgs {
    pub module_id: String,
    /// JSON object matching the module's input schema.
    #[arg(long, default_value = "{}")]
    pub input: String,
    #[arg(long)]
    pub bindings_dir: Option<PathBuf>,
    /// ACL file; defaults to <config_dir>/acl.yaml when present.
    #[arg(long)]
    pub acl: Option<PathBuf>,
    /// Report governance decisions and argv without executing.
    #[arg(long)]
    pub dry_run: bool,
    /// Assert a caller for simulation only; requires --dry-run.
    #[arg(long = "as", requires = "dry_run", value_parser = parse_caller)]
    pub caller: Option<String>,
}

fn parse_caller(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err("Caller must not be empty".into());
    }
    Ok(value.into())
}

impl RunArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let input: Value = serde_json::from_str(&self.input)?;
        anyhow::ensure!(input.is_object(), "Input must be a JSON object");
        let acl = self.acl_path(config)?;
        let dir = self.bindings_dir.as_ref().unwrap_or(&config.bindings_dir);
        let modules =
            crate::output::load_modules_from_dir(dir, config.bindings_pattern.as_deref())?;
        let scanned = modules
            .iter()
            .find(|module| module.module_id == self.module_id)
            .cloned();
        let executor = crate::module::registry::build_executor_from_modules(
            &self.executor_options(config, dir, acl.as_deref()),
            modules,
        )?;
        let context = self.context();
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            if self.dry_run {
                let result = self
                    .preflight(&executor, &input, &context, scanned.as_ref(), acl.is_some())
                    .await?;
                let valid = result["valid"] == true;
                write_json(&result)?;
                anyhow::ensure!(valid, "Preflight refused the call; inspect the JSON checks");
            } else {
                let output = executor
                    .call(&self.module_id, input, Some(&context), None)
                    .await?;
                write_json(&output)?;
                anyhow::ensure!(
                    output["exit_code"] == 0,
                    "Wrapped command failed (exit_code {})",
                    output["exit_code"]
                );
            }
            Ok(())
        })
    }

    fn context(&self) -> Context<Value> {
        super::local::context(
            self.caller.as_deref().unwrap_or("local"),
            self.caller.is_some(),
        )
    }

    fn acl_path(&self, config: &ApexeConfig) -> anyhow::Result<Option<PathBuf>> {
        super::local::acl_path(self.acl.as_deref(), config)
    }

    fn executor_options<'a>(
        &self,
        config: &'a ApexeConfig,
        dir: &'a std::path::Path,
        acl: Option<&'a std::path::Path>,
    ) -> ExecutorOptions<'a> {
        super::local::options(
            config,
            dir,
            config.bindings_pattern.as_deref(),
            acl,
            ModuleFilter::default(),
        )
    }

    async fn preflight(
        &self,
        executor: &apcore::Executor,
        input: &Value,
        context: &Context<Value>,
        scanned: Option<&apcore_toolkit::ScannedModule>,
        has_acl: bool,
    ) -> anyhow::Result<Value> {
        let preflight = executor
            .validate(&self.module_id, input, Some(context))
            .await?;
        let acl_denied = preflight
            .checks
            .iter()
            .any(|check| check.check == apcore::module::ACL_CHECK_NAME && !check.passed);
        let mut result = json!({
            "module_id": self.module_id, "valid": preflight.valid,
            "caller": context.caller_id, "caller_asserted": self.caller.is_some(),
            "acl_decision": acl_verdict(&preflight, has_acl),
            "approval_required": preflight.requires_approval,
            "checks": preflight.checks, "path_guard": [],
        });
        if !preflight.valid || acl_denied {
            return Ok(result);
        }
        let scanned =
            scanned.ok_or_else(|| anyhow::anyhow!("Module not found: {}", self.module_id))?;
        add_command_plan(&mut result, scanned, input)?;
        Ok(result)
    }
}

fn add_command_plan(
    result: &mut Value,
    scanned: &apcore_toolkit::ScannedModule,
    input: &Value,
) -> anyhow::Result<()> {
    let module = CliModule::from_scanned(scanned, 0)?;
    result["annotations"] = serde_json::to_value(scanned.annotations.clone().unwrap_or_default())?;
    result["binary_path"] = json!(module.binary_path());
    result["path_guard"] = inspect_paths(
        input,
        &scanned.input_schema,
        scanned.annotations.as_ref().is_some_and(|a| a.readonly),
    );
    match module.planned_argv(input) {
        Ok(argv) => result["argv"] = json!(argv),
        Err(error) => {
            result["valid"] = json!(false);
            result["checks"]
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("Invalid preflight checks"))?
                .push(json!({"check":"argv", "passed":false, "error":error.to_dict()}));
        }
    }
    Ok(())
}

fn inspect_paths(input: &Value, schema: &Value, readonly: bool) -> Value {
    let mut verdicts = Vec::new();
    if let Some(properties) = input.as_object() {
        for (name, value) in properties {
            if !crate::module::executor::value_is_path(Some(schema), name)
                || !crate::module::executor::is_effective(value)
            {
                continue;
            }
            let values: Vec<&Value> = match value {
                Value::Array(items) => items.iter().collect(),
                value => vec![value],
            };
            for value in values {
                if let Some(path) = value.as_str() {
                    let (resolved, allowed) = crate::governance::path_guard::active().inspect(
                        path,
                        crate::governance::path_guard::AccessMode::from_readonly(readonly),
                    );
                    verdicts.push(json!({"parameter":name, "requested":path, "resolved":resolved, "allowed":allowed}));
                }
            }
        }
    }
    json!(verdicts)
}

pub(super) fn write_json(value: &Value) -> anyhow::Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}
