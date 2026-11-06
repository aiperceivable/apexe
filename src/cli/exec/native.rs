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
        scanned: crate::cli::ScannedPolicy::Off,
        activation_script: None,
        deactivation_script: None,
        source_scripts_dir: None,
    })
}

/// The route a reviewed binding contributes to native execution.
///
/// A route whose grammar is heuristic is left out deliberately. `apexe exec` is
/// the reviewed path — `--scanned` is a shim policy, not a namespace — so a
/// module the scan only guessed at must surface as a coverage gap ("unmanaged")
/// rather than execute because some other surface admitted it.
fn binding_route(module: &ScannedModule) -> Option<Route> {
    if !crate::models::route_is_reviewed(&module.metadata) {
        return None;
    }
    let target = module.target.strip_prefix("exec://")?;
    let parts: Vec<String> = shell_words::split(target).ok()?;
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
                .chain(parts[1..].iter().cloned())
                .collect()
        });
    Some(Route {
        module_id: module.module_id.clone(),
        command_path: path,
        tier: super::super::shim::RouteTier::Verified,
        tombstone: false,
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
            // An installed `--scanned` route is admitted by the shim policy that
            // built that manifest, not by this call. Native execution keeps its
            // own rule: reviewed grammar only.
            if route.tier != super::super::shim::RouteTier::Verified {
                continue;
            }
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
    let registered_name = manifest_route_name(name, &binary, modules, manifest);
    if let Some(name) = registered_name {
        let (module, input, _) =
            super::super::shim::resolve_route(&name, &args[1..], manifest, modules)?;
        let target = crate::scanner::resolver::extract_binary_path(&module.target)
            .ok_or_else(|| anyhow::anyhow!("Invalid CLI target"))?;
        anyhow::ensure!(
            Path::new(&target).canonicalize()? == binary.canonicalize()?,
            "Resolved executable differs from the binding for {}; rescan with the intended PATH",
            module.module_id
        );
        return Ok(NativeInvocation {
            module: module.clone(),
            input,
            unmanaged: None,
        });
    }
    // A binding without reviewed grammar is a coverage gap, not an unmanaged
    // command: `--unmanaged` exists for tools apexe has never described, and
    // letting it stand in for a binding apexe knows about would turn "we
    // guessed wrong" into a raw execution. The operator is told to fix the
    // grammar instead.
    if registered_module(&binary, modules).is_some() {
        return Err(super::super::shim::coverage_refusal(format!(
            "Command '{name}' has a binding but no reviewed grammar for this route; --unmanaged cannot substitute for coverage. Scan it with an overlay that covers this command"
        ))
        .into());
    }
    if policy == UnmanagedPolicy::Deny {
        return Err(super::super::shim::coverage_refusal(format!(
            "Command '{name}' is unmanaged: no binding for it carries reviewed grammar. Scan it with an overlay that covers this route, or explicitly select --unmanaged allow/approval"
        ))
        .into());
    }
    unmanaged_invocation(name, &binary, &args[1..], policy, config)
}

/// The route name to use for a command, when the manifest carries routes for it.
///
/// A binding can exist with no route: `binding_route` withholds every module
/// whose grammar the scan only guessed at. Reporting the binding's name anyway
/// turned that gap into "Shim 'git' has no registered route; re-run apexe env
/// --enable", which is `apexe exec` misdescribing itself as a shim and pointing
/// at the wrong remedy. Falling through instead reaches the honest answer:
/// the command is unmanaged because nothing covers it with reviewed grammar.
fn manifest_route_name(
    name: &str,
    binary: &Path,
    modules: &[ScannedModule],
    manifest: &Manifest,
) -> Option<String> {
    if manifest.commands.contains_key(name) {
        return Some(name.to_string());
    }
    registered_binary_name(binary, modules)
        .filter(|candidate| manifest.commands.contains_key(candidate))
}

fn registered_module<'a>(binary: &Path, modules: &'a [ScannedModule]) -> Option<&'a ScannedModule> {
    let actual = binary.canonicalize().ok()?;
    modules.iter().find(|module| {
        crate::scanner::resolver::extract_binary_path(&module.target)
            .and_then(|target| Path::new(&target).canonicalize().ok())
            .is_some_and(|target| target == actual)
    })
}

fn registered_binary_name(binary: &Path, modules: &[ScannedModule]) -> Option<String> {
    let module = registered_module(binary, modules)?;
    let target = crate::scanner::resolver::extract_binary_path(&module.target)?;
    module
        .metadata
        .get("command_path")
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| Path::new(&target).file_name()?.to_str().map(str::to_owned))
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
        timeout_ms: config.timeout_milliseconds()?,
        max_output_bytes: crate::module::executor::DEFAULT_MAX_OUTPUT_BYTES,
    })
    .with_fixed_argv(args.to_vec())?
    .with_execution_io(ExecutionIo::Passthrough {
        stdin: StdinMode::Null,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::shim::{Manifest, RouteTier};
    use serde_json::json;
    use std::collections::BTreeMap;

    /// A binding for `git status` whose grammar the converter stamped `scanned`.
    fn guessed_route() -> ScannedModule {
        let binary = std::env::current_exe().expect("test binary path");
        let mut module = ScannedModule::new(
            "cli.git.status".into(),
            "git status".into(),
            json!({"type": "object", "properties": {}}),
            json!({"type": "object"}),
            vec!["test".into()],
            format!("exec://{} status", binary.display()),
        );
        module
            .metadata
            .insert("command_path".into(), json!(["git", "status"]));
        module
            .metadata
            .insert(crate::models::GRAMMAR_KEY.into(), json!("scanned"));
        module.metadata.insert(
            "shim_contract".into(),
            json!({"authoritative": false, "variant": "unknown"}),
        );
        module
    }

    fn manifest() -> Manifest {
        Manifest {
            version: crate::cli::shim::MANIFEST_VERSION,
            executable: std::env::current_exe().expect("test binary path"),
            bindings_dir: std::env::temp_dir(),
            bindings_pattern: None,
            acl: None,
            filter: Default::default(),
            commands: BTreeMap::new(),
            scanned: crate::cli::ScannedPolicy::Off,
            activation_script: None,
            deactivation_script: None,
            source_scripts_dir: None,
        }
    }

    #[test]
    fn test_binding_route_withholds_a_route_whose_grammar_is_heuristic() {
        assert!(binding_route(&guessed_route()).is_none());
    }

    #[test]
    fn test_manifest_route_name_ignores_a_binding_with_no_route() {
        // The binding exists and the manifest carries no route for it, so the
        // caller must fall through to "unmanaged" rather than report a shim
        // with no registered route — the wrong surface and the wrong remedy.
        let modules = [guessed_route()];
        let binary = std::env::current_exe().expect("test binary path");
        assert_eq!(
            manifest_route_name("git", &binary, &modules, &manifest()),
            None
        );
    }

    #[test]
    fn test_manifest_route_name_keeps_a_name_the_manifest_carries() {
        let mut manifest = manifest();
        manifest.commands.insert(
            "git".into(),
            vec![Route {
                module_id: "cli.git.status".into(),
                command_path: vec!["git".into(), "status".into()],
                tier: RouteTier::Verified,
                tombstone: false,
            }],
        );
        let binary = std::env::current_exe().expect("test binary path");
        assert_eq!(
            manifest_route_name("git", &binary, &[guessed_route()], &manifest),
            Some("git".into())
        );
    }
}
