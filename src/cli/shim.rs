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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum RouteTier {
    #[default]
    Verified,
    Scanned,
    /// Installed from a binding that predates the per-route grammar stamp and
    /// whose only claim is the legacy tool-level `authoritative` flag.
    ///
    /// Admitted like a scanned route — it is not reviewed — but labelled, so an
    /// operator can tell an inherited trust from an earned one (issue #47).
    Legacy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Route {
    pub module_id: String,
    pub command_path: Vec<String>,
    #[serde(default)]
    pub tier: RouteTier,
    /// Retained by `env --reload` after its module disappeared or was filtered
    /// out. A tombstone refuses execution by design; `env --check` reports it
    /// as a warning rather than a broken installation.
    #[serde(default)]
    pub tombstone: bool,
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
    pub scanned: crate::cli::ScannedPolicy,
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
    let parts: Vec<String> = shell_words::split(target)
        .map_err(|error| anyhow::anyhow!("Shim target has invalid quoting: {error}"))?;
    anyhow::ensure!(
        !parts.is_empty() && Path::new(&parts[0]).is_absolute(),
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
        && name != crate::cli::sh::ENTRY_NAME
        && name != "apexe-sh.exe"
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
    // Trust is per route, not per tool. `authoritative` records the overlay's
    // mode; `metadata.grammar` records what this route's flags actually are, so
    // one reviewed subcommand does not have to vouch for its 130 heuristic
    // siblings and vice versa. A binding written before the stamp existed falls
    // back to the tool-level contract, which is the stricter of the two.
    anyhow::ensure!(
        crate::models::route_is_reviewed(&module.metadata)
            && Some(contract.variant.as_str())
                == module
                    .metadata
                    .get("variant")
                    .and_then(serde_json::Value::as_str),
        "{} has no reviewed grammar for this route; rescan with an overlay that covers it, or install shims with --scanned",
        module.module_id
    );
    anyhow::ensure!(
        contract.cross_flag_ordering == Some(false)
            && module.input_schema["x-apexe-cross-flag-ordering"] == false,
        "{} requires cross-flag ordering",
        module.module_id
    );
    validate_capabilities(module, &contract)?;
    scannable(module)
}

/// Structural checks shared by reviewed and scanned routes.
///
/// Scanned routes deliberately stop here: they do not require an authoritative
/// overlay, but they do require a current binding format, a usable command
/// path and a reachable binary.
pub(super) fn scannable(module: &ScannedModule) -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::output::has_current_scan_format(module),
        "{} needs a current-format rescan",
        module.module_id
    );
    command_path(module)?;
    let binary = crate::scanner::resolver::extract_binary_path(&module.target)
        .ok_or_else(|| anyhow::anyhow!("Invalid CLI target"))?;
    anyhow::ensure!(
        crate::scanner::resolver::binary_is_reachable(&binary),
        "Binary for {} is unavailable",
        module.module_id
    );
    anyhow::ensure!(
        !Path::new(&binary)
            .parent()
            .is_some_and(crate::runtime_path::is_shim_directory),
        "Binding targets another shim; rescan with a clean PATH"
    );
    Ok(())
}

/// Whether a module without an authoritative overlay may be exposed as a
/// scanned route under `policy`.
pub(super) fn scanned_admits(module: &ScannedModule, policy: crate::cli::ScannedPolicy) -> bool {
    if policy == crate::cli::ScannedPolicy::Off || scannable(module).is_err() {
        return false;
    }
    if policy == crate::cli::ScannedPolicy::All {
        return true;
    }
    module.annotations.as_ref().is_some_and(|annotations| {
        annotations.readonly && !annotations.destructive && !annotations.open_world
    })
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

/// Build a refusal caused by coverage — no reviewed grammar for the route —
/// rather than by policy.
///
/// Two things this buys. `GeneralInvalidInput` instead of
/// `GeneralInternalError`: nothing inside apexe failed, the route is simply not
/// part of the declared surface, and an agent reading `--error-format json`
/// must not be told to treat that as a bug. And `details.layer`: coverage is
/// decided *before* the ACL is evaluated, so a caller otherwise cannot tell
/// which of the two gates refused it.
pub(super) fn coverage_refusal(message: String) -> apcore::ModuleError {
    let mut error = apcore::ModuleError::new(apcore::ErrorCode::GeneralInvalidInput, message);
    let _ = error
        .details
        .insert("layer".to_string(), serde_json::json!("coverage"));
    error.with_retryable(false).with_ai_guidance(
        "This is a coverage refusal, not a policy denial: the ACL was not the deciding \
         layer. Open the route in the tool's curated overlay (a commands[] entry) and \
         rescan, or pass --unmanaged for a tool apexe has never described.",
    )
}

pub(super) fn resolve_route<'a>(
    name: &str,
    args: &[String],
    manifest: &Manifest,
    modules: &'a [ScannedModule],
) -> anyhow::Result<(
    &'a ScannedModule,
    crate::adapter::argv::ResolvedArgv,
    Option<Vec<String>>,
)> {
    let routes = manifest.commands.get(name).ok_or_else(|| {
        coverage_refusal(format!(
            "Shim '{name}' has no registered route; re-run apexe env --enable"
        ))
    })?;
    let candidates = route_candidates(routes, manifest, modules);
    let schemas: Vec<(std::borrow::Cow<'_, serde_json::Value>, bool)> = candidates
        .iter()
        .filter_map(|(module, route)| {
            module.map(|module| (route_schema(module, route), route.command_path.len() == 1))
        })
        .collect();
    let borrowed: Vec<(&serde_json::Value, bool)> = schemas
        .iter()
        .map(|(schema, root)| (schema.as_ref(), *root))
        .collect();
    let matcher = crate::adapter::argv::CommandMatcher::new(&borrowed)?;
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
        // Eligibility belongs to the route this call selected, so it is checked
        // here rather than while the candidate list is built. Checking every
        // sibling up front let one unusable binding refuse commands that never
        // touch it, and named a module the caller had not asked for: `git
        // status` was refused as `cli.git.add` because `add` sorts first.
        check_route(module, route).map_err(|error| {
            anyhow::anyhow!("Route '{}': {error:#}", route.command_path.join(" "))
        })?;
        // Once a subcommand is selected, any input refusal is final. Falling
        // back to a broader root module could bypass the selected module's ACL.
        let schema = route_schema(module, route);
        let input = crate::adapter::argv::resolve_command_argv(
            args,
            schema.as_ref(),
            &route.command_path[1..],
        )
        .map_err(|error| {
            anyhow::Error::new(error).context(RouteRefusal {
                module_id: module.module_id.clone(),
            })
        })?;
        let replay =
            matches!(route.tier, RouteTier::Scanned | RouteTier::Legacy).then(|| args.to_vec());
        reject_exec_parameters(module, &input.input)?;
        return Ok((module, input, replay));
    }
    Err(coverage_refusal(unmatched_route_message(name, args, modules)).into())
}

/// The schema this route validates against.
///
/// A scanned route validates the caller's argv and then forwards it unchanged,
/// so a short-flag bundle is checked flag by flag and is always safe to admit.
/// An overlay that is silent about bundling must not be read as "this tool
/// cannot bundle": `grep -rn`, `du -sh`, `cut -d:` are the spellings the tools
/// themselves accept, and refusing them made ordinary invocations fail closed
/// while a raw shell accepted them. Verified routes keep the overlay's answer,
/// because a native route re-emits the arguments it parsed.
fn route_schema<'a>(
    module: &'a ScannedModule,
    route: &Route,
) -> std::borrow::Cow<'a, serde_json::Value> {
    if matches!(route.tier, RouteTier::Scanned | RouteTier::Legacy)
        && module.input_schema["x-apexe-short-flag-bundling"].is_null()
    {
        let mut schema = module.input_schema.clone();
        schema["x-apexe-short-flag-bundling"] = serde_json::json!(true);
        return std::borrow::Cow::Owned(schema);
    }
    std::borrow::Cow::Borrowed(&module.input_schema)
}

/// Explain a route miss with the remedy that applies to it.
///
/// "Re-run apexe scan" is the wrong advice when the binding is current but the
/// route was withheld: `git add` under `--scanned readonly` has a fresh binding
/// and no route, and telling the operator to rescan a scanned tool sends them
/// in a circle. Naming the module and the policy is what makes the refusal
/// actionable — and this is the one place that knows both.
fn unmatched_route_message(name: &str, args: &[String], modules: &[ScannedModule]) -> String {
    let requested = args
        .iter()
        .find(|token| !token.starts_with('-') && token.as_str() != "--");
    let withheld = requested.and_then(|token| {
        modules.iter().find(|module| {
            module
                .metadata
                .get("command_path")
                .cloned()
                .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                .is_some_and(|path| path.len() == 2 && path[0] == name && &path[1] == token)
        })
    });
    // A route the overlay left closed has no binding any more, so the lookup
    // above cannot find it. The inventory recorded at scan time can: it is the
    // evidence that this name was scanned and deliberately not opened.
    if let Some(token) = requested {
        let route = format!("{name} {token}");
        let closed = modules.iter().any(|module| {
            module
                .metadata
                .get("scan_inventory")
                .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
                .is_some_and(|inventory| inventory.iter().any(|entry| entry == &route))
        });
        if closed {
            return format!(
                "'{route}' is not part of the declared surface for '{name}': a curated overlay scanned it and left it closed. Open it by adding a commands[] entry for ['{token}'] to that overlay and rescanning; --scanned lowers the grammar bar for declared routes and cannot widen the set"
            );
        }
    }
    match (withheld, requested) {
        (Some(module), Some(token)) => format!(
            "Shim '{name}' has no route for '{name} {token}'. The binding {} exists but no route was installed for it, which usually means the shims were installed with --scanned off|readonly and this route is heuristic or outside that policy. Re-run `apexe env --reload --scanned all` to admit scanned routes, or scan the tool with an overlay that covers this command. No native fallback is permitted",
            module.module_id
        ),
        _ => format!(
            "'{name}' has no matching declared command_path for this invocation. If a curated \
             overlay covers '{name}', open this route in that overlay's commands[] and rescan; \
             otherwise run `apexe scan {name}`. No native fallback is permitted"
        ),
    }
}

/// Join every route to its registered module, longest command path first.
///
/// Deliberately not a validation pass: see [`resolve_route`]. A route that
/// cannot run is only a problem when it is the route the caller selected, and
/// validating the whole list here made the refusal name whichever sibling
/// happened to sort first.
fn route_candidates<'module, 'route>(
    routes: &'route [Route],
    manifest: &Manifest,
    modules: &'module [ScannedModule],
) -> Vec<(Option<&'module ScannedModule>, &'route Route)> {
    let mut routes: Vec<&Route> = routes.iter().collect();
    // `manifest.scanned` is the installed policy. It is re-checked at
    // execution time, not only while planning: without this a route installed
    // under `--scanned readonly|all` survives a downgrade to `off` and keeps
    // replaying the caller's argv.
    if manifest.scanned == crate::cli::ScannedPolicy::Off {
        routes.retain(|route| route.tier != RouteTier::Scanned);
    }
    routes.sort_by_key(|route| std::cmp::Reverse(route.command_path.len()));
    routes
        .into_iter()
        .map(|route| {
            let module = modules.iter().find(|module| {
                module.module_id == route.module_id && manifest.filter.admits(module)
            });
            (module, route)
        })
        .collect()
}

/// Refuse a parsed native call that fills a parameter the wrapped tool executes.
///
/// Module mode reaches this through `build_argv`, which renders the input into
/// argv. A `Scanned` route does not render: it replays the caller's argv, so the
/// only chance to notice `git -c core.pager=<cmd>` is here, on the parsed input
/// that both tiers produce. Without this the `executes` assertion held for
/// module calls and silently did nothing for the native argv path.
fn reject_exec_parameters(module: &ScannedModule, input: &serde_json::Value) -> anyhow::Result<()> {
    let Some(kwargs) = input.as_object() else {
        return Ok(());
    };
    crate::module::executor::reject_exec_parameters(kwargs, Some(&module.input_schema))
        .map_err(|error| anyhow::anyhow!("{}", error.message))
}

/// Validate the one route the caller selected.
///
/// A `Verified` route needs the authoritative overlay contract that makes its
/// grammar reviewed; a `Scanned` route only needs a usable, reachable binding.
/// Both must still agree with the manifest's `command_path`.
fn check_route(module: &ScannedModule, route: &Route) -> anyhow::Result<()> {
    match route.tier {
        RouteTier::Verified => eligible(module)?,
        // A legacy route runs on the same structural checks as a scanned one:
        // nothing stamped its grammar, so nothing may vouch for it (issue #47).
        RouteTier::Scanned | RouteTier::Legacy => scannable(module)?,
    }
    anyhow::ensure!(
        command_path(module)? == route.command_path,
        "Shim command_path changed; reinstall shims"
    );
    Ok(())
}

fn execute(
    name: &str,
    args: &[String],
    manifest: &Manifest,
    config: &ApexeConfig,
) -> anyhow::Result<i32> {
    let started = std::time::Instant::now();
    let context = super::local::context(super::local::LOCAL_CALLER, false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let modules = crate::output::load_modules_from_dir(
        &manifest.bindings_dir,
        manifest.bindings_pattern.as_deref(),
    )?;
    let (selected, input, replay) = match resolve_route(name, args, manifest, &modules) {
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
    let stdin = passthrough_stdin_mode();
    let executor = match replay {
        Some(argv) => build_executor_for_scanned(manifest, config, selected, argv, stdin)?,
        None => build_executor(
            manifest,
            config,
            modules,
            &module_id,
            input.opaque_tail,
            stdin,
        )?,
    };
    let output = runtime.block_on(executor.call(&module_id, input.input, Some(&context), None))?;
    output["exit_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .ok_or_else(|| anyhow::anyhow!("Passthrough returned no exit status"))
}

/// Stdin mode for a local passthrough call.
///
/// A shim stands in for the command the caller typed, so the caller's stdin is
/// the command's stdin: `... | grep x` must read the pipe, exactly as the
/// unwrapped `grep` would. Every passthrough call site is a local shell
/// surface (shim or `apexe exec` native argv); captured execution — `apexe
/// serve`, MCP, A2A — never reaches here and keeps [`StdinMode::Null`].
///
/// This used to be gated on the module's `x-apexe-streaming-stdin-safe`
/// assertion and silently downgraded to [`StdinMode::Null`] otherwise, which
/// closed stdin for every scanned route: `printf 'a\nb\n' | wc -l` answered
/// `0` and `... | grep x` reported no match. The assertion cannot be the gate
/// here, because the caller already owns the descriptor.
pub(super) fn passthrough_stdin_mode() -> StdinMode {
    StdinMode::Inherit
}

fn build_executor_for_scanned(
    manifest: &Manifest,
    config: &ApexeConfig,
    selected: &ScannedModule,
    argv: Vec<String>,
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
    Ok(crate::module::registry::build_executor_for_scanned(
        &options,
        selected,
        argv,
        ExecutionIo::Passthrough { stdin },
    )?)
}

fn build_executor(
    manifest: &Manifest,
    config: &ApexeConfig,
    modules: Vec<ScannedModule>,
    module_id: &str,
    tail: Option<crate::adapter::argv::OpaqueTail>,
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
    let io = ExecutionIo::Passthrough { stdin };
    if let Some(tail) = tail {
        let selected = modules
            .iter()
            .find(|module| module.module_id == module_id)
            .ok_or_else(|| anyhow::anyhow!("Module not found: {module_id}"))?;
        let module = crate::module::CliModule::from_scanned(selected, options.timeout_ms)?
            .with_execution_io(io)
            .with_opaque_tail(Some(tail));
        return Ok(crate::module::registry::build_executor_for_invocation(
            &options, selected, module,
        )?);
    }
    Ok(crate::module::registry::build_executor_from_modules_with_io(&options, modules, io)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binary() -> PathBuf {
        std::env::current_exe().expect("test binary path")
    }

    fn route(module_id: &str, path: &[&str]) -> Route {
        Route {
            module_id: module_id.into(),
            command_path: path.iter().map(|part| (*part).into()).collect(),
            tier: RouteTier::Verified,
            tombstone: false,
        }
    }

    /// A binding that `eligible` accepts only when `authoritative` is set.
    ///
    /// Mirrors what `apexe scan` writes for a subcommand route: current format,
    /// matching variant, contract capabilities agreeing with the schema markers,
    /// and a `command_path` that agrees with the target's argv tail.
    fn module(module_id: &str, path: &[&str], authoritative: bool) -> ScannedModule {
        let argv_tail = path[1..].join(" ");
        let target = if argv_tail.is_empty() {
            format!("exec://{}", binary().display())
        } else {
            format!("exec://{} {argv_tail}", binary().display())
        };
        let schema = json!({
            "type": "object",
            "properties": {"short": {"type": "boolean", "x-apexe-flag": "--short"}},
            "additionalProperties": false,
            "x-apexe-short-flag-bundling": false,
            "x-apexe-streaming-stdin-safe": false,
            "x-apexe-cross-flag-ordering": false
        });
        let mut module = ScannedModule::new(
            module_id.into(),
            format!("{module_id} test binding"),
            schema,
            json!({"type": "object"}),
            vec!["test".into()],
            target,
        );
        module.metadata.insert("command_path".into(), json!(path));
        module.metadata.insert("variant".into(), json!("unknown"));
        module.metadata.insert(
            "apexe_scan_format".into(),
            json!(crate::output::yaml::SCAN_FORMAT_VERSION),
        );
        module.metadata.insert(
            "shim_contract".into(),
            json!({
                "authoritative": authoritative,
                "variant": "unknown",
                "short_flag_bundling": false,
                "streaming_stdin_safe": false,
                "cross_flag_ordering": false
            }),
        );
        // Stamped by the converter, one value per route.
        module.metadata.insert(
            crate::models::GRAMMAR_KEY.into(),
            json!(if authoritative { "reviewed" } else { "scanned" }),
        );
        module
    }

    fn manifest(routes: Vec<Route>) -> Manifest {
        Manifest {
            version: MANIFEST_VERSION,
            executable: binary(),
            bindings_dir: std::env::temp_dir(),
            bindings_pattern: None,
            acl: None,
            filter: ModuleFilter::default(),
            commands: BTreeMap::from([("git".to_string(), routes)]),
            scanned: crate::cli::ScannedPolicy::Off,
            activation_script: None,
            deactivation_script: None,
            source_scripts_dir: None,
        }
    }

    #[test]
    fn test_resolve_route_ignores_an_ineligible_sibling() {
        // `git add` has no authoritative overlay; `git status` has one.
        let modules = [
            module("cli.git.add", &["git", "add"], false),
            module("cli.git.status", &["git", "status"], true),
        ];
        let manifest = manifest(vec![
            route("cli.git.add", &["git", "add"]),
            route("cli.git.status", &["git", "status"]),
        ]);

        let (selected, input, replay) = resolve_route(
            "git",
            &["status".to_string(), "--short".to_string()],
            &manifest,
            &modules,
        )
        .expect("an eligible route must resolve despite an ineligible sibling");

        assert_eq!(selected.module_id, "cli.git.status");
        assert_eq!(input.input["short"], json!(true));
        assert!(replay.is_none());
    }

    /// A scanned-tier binding whose tool-level `-c` is marked `executes`.
    fn exec_module(module_id: &str, path: &[&str]) -> ScannedModule {
        let mut module = module(module_id, path, false);
        module.input_schema["properties"] = json!({
            "c": {
                "type": "string",
                "x-apexe-flag": "-c",
                "x-apexe-flag-position": "before-subcommand",
                "x-apexe-exec": true
            },
            "short": {"type": "boolean", "x-apexe-flag": "--short"}
        });
        module
    }

    #[test]
    fn test_unmatched_route_names_a_binding_that_was_withheld_by_policy() {
        // `git add` has a binding but no route under `--scanned readonly`.
        // "Re-run apexe scan" would be circular advice; the message has to name
        // the module and the policy that withheld it.
        let modules = [module("cli.git.add", &["git", "add"], false)];
        let message = unmatched_route_message(
            "git",
            &["add".to_string(), "README.md".to_string()],
            &modules,
        );
        assert!(message.contains("cli.git.add"), "{message}");
        assert!(message.contains("--scanned"), "{message}");
        assert!(
            !message.contains("Re-run apexe scan;"),
            "a current binding must not be answered with a rescan: {message}"
        );
    }

    #[test]
    fn test_unmatched_route_reports_a_route_the_overlay_left_closed() {
        // After `--prune` there is no binding for a closed route, so the refusal
        // can only be explained from the inventory the scan recorded.
        let mut module = module("cli.git.status", &["git", "status"], false);
        module
            .metadata
            .insert("scan_inventory".into(), json!(["git log", "git push"]));
        let modules = [module];
        let message = unmatched_route_message("git", &["log".to_string()], &modules);
        assert!(
            message.contains("not part of the declared surface"),
            "{message}"
        );
        assert!(message.contains("commands[]"), "{message}");
        assert!(message.contains("cannot widen the set"), "{message}");
    }

    #[test]
    fn test_unmatched_route_fallback_does_not_blame_a_shim() {
        // Reached when the binding carries no `scan_inventory` — an older or
        // hand-edited binding. The advice still has to be the overlay remedy,
        // and `apexe exec` must not be described as a shim it is not.
        let modules = [module("cli.git.status", &["git", "status"], true)];
        let message = unmatched_route_message("git", &["log".to_string()], &modules);
        assert!(!message.contains("Shim"), "{message}");
        assert!(message.contains("commands[]"), "{message}");
    }

    #[test]
    fn test_coverage_refusal_reports_the_coverage_layer_not_an_internal_error() {
        let error = coverage_refusal("'git log' is not part of the declared surface".into());
        assert_eq!(error.code, apcore::ErrorCode::GeneralInvalidInput);
        assert_eq!(
            error.details.get("layer").and_then(|value| value.as_str()),
            Some("coverage")
        );
    }

    #[test]
    fn test_eligible_accepts_a_reviewed_route_under_a_merge_contract() {
        // The point of the per-route stamp: a merge overlay never sets
        // `authoritative`, but a route whose whole flag surface is curated is
        // still reviewed, and must be usable natively.
        let mut module = module("cli.git.status", &["git", "status"], false);
        module
            .metadata
            .insert(crate::models::GRAMMAR_KEY.into(), json!("reviewed"));
        assert!(eligible(&module).is_ok());
    }

    #[test]
    fn test_eligible_refuses_a_scanned_route_under_an_authoritative_contract() {
        // And the other direction: a tool-level claim does not promote a route
        // the converter stamped as heuristic.
        let mut module = module("cli.git.stash", &["git", "stash"], true);
        module
            .metadata
            .insert(crate::models::GRAMMAR_KEY.into(), json!("scanned"));
        assert!(eligible(&module).is_err());
    }

    #[test]
    fn test_resolve_route_refuses_an_executed_parameter_on_a_scanned_route() {
        // A `Scanned` route replays the caller's argv instead of rendering
        // module input, so `x-apexe-exec` has no `build_argv` to run in and the
        // refusal has to happen on the parsed input instead.
        let modules = [exec_module("cli.git.status", &["git", "status"])];
        let mut manifest = manifest(vec![Route {
            module_id: "cli.git.status".into(),
            command_path: vec!["git".into(), "status".into()],
            tier: RouteTier::Scanned,
            tombstone: false,
        }]);
        // The route is only executable when the installed policy admits the
        // scanned tier; the test is about the exec-parameter guard, not policy.
        manifest.scanned = crate::cli::ScannedPolicy::All;

        let error = resolve_route(
            "git",
            &[
                "-c".to_string(),
                "core.pager=sh".to_string(),
                "status".to_string(),
            ],
            &manifest,
            &modules,
        )
        .expect_err("a value the wrapped tool would run must be refused");

        assert!(
            format!("{error:#}").contains("is refused"),
            "the refusal must name the parameter: {error:#}"
        );
    }

    #[test]
    fn test_scanned_route_is_refused_when_the_installed_policy_is_off() {
        // Regression: `manifest.scanned` was written but never read at
        // execution time, so `--reload --scanned off` left previously
        // installed scanned routes replaying the caller's argv.
        let modules = [exec_module("cli.git.status", &["git", "status"])];
        let manifest = manifest(vec![Route {
            module_id: "cli.git.status".into(),
            command_path: vec!["git".into(), "status".into()],
            tier: RouteTier::Scanned,
            tombstone: false,
        }]);
        assert_eq!(manifest.scanned, crate::cli::ScannedPolicy::Off);
        let error = resolve_route("git", &["status".to_string()], &manifest, &modules)
            .expect_err("a scanned route must not execute when the policy is off");
        let message = format!("{error:#}");
        assert!(
            message.contains("no matching declared command_path") || message.contains("no route"),
            "unexpected refusal: {message}"
        );
    }

    #[test]
    fn test_resolve_route_refusal_names_the_selected_route() {
        let modules = [
            module("cli.git.add", &["git", "add"], false),
            module("cli.git.status", &["git", "status"], true),
        ];
        let manifest = manifest(vec![
            route("cli.git.add", &["git", "add"]),
            route("cli.git.status", &["git", "status"]),
        ]);

        let error = resolve_route(
            "git",
            &["add".to_string(), "README.md".to_string()],
            &manifest,
            &modules,
        )
        .expect_err("an ineligible selected route must be refused");

        let message = format!("{error:#}");
        assert!(
            message.contains("Route 'git add'"),
            "refusal must name the selected command path: {message}"
        );
        assert!(
            message.contains("cli.git.add") && message.contains("no reviewed grammar"),
            "refusal must keep the reason: {message}"
        );
        assert!(
            !message.contains("cli.git.status"),
            "refusal must not mention a route the caller never asked for: {message}"
        );
    }

    #[test]
    fn test_resolve_route_refuses_an_unregistered_route_only_when_selected() {
        let modules = [module("cli.git.status", &["git", "status"], true)];
        let manifest = manifest(vec![
            route("cli.git.stash", &["git", "stash"]),
            route("cli.git.status", &["git", "status"]),
        ]);

        assert!(
            resolve_route("git", &["status".to_string()], &manifest, &modules).is_ok(),
            "a route whose module is gone must not refuse a sibling"
        );

        let error = resolve_route("git", &["stash".to_string()], &manifest, &modules)
            .expect_err("selecting a route without a binding must refuse");
        assert!(
            format!("{error:#}").contains("cli.git.stash"),
            "refusal must name the missing module: {error:#}"
        );
    }
}
