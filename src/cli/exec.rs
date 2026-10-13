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

mod native;

/// Policy for commands with no registered CLI binding.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum UnmanagedPolicy {
    #[default]
    Deny,
    Allow,
    Approval,
}

/// Execute native argv or a module with JSON input through shared governance.
#[derive(Debug, clap::Args, serde::Serialize, serde::Deserialize)]
pub struct ExecArgs {
    /// Scanned module ID for captured JSON execution.
    #[arg(required_unless_present = "native", conflicts_with = "native")]
    pub module_id: Option<String>,
    /// Native command and arguments, following --; output passes through.
    #[arg(last = true, required_unless_present = "module_id", num_args = 1..)]
    pub native: Vec<String>,
    /// JSON object matching the module's input schema.
    #[arg(long, requires = "module_id", conflicts_with = "native")]
    pub input: Option<String>,
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
    /// Policy for unregistered native commands; loaded ACL still applies.
    #[arg(long, value_enum, default_value = "deny", requires = "native")]
    pub unmanaged: UnmanagedPolicy,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum ExecutionMode {
    Module,
    Native,
    Unmanaged,
}

struct PreparedCall {
    module_id: String,
    input: Value,
    scanned: Option<apcore_toolkit::ScannedModule>,
    executor: std::sync::Arc<apcore::Executor>,
    mode: ExecutionMode,
    unmanaged_binary: Option<String>,
}

fn parse_caller(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err("Caller must not be empty".into());
    }
    Ok(value.into())
}

impl ExecArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<i32> {
        self.validate_mode()?;
        let acl = self.acl_path(config)?;
        let dir = self.bindings_dir.as_ref().unwrap_or(&config.bindings_dir);
        let modules =
            crate::output::load_modules_from_dir(dir, config.bindings_pattern.as_deref())?;
        let context = self.context();
        let options = self.executor_options(config, dir, acl.as_deref());
        tokio::runtime::Runtime::new()?.block_on(async {
            let call = if self.native.is_empty() {
                self.prepare_module(&options, modules)?
            } else {
                self.prepare_native(&options, modules, config, dir, &context)
                    .await?
            };
            if self.dry_run {
                let result = self.preflight(&call, &context, acl.is_some()).await?;
                write_json(&result)?;
                anyhow::ensure!(
                    result["valid"] == true,
                    "Preflight refused the call; inspect the JSON checks"
                );
                return Ok(0);
            }
            let output = call
                .executor
                .call(&call.module_id, call.input, Some(&context), None)
                .await?;
            if call.mode == ExecutionMode::Module {
                write_json(&output)?;
            }
            output["exit_code"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok())
                .ok_or_else(|| anyhow::anyhow!("Execution returned no exit status"))
        })
    }

    fn validate_mode(&self) -> anyhow::Result<()> {
        let native = !self.native.is_empty();
        anyhow::ensure!(
            native != self.module_id.is_some(),
            "Select exactly one module ID or native command after --"
        );
        anyhow::ensure!(
            !native || self.input.is_none(),
            "Native argv cannot be combined with --input"
        );
        anyhow::ensure!(
            native || self.unmanaged == UnmanagedPolicy::Deny,
            "Unmanaged policy requires native execution"
        );
        anyhow::ensure!(
            self.caller.is_none() || self.dry_run,
            "Caller assertions require --dry-run"
        );
        if let Some(caller) = &self.caller {
            parse_caller(caller).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }

    fn prepare_module(
        &self,
        options: &ExecutorOptions<'_>,
        modules: Vec<apcore_toolkit::ScannedModule>,
    ) -> anyhow::Result<PreparedCall> {
        let id = self
            .module_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Module ID is required"))?;
        let input: Value = serde_json::from_str(self.input.as_deref().unwrap_or("{}"))?;
        anyhow::ensure!(input.is_object(), "Input must be a JSON object");
        let scanned = modules
            .iter()
            .find(|module| module.module_id == id)
            .cloned();
        let executor = crate::module::registry::build_executor_from_modules(options, modules)?;
        Ok(PreparedCall {
            module_id: id.into(),
            input,
            scanned,
            executor,
            mode: ExecutionMode::Module,
            unmanaged_binary: None,
        })
    }

    async fn prepare_native(
        &self,
        options: &ExecutorOptions<'_>,
        modules: Vec<apcore_toolkit::ScannedModule>,
        config: &ApexeConfig,
        dir: &std::path::Path,
        context: &Context<Value>,
    ) -> anyhow::Result<PreparedCall> {
        let invocation =
            native::resolve(&self.native, self.unmanaged, &modules, config, dir, context).await?;
        let mut unmanaged_binary = None;
        let (executor, mode) = match invocation.unmanaged {
            Some(module) => {
                unmanaged_binary = Some(module.binary_path().into());
                (
                    crate::module::registry::build_executor_for_unmanaged(
                        options,
                        &invocation.module,
                        module,
                    )?,
                    ExecutionMode::Unmanaged,
                )
            }
            None => (
                crate::module::registry::build_executor_from_modules_with_io(
                    options,
                    modules,
                    crate::module::ExecutionIo::Passthrough {
                        stdin: super::shim::stdin_mode(&invocation.module),
                    },
                )?,
                ExecutionMode::Native,
            ),
        };
        Ok(PreparedCall {
            module_id: invocation.module.module_id.clone(),
            input: invocation.input,
            scanned: Some(invocation.module),
            executor,
            mode,
            unmanaged_binary,
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
        call: &PreparedCall,
        context: &Context<Value>,
        has_acl: bool,
    ) -> anyhow::Result<Value> {
        let preflight = call
            .executor
            .validate(&call.module_id, &call.input, Some(context))
            .await?;
        let acl_denied = preflight
            .checks
            .iter()
            .any(|check| check.check == apcore::module::ACL_CHECK_NAME && !check.passed);
        let mut result = json!({
            "module_id": call.module_id, "valid": preflight.valid, "mode":call.mode,
            "caller": context.caller_id, "caller_asserted": self.caller.is_some(),
            "acl_decision": acl_verdict(&preflight, has_acl),
            "approval_required": preflight.requires_approval,
            "checks": preflight.checks, "path_guard": [],
        });
        if !preflight.valid || acl_denied {
            return Ok(result);
        }
        let scanned = call
            .scanned
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Module not found: {}", call.module_id))?;
        if call.mode == ExecutionMode::Unmanaged {
            result["annotations"] =
                serde_json::to_value(scanned.annotations.clone().unwrap_or_default())?;
            result["binary_path"] = json!(call.unmanaged_binary);
            result["argv"] = call.input["argv"].clone();
            result["path_guard_available"] = json!(false);
            result["stdin"] = json!(crate::module::StdinMode::Null);
        } else {
            let io = if call.mode == ExecutionMode::Native {
                let stdin = super::shim::stdin_mode(scanned);
                result["stdin"] = json!(stdin);
                crate::module::ExecutionIo::Passthrough { stdin }
            } else {
                crate::module::ExecutionIo::Captured
            };
            add_command_plan(&mut result, scanned, &call.input, io)?;
        }
        Ok(result)
    }
}

fn add_command_plan(
    result: &mut Value,
    scanned: &apcore_toolkit::ScannedModule,
    input: &Value,
    io: crate::module::ExecutionIo,
) -> anyhow::Result<()> {
    let module = CliModule::from_scanned(scanned, 0)?.with_execution_io(io);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> ExecArgs {
        ExecArgs {
            module_id: Some("cli.cat".into()),
            native: vec![],
            input: None,
            bindings_dir: None,
            acl: None,
            dry_run: false,
            caller: None,
            unmanaged: UnmanagedPolicy::Deny,
        }
    }

    #[test]
    fn test_exec_public_arguments_reject_real_caller_assertion() {
        let mut args = args();
        args.caller = Some("ci".into());
        assert!(args.validate_mode().is_err());
        args.dry_run = true;
        assert!(args.validate_mode().is_ok());
        args.caller = Some(" ".into());
        assert!(args.validate_mode().is_err());
    }

    #[test]
    fn test_exec_public_arguments_reject_mixed_and_missing_modes() {
        let mut args = args();
        args.native = vec!["cat".into()];
        assert!(args.validate_mode().is_err());
        args.module_id = None;
        args.input = Some("{}".into());
        assert!(args.validate_mode().is_err());
        args.input = None;
        assert!(args.validate_mode().is_ok());
        args.native.clear();
        assert!(args.validate_mode().is_err());
    }

    #[test]
    fn test_exec_public_arguments_limit_unmanaged_policy_to_native() {
        let mut args = args();
        args.unmanaged = UnmanagedPolicy::Allow;
        assert!(args.validate_mode().is_err());
        args.module_id = None;
        args.native = vec!["toy".into()];
        assert!(args.validate_mode().is_ok());
    }
}
