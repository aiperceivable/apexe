use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config::ApexeConfig;
use crate::models::ShimContract;
use crate::module::{ExecutionIo, ModuleFilter, StdinMode};
use crate::runtime_path::MANIFEST_NAME;
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};

pub(super) const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Route {
    pub module_id: String,
    pub command_path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub version: u32,
    pub executable: PathBuf,
    pub bindings_dir: PathBuf,
    pub bindings_pattern: Option<String>,
    pub acl: Option<PathBuf>,
    pub filter: ModuleFilter,
    pub commands: BTreeMap<String, Vec<Route>>,
    #[serde(default)]
    pub activation_script: Option<String>,
    #[serde(default)]
    pub deactivation_script: Option<String>,
    #[serde(default)]
    pub source_scripts_dir: Option<PathBuf>,
}

pub(super) fn read_manifest(directory: &Path) -> anyhow::Result<Manifest> {
    let path = directory.join(MANIFEST_NAME);
    let metadata = std::fs::metadata(&path)?;
    anyhow::ensure!(metadata.len() <= 1024 * 1024, "Shim manifest exceeds 1 MiB");
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        manifest.version == MANIFEST_VERSION,
        "Unsupported shim manifest version; re-run apexe env --enable"
    );
    Ok(manifest)
}

pub(super) fn command_path(module: &ScannedModule) -> anyhow::Result<Vec<String>> {
    let path: Vec<String> = serde_json::from_value(
        module
            .metadata
            .get("command_path")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Missing command_path; rescan {}", module.module_id))?,
    )?;
    anyhow::ensure!(
        !path.is_empty() && valid_name(&path[0]),
        "Invalid shim command name"
    );
    let target = module
        .target
        .strip_prefix("exec://")
        .ok_or_else(|| anyhow::anyhow!("Shim target is not a CLI binary"))?;
    let parts: Vec<&str> = target.split_whitespace().collect();
    anyhow::ensure!(
        !parts.is_empty() && Path::new(parts[0]).is_absolute(),
        "Shim targets must be absolute"
    );
    anyhow::ensure!(
        parts[1..] == path[1..],
        "command_path does not agree with target for {}",
        module.module_id
    );
    Ok(path)
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && name != "apexe"
        && name != "apexe.exe"
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-.".contains(character))
}

pub(super) fn eligible(module: &ScannedModule) -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::output::has_current_scan_format(module),
        "{} needs a current-format rescan",
        module.module_id
    );
    let contract: ShimContract =
        serde_json::from_value(module.metadata.get("shim_contract").cloned().ok_or_else(
            || anyhow::anyhow!("{} has no authoritative overlay contract", module.module_id),
        )?)?;
    anyhow::ensure!(
        contract.authoritative
            && Some(contract.variant.as_str())
                == module
                    .metadata
                    .get("variant")
                    .and_then(serde_json::Value::as_str),
        "{} has no matching authoritative overlay",
        module.module_id
    );
    anyhow::ensure!(
        contract.cross_flag_ordering == Some(false)
            && module.input_schema["x-apexe-cross-flag-ordering"] == false,
        "{} requires cross-flag ordering",
        module.module_id
    );
    validate_capabilities(module, &contract)?;
    command_path(module)?;
    let binary = crate::scanner::resolver::extract_binary_path(&module.target)
        .ok_or_else(|| anyhow::anyhow!("Invalid CLI target"))?;
    anyhow::ensure!(
        crate::scanner::resolver::binary_is_reachable(binary),
        "Binary for {} is unavailable",
        module.module_id
    );
    anyhow::ensure!(
        !Path::new(binary)
            .parent()
            .is_some_and(crate::runtime_path::is_shim_directory),
        "Binding targets another shim; rescan with a clean PATH"
    );
    Ok(())
}

fn validate_capabilities(module: &ScannedModule, contract: &ShimContract) -> anyhow::Result<()> {
    anyhow::ensure!(
        module.input_schema["x-apexe-streaming-stdin-safe"]
            .as_bool()
            .unwrap_or(false)
            == contract.streaming_stdin_safe,
        "Inconsistent stdin capability for {}",
        module.module_id
    );
    anyhow::ensure!(
        module.input_schema["x-apexe-short-flag-bundling"].as_bool()
            == contract.short_flag_bundling,
        "Inconsistent bundling capability for {}",
        module.module_id
    );
    Ok(())
}

fn invocation(raw: &[OsString]) -> Option<anyhow::Result<(PathBuf, String)>> {
    let name = Path::new(raw.first()?).file_name()?.to_str()?;
    #[cfg(windows)]
    let name = name.strip_suffix(".exe").unwrap_or(name);
    if name == "apexe" {
        return None;
    }
    Some((|| {
        anyhow::ensure!(valid_name(name), "Invalid shim name");
        let path =
            if Path::new(&raw[0]).is_absolute() || Path::new(&raw[0]).components().count() > 1 {
                std::path::absolute(&raw[0])?
            } else {
                which::which(&raw[0])?
            };
        let directory = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Shim has no parent directory"))?
            .to_path_buf();
        Ok((directory, name.into()))
    })())
}

pub(super) fn try_execute(raw: &[OsString]) -> Option<anyhow::Result<i32>> {
    let invocation = invocation(raw)?;
    Some(invocation.and_then(|(directory, name)| {
        let config = crate::config::load_config(None)?;
        super::install_path_guard(&config)?;
        let manifest = read_manifest(&directory)?;
        let args = raw[1..]
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("Native arguments must be valid UTF-8"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        execute(&name, &args, &manifest, &config)
    }))
}

#[derive(Debug, thiserror::Error)]
#[error("Native arguments refused for {module_id}")]
struct RouteRefusal {
    module_id: String,
}

pub(super) fn resolve_route<'a>(
    name: &str,
    args: &[String],
    manifest: &Manifest,
    modules: &'a [ScannedModule],
) -> anyhow::Result<(&'a ScannedModule, serde_json::Value)> {
    let routes = manifest.commands.get(name).ok_or_else(|| {
        anyhow::anyhow!("Shim '{name}' has no registered route; re-run apexe env --enable")
    })?;
    let candidates = route_candidates(routes, manifest, modules)?;
    let schemas: Vec<(&serde_json::Value, bool)> = candidates
        .iter()
        .filter_map(|(module, route)| {
            module.map(|module| (&module.input_schema, route.command_path.len() == 1))
        })
        .collect();
    let matcher = crate::adapter::argv::CommandMatcher::new(&schemas)?;
    for (module, route) in candidates {
        if !matcher.matches(args, &route.command_path[1..])? {
            continue;
        }
        let module = module.ok_or_else(|| {
            anyhow::anyhow!(
                "{} is no longer registered; no native fallback is permitted",
                route.module_id
            )
        })?;
        // Once a subcommand is selected, any input refusal is final. Falling
        // back to a broader root module could bypass the selected module's ACL.
        let input = crate::adapter::argv::resolve_command_argv(
            args,
            &module.input_schema,
            &route.command_path[1..],
        )
        .map_err(|error| {
            anyhow::Error::new(error).context(RouteRefusal {
                module_id: module.module_id.clone(),
            })
        })?;
        return Ok((module, input));
    }
    anyhow::bail!("Shim '{name}' has no matching declared command_path. Re-run apexe scan; no native fallback is permitted")
}

fn route_candidates<'module, 'route>(
    routes: &'route [Route],
    manifest: &Manifest,
    modules: &'module [ScannedModule],
) -> anyhow::Result<Vec<(Option<&'module ScannedModule>, &'route Route)>> {
    let mut routes: Vec<&Route> = routes.iter().collect();
    routes.sort_by_key(|route| std::cmp::Reverse(route.command_path.len()));
    let mut candidates = Vec::new();
    for route in routes {
        let module = modules
            .iter()
            .find(|module| module.module_id == route.module_id && manifest.filter.admits(module));
        if let Some(module) = module {
            eligible(module)?;
            anyhow::ensure!(
                command_path(module)? == route.command_path,
                "Shim command_path changed; reinstall shims"
            );
        }
        candidates.push((module, route));
    }
    Ok(candidates)
}

fn execute(
    name: &str,
    args: &[String],
    manifest: &Manifest,
    config: &ApexeConfig,
) -> anyhow::Result<i32> {
    let started = std::time::Instant::now();
    let context = super::local::context("local", false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let modules = crate::output::load_modules_from_dir(
        &manifest.bindings_dir,
        manifest.bindings_pattern.as_deref(),
    )?;
    let (selected, input) = match resolve_route(name, args, manifest, &modules) {
        Ok(resolved) => resolved,
        Err(error) => {
            runtime.block_on(audit_resolution_refusal(
                name,
                manifest,
                &modules,
                &context,
                config,
                &error,
                started.elapsed().as_millis() as u64,
            ));
            return Err(error);
        }
    };
    let module_id = selected.module_id.clone();
    let stdin = stdin_mode(selected);
    let executor = build_executor(manifest, config, modules, stdin)?;
    let output = runtime.block_on(executor.call(&module_id, input, Some(&context), None))?;
    output["exit_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .ok_or_else(|| anyhow::anyhow!("Passthrough returned no exit status"))
}

pub(super) fn stdin_mode(selected: &ScannedModule) -> StdinMode {
    if selected.input_schema["x-apexe-streaming-stdin-safe"] == true {
        StdinMode::Inherit
    } else {
        StdinMode::Null
    }
}

fn build_executor(
    manifest: &Manifest,
    config: &ApexeConfig,
    modules: Vec<ScannedModule>,
    stdin: StdinMode,
) -> anyhow::Result<std::sync::Arc<apcore::Executor>> {
    let acl = super::local::acl_path(manifest.acl.as_deref(), config)?;
    let options = super::local::options(
        config,
        &manifest.bindings_dir,
        manifest.bindings_pattern.as_deref(),
        acl.as_deref(),
        manifest.filter.clone(),
    );
    Ok(
        crate::module::registry::build_executor_from_modules_with_io(
            &options,
            modules,
            ExecutionIo::Passthrough { stdin },
        )?,
    )
}

pub(super) async fn audit_resolution_refusal(
    name: &str,
    manifest: &Manifest,
    modules: &[ScannedModule],
    context: &apcore::Context<serde_json::Value>,
    config: &ApexeConfig,
    error: &anyhow::Error,
    duration_ms: u64,
) {
    let selected = error.downcast_ref::<RouteRefusal>().and_then(|refusal| {
        modules
            .iter()
            .find(|module| module.module_id == refusal.module_id)
    });
    let module = selected.or_else(|| {
        manifest.commands.get(name).and_then(|routes| {
            routes.iter().find_map(|route| {
                modules
                    .iter()
                    .find(|module| module.module_id == route.module_id)
            })
        })
    });
    crate::governance::AuditManager::new(&config.audit_log)
        .log_refusal(
            module.map_or("<shim-route>", |module| module.module_id.as_str()),
            &context.trace_id,
            context.caller_id.as_deref(),
            None,
            if module.is_some() {
                apcore::ErrorCode::GeneralInvalidInput
            } else {
                apcore::ErrorCode::ModuleNotFound
            },
            duration_ms,
        )
        .await;
}
