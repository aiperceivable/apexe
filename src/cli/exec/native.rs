use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apcore::Context;
use apcore_toolkit::ScannedModule;
use serde_json::{json, Value};

use super::UnmanagedPolicy;
use crate::cli::shim::{Manifest, Route};
use crate::config::ApexeConfig;
use crate::module::{CliModule, ExecutionIo, StdinMode};

pub(super) struct NativeInvocation {
    pub module: ScannedModule,
    pub input: Value,
    pub unmanaged: Option<CliModule>,
}

pub(super) async fn resolve(
    args: &[String],
    policy: UnmanagedPolicy,
    modules: &[ScannedModule],
    config: &ApexeConfig,
    bindings: &Path,
    context: &Context<Value>,
) -> anyhow::Result<NativeInvocation> {
    let started = std::time::Instant::now();
    let manifest = build_manifest(modules, config, bindings)?;
    let name = Path::new(&args[0])
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Native command must have a UTF-8 file name"))?;
    match resolve_invocation(name, args, policy, modules, &manifest, config) {
        Ok(invocation) => Ok(invocation),
        Err(error) => {
            super::super::shim::audit_resolution_refusal(
                name,
                &manifest,
                modules,
                context,
                config,
                &error,
                started.elapsed().as_millis() as u64,
            )
            .await;
            Err(error)
        }
    }
}

fn build_manifest(
    modules: &[ScannedModule],
    config: &ApexeConfig,
    bindings: &Path,
) -> anyhow::Result<Manifest> {
    let mut commands: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    for module in modules {
        if let Some(route) = binding_route(module) {
            commands
                .entry(route.command_path[0].clone())
                .or_default()
                .push(route);
        }
    }
    retain_installed_routes(&mut commands, bindings)?;
    Ok(Manifest {
        version: super::super::shim::MANIFEST_VERSION,
        executable: std::env::current_exe()?,
        bindings_dir: bindings.to_path_buf(),
        bindings_pattern: config.bindings_pattern.clone(),
        acl: None,
        filter: Default::default(),
        commands,
        activation_script: None,
        deactivation_script: None,
        source_scripts_dir: None,
    })
}

fn binding_route(module: &ScannedModule) -> Option<Route> {
    let target = module.target.strip_prefix("exec://")?;
    let parts: Vec<&str> = target.split_whitespace().collect();
    let binary = parts.first()?;
    let fallback = Path::new(binary).file_name()?.to_str()?;
    let path = module
        .metadata
        .get("command_path")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| {
            std::iter::once(fallback.to_string())
                .chain(parts[1..].iter().map(|part| (*part).into()))
                .collect()
        });
    Some(Route {
        module_id: module.module_id.clone(),
        command_path: path,
    })
}

fn retain_installed_routes(
    commands: &mut BTreeMap<String, Vec<Route>>,
    bindings: &Path,
) -> anyhow::Result<()> {
    let directory = std::env::var_os("APEXE_SHIM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(crate::runtime_path::default_shim_dir);
    if !directory
        .join(crate::runtime_path::MANIFEST_NAME)
        .try_exists()?
    {
        return Ok(());
    }
    let installed = super::super::shim::read_manifest(&directory)?;
    if installed.bindings_dir.canonicalize()? != bindings.canonicalize()? {
        return Ok(());
    }
    for (name, routes) in installed.commands {
        let current = commands.entry(name).or_default();
        for route in routes {
            if !current
                .iter()
                .any(|existing| existing.module_id == route.module_id)
            {
                current.push(route);
            }
        }
    }
    Ok(())
}

fn resolve_invocation(
    name: &str,
    args: &[String],
    policy: UnmanagedPolicy,
    modules: &[ScannedModule],
    manifest: &Manifest,
    config: &ApexeConfig,
) -> anyhow::Result<NativeInvocation> {
    let binary = resolve_binary(&args[0])?;
    let registered_name = if manifest.commands.contains_key(name) {
        Some(name.to_string())
    } else {
        registered_binary_name(&binary, modules)
    };
    if let Some(name) = registered_name {
        let (module, input) =
            super::super::shim::resolve_route(&name, &args[1..], manifest, modules)?;
        let target = crate::scanner::resolver::extract_binary_path(&module.target)
            .ok_or_else(|| anyhow::anyhow!("Invalid CLI target"))?;
        anyhow::ensure!(
            Path::new(target).canonicalize()? == binary.canonicalize()?,
            "Resolved executable differs from the binding for {}; rescan with the intended PATH",
            module.module_id
        );
        return Ok(NativeInvocation {
            module: module.clone(),
            input,
            unmanaged: None,
        });
    }
    anyhow::ensure!(policy != UnmanagedPolicy::Deny,
        "Command '{name}' is unmanaged; scan it with a matching authoritative overlay or explicitly select --unmanaged allow/approval");
    unmanaged_invocation(name, &binary, &args[1..], policy, config)
}

fn registered_binary_name(binary: &Path, modules: &[ScannedModule]) -> Option<String> {
    let actual = binary.canonicalize().ok()?;
    modules.iter().find_map(|module| {
        let target = crate::scanner::resolver::extract_binary_path(&module.target)?;
        if Path::new(target).canonicalize().ok()? != actual {
            return None;
        }
        module
            .metadata
            .get("command_path")
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| Path::new(target).file_name()?.to_str().map(str::to_owned))
    })
}

fn resolve_binary(command: &str) -> anyhow::Result<PathBuf> {
    let binary = which::which_in(
        command,
        Some(crate::runtime_path::search_path()),
        std::env::current_dir()?,
    )?;
    anyhow::ensure!(
        !binary
            .parent()
            .is_some_and(crate::runtime_path::is_shim_directory),
        "Native execution cannot target another shim"
    );
    // A symlink alias must not hide an apexe shim outside its installed directory.
    let canonical = binary.canonicalize()?;
    anyhow::ensure!(
        canonical != std::env::current_exe()?.canonicalize()?,
        "Native execution cannot target apexe itself"
    );
    Ok(binary)
}

fn unmanaged_invocation(
    name: &str,
    binary: &Path,
    args: &[String],
    policy: UnmanagedPolicy,
    config: &ApexeConfig,
) -> anyhow::Result<NativeInvocation> {
    anyhow::ensure!(
        super::super::shim::valid_name(name),
        "Invalid unmanaged command name"
    );
    let binary = binary
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Executable path must be valid UTF-8"))?;
    let mut scanned = ScannedModule::new(
        format!("unmanaged.{name}"),
        "Explicit unmanaged native command; arguments have no schema-derived path guard".into(),
        json!({"type":"object","properties":{"argv":{"type":"array","items":{"type":"string"}}},"required":["argv"],"additionalProperties":false}),
        json!({"type":"object"}),
        vec!["unmanaged".into()],
        format!("exec://{binary}"),
    );
    let annotations = apcore::ModuleAnnotations {
        requires_approval: policy == UnmanagedPolicy::Approval,
        open_world: true,
        ..Default::default()
    };
    scanned.annotations = Some(annotations);
    tracing::warn!(module_id = %scanned.module_id, policy = ?policy,
        "Explicit unmanaged execution has no schema-based argument path guard");
    let module = build_unmanaged_module(&scanned, binary, args, config)?;
    Ok(NativeInvocation {
        module: scanned,
        input: json!({"argv":args}),
        unmanaged: Some(module),
    })
}

fn build_unmanaged_module(
    scanned: &ScannedModule,
    binary: &str,
    args: &[String],
    config: &ApexeConfig,
) -> anyhow::Result<CliModule> {
    Ok(CliModule::new(crate::module::cli_module::CliModuleConfig {
        module_id: scanned.module_id.clone(),
        description: scanned.description.clone(),
        input_schema: scanned.input_schema.clone(),
        output_schema: scanned.output_schema.clone(),
        annotations: scanned.annotations.clone().unwrap_or_default(),
        binary_path: binary.into(),
        command_parts: vec![],
        json_flag: None,
        timeout_ms: config.default_timeout * 1000,
        max_output_bytes: crate::module::executor::DEFAULT_MAX_OUTPUT_BYTES,
    })
    .with_fixed_argv(args.to_vec())?
    .with_execution_io(ExecutionIo::Passthrough {
        stdin: StdinMode::Null,
    }))
}
