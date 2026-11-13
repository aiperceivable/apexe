use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apcore::Context;
use apcore_toolkit::ScannedModule;
use serde_json::{json, Value};

use super::UnmanagedPolicy;
use crate::adapter::argv::OpaqueTail;
use crate::cli::shim::{Manifest, Route};
use crate::config::ApexeConfig;
use crate::module::{CliModule, ExecutionIo, StdinMode};

pub(super) struct NativeInvocation {
    pub module: ScannedModule,
    pub input: Value,
    pub unmanaged: Option<CliModule>,
    pub opaque_tail: Option<OpaqueTail>,
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
    extend_with_verified_routes(commands, installed.commands);
    Ok(())
}

/// Add the routes of an installed shim manifest that native execution may use.
///
/// A command whose installed routes are all `scanned` contributes nothing here:
/// it is admissible through the shim that installed it, not through `apexe
/// exec`. Inserting the name with an empty route list made `manifest_route_name`
/// treat it as a native route, so the refusal came out of the shim's resolver —
/// advising `apexe env --reload --scanned all`, a flag this subcommand does not
/// have and a policy that cannot reach native execution. An absent entry falls
/// through to the honest answer: the binding exists, its grammar is not
/// reviewed, so the route is a coverage gap.
fn extend_with_verified_routes(
    commands: &mut BTreeMap<String, Vec<Route>>,
    installed: BTreeMap<String, Vec<Route>>,
) {
    for (name, routes) in installed {
        let verified: Vec<Route> = routes
            .into_iter()
            .filter(|route| route.tier == super::super::shim::RouteTier::Verified)
            .collect();
        if verified.is_empty() {
            continue;
        }
        let current = commands.entry(name).or_default();
        for route in verified {
            if !current
                .iter()
                .any(|existing| existing.module_id == route.module_id)
            {
                current.push(route);
            }
        }
    }
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
            input: input.input,
            unmanaged: None,
            opaque_tail: input.opaque_tail,
        });
    }
    // A binding without reviewed grammar is a coverage gap, not an unmanaged
    // command: `--unmanaged` exists for tools apexe has never described, and
    // letting it stand in for a binding apexe knows about would turn "we
    // guessed wrong" into a raw execution. The operator is told to fix the
    // grammar instead.
    if registered_module(&binary, modules).is_some() {
        return Err(
            coverage_gap_error(name, selected_binding(&binary, name, &args[1..], modules)).into(),
        );
    }
    if policy == UnmanagedPolicy::Deny {
        return Err(super::super::shim::coverage_refusal(format!(
            "Command '{name}' is unmanaged: no binding for it carries reviewed grammar. Scan it with an overlay that covers this route, or explicitly select --unmanaged allow/approval"
        ))
        .into());
    }
    unmanaged_invocation(name, &binary, &args[1..], policy, config)
}

/// The refusal an `apexe exec` caller gets for a route the scan only guessed at.
///
/// Native execution is the reviewed path, so the message names the binding the
/// caller actually asked for and the two surfaces that would work — its shim in
/// a governed shell, or the module with `--input` — instead of the shim's
/// remedy. It deliberately does not mention `env --reload --scanned all`: that
/// flag configures shims and cannot reach this surface, and an operator who
/// follows it changes a policy that was never the deciding layer.
fn coverage_gap_error(name: &str, module_id: Option<&str>) -> apcore::ModuleError {
    let subject = module_id.map_or_else(
        || format!("Command '{name}' has a binding"),
        |module_id| format!("Command '{name}' has a binding ({module_id})"),
    );
    let call = module_id.map_or_else(
        || "call the module with --input".to_string(),
        |module_id| format!("call {module_id} with --input"),
    );
    super::super::shim::coverage_refusal(format!(
        "{subject} but no reviewed grammar for this route: `apexe exec` executes \
         reviewed grammar only, and --unmanaged cannot substitute for coverage. Run it \
         through its shim in a governed shell, or {call}; to make native argv available, \
         cover the route in a curated overlay and rescan"
    ))
}

/// The binding the caller actually asked for, when one can be identified.
///
/// [`registered_module`] only proves that *some* module targets the binary:
/// `bun` has six, and naming the first one made the refusal report
/// `cli.bun.exec` for a call to `bun run`. The route is the longest declared
/// command path whose tail the arguments start with — the same rule the shim
/// uses to select a route — among the modules that target this binary.
fn selected_binding<'a>(
    binary: &Path,
    name: &str,
    args: &[String],
    modules: &'a [ScannedModule],
) -> Option<&'a str> {
    let actual = binary.canonicalize().ok()?;
    let mut candidates: Vec<(&str, usize)> = modules
        .iter()
        .filter_map(|module| {
            let target = crate::scanner::resolver::extract_binary_path(&module.target)?;
            if Path::new(&target).canonicalize().ok()? != actual {
                return None;
            }
            let path: Vec<String> = module
                .metadata
                .get("command_path")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())?;
            if path.first().map(String::as_str) != Some(name) {
                return None;
            }
            let tail = &path[1..];
            let matches = tail.len() <= args.len()
                && tail
                    .iter()
                    .zip(args)
                    .all(|(expected, given)| expected == given);
            matches.then_some((module.module_id.as_str(), path.len()))
        })
        .collect();
    candidates.sort_by_key(|(_, length)| std::cmp::Reverse(*length));
    candidates.first().map(|(module_id, _)| *module_id)
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
        opaque_tail: None,
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
        timeout_ms: config.local_timeout_milliseconds()?,
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

    #[test]
    fn test_extend_with_verified_routes_skips_a_command_whose_routes_are_all_scanned() {
        // `apexe env --enable --scanned all` installs `bun: 6 scanned`. Those
        // routes are admissible through the shim, not through native execution,
        // and the name must not become an empty native entry: that entry is what
        // routed the refusal through the shim's resolver and told an `apexe exec`
        // caller to re-run `apexe env --reload --scanned all`.
        let mut commands: BTreeMap<String, Vec<Route>> = BTreeMap::new();
        let installed = BTreeMap::from([(
            "bun".to_string(),
            vec![Route {
                module_id: "cli.bun.run".into(),
                command_path: vec!["bun".into(), "run".into()],
                tier: RouteTier::Scanned,
                tombstone: false,
            }],
        )]);

        extend_with_verified_routes(&mut commands, installed);

        assert!(
            commands.is_empty(),
            "a scanned-only command must not become a native route"
        );
    }

    #[test]
    fn test_extend_with_verified_routes_adds_a_reviewed_route() {
        let mut commands: BTreeMap<String, Vec<Route>> = BTreeMap::new();
        let installed = BTreeMap::from([(
            "ls".to_string(),
            vec![Route {
                module_id: "cli.ls".into(),
                command_path: vec!["ls".into()],
                tier: RouteTier::Verified,
                tombstone: false,
            }],
        )]);

        extend_with_verified_routes(&mut commands, installed);

        assert_eq!(commands.get("ls").map(Vec::len), Some(1));
        assert_eq!(commands["ls"][0].module_id, "cli.ls");
    }

    #[test]
    fn test_coverage_gap_names_the_binding_and_the_surfaces_that_work() {
        let error = coverage_gap_error("bun", Some("cli.bun.run"));

        assert!(error.message.contains("cli.bun.run"), "{}", error.message);
        assert!(
            error.message.contains("reviewed grammar only"),
            "{}",
            error.message
        );
        assert!(error.message.contains("--input"), "{}", error.message);
        // The shim's remedy is inapplicable on this surface, and an agent that
        // follows it changes a policy that was never the deciding layer.
        assert!(!error.message.contains("--scanned"), "{}", error.message);
        assert!(!error.message.contains("env --reload"), "{}", error.message);
        assert_eq!(
            error.details.get("layer").and_then(|layer| layer.as_str()),
            Some("coverage")
        );
    }

    /// One `bun <tail>` binding that targets this test binary.
    fn bun_route(module_id: &str, tail: &str) -> ScannedModule {
        let binary = std::env::current_exe().expect("test binary path");
        let mut module = ScannedModule::new(
            module_id.into(),
            format!("bun {tail}"),
            json!({"type": "object", "properties": {}}),
            json!({"type": "object"}),
            vec!["test".into()],
            format!("exec://{} {tail}", binary.display()),
        );
        module
            .metadata
            .insert("command_path".into(), json!(["bun", tail]));
        module
            .metadata
            .insert(crate::models::GRAMMAR_KEY.into(), json!("scanned"));
        module
    }

    #[test]
    fn test_selected_binding_names_the_route_the_arguments_select() {
        // `bun` has six bindings; naming the first one told a `bun run` caller
        // that `cli.bun.exec` had no reviewed grammar.
        let modules = [
            bun_route("cli.bun.run", "run"),
            bun_route("cli.bun.exec", "exec"),
        ];
        let binary = std::env::current_exe().expect("test binary path");
        let args = ["run".to_string(), "test:extension".to_string()];

        assert_eq!(
            selected_binding(&binary, "bun", &args, &modules),
            Some("cli.bun.run")
        );
    }

    #[test]
    fn test_selected_binding_reports_none_when_no_declared_route_matches() {
        let modules = [bun_route("cli.bun.run", "run")];
        let binary = std::env::current_exe().expect("test binary path");
        let args = ["repl".to_string()];

        assert_eq!(selected_binding(&binary, "bun", &args, &modules), None);
    }
}
