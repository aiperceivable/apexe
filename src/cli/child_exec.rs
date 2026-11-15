//! Policy for commands that embed another program's argv (`find -exec rm {} ;`).
//!
//! A child list is not this command's grammar. The safe default is to refuse
//! it, and the only way to admit one is to resolve the child program to its own
//! apexe module and run that module's real preflight — ACL, schema, path and
//! approval — for the same caller. The parent call's gates still apply, so the
//! decision is their intersection.
//!
//! What this cannot do is stated rather than approximated: a child program that
//! computes its own paths (`sh -c`, `sed 'w /etc/x'`, `python -c`) is invisible
//! to argv analysis. Those programs must be refused (or covered by a future
//! confinement backend), never admitted on the parent approval alone.

use apcore::Context;
use apcore_toolkit::ScannedModule;
use serde_json::Value;

use crate::adapter::argv::ExecList;
use crate::cli::shim::Manifest;
use crate::config::{ApexeConfig, ChildExecMode};
use crate::governance::delegation::DelegationHandle;
use crate::module::registry;
use crate::module::{CliModule, ExecutionIo};

/// Whether the mode admits an embedded command list at all.
///
/// Kept separate from [`analyze`] so the shim and `exec` refuse before building
/// any executor, and so the refusal is the same sentence on both surfaces.
pub(super) fn refusal() -> anyhow::Error {
    anyhow::anyhow!(
        "This command embeds another program's argv (`-exec`), which is refused by \
         `child_exec.mode: refuse` (the default). Set `child_exec.mode: analyze` to have \
         the child command resolved and checked against its own module, or use a \
         confinement backend"
    )
}

/// Analyze every embedded command list and, where the child module supports it,
/// insert `--` before the first path placeholder.
///
/// The caller's argv is mutated only for that standard separator insertion; the
/// program token is left as typed, because the pinned `PATH` makes the executed
/// binary the one the analysis resolved. Nothing here rewrites the command for
/// auditing; the caller's argv carries the insertion and the dry-run reports it.
#[allow(clippy::too_many_arguments)] // The call sites all own these facts; bundling them would hide them.
pub(super) async fn analyze(
    config: &ApexeConfig,
    parent: &ScannedModule,
    parent_input: &Value,
    exec_lists: &[ExecList],
    replay: &mut Vec<String>,
    modules: &[ScannedModule],
    manifest: &Manifest,
    context: &Context<Value>,
) -> anyhow::Result<Option<DelegationHandle>> {
    let delegate = match config.child_exec.mode {
        ChildExecMode::Refuse => return Err(refusal()),
        ChildExecMode::Analyze => false,
        ChildExecMode::Delegate => true,
    };
    // `find` resolves a bare program through PATH at execution time. In
    // analyze mode the shim directories are stripped, so the name resolves to
    // the real binary that was checked. In delegate mode the shim directory is
    // first, so each match re-enters the installed child shim and its approval
    // is delegated by the grant below. The identity check below always uses the
    // stripped path, because the binding names the real binary.
    let clean_path = crate::runtime_path::clean_path(&std::env::var_os("PATH").unwrap_or_default());
    let execution_path = if delegate {
        let shims = crate::runtime_path::default_shim_dir();
        let mut directories = vec![shims];
        directories.extend(std::env::split_paths(&clean_path));
        std::env::join_paths(directories).unwrap_or_else(|_| clean_path.clone())
    } else {
        clean_path.clone()
    };
    std::env::set_var("PATH", execution_path);
    let acl = crate::cli::local::acl_path(None, config)?;
    let options = crate::cli::local::options(
        config,
        config.bindings_dir.as_path(),
        config.bindings_pattern.as_deref(),
        acl.as_deref(),
        crate::module::ModuleFilter::default(),
    );
    let parent_roots = path_values(parent_input, &parent.input_schema);
    let mut insertions: Vec<usize> = Vec::new();
    let mut child_modules: Vec<String> = Vec::new();

    for list in exec_lists {
        let Some(program) = list.tokens.first() else {
            return Err(anyhow::anyhow!("Empty embedded command list"));
        };
        let program = program.as_str();
        if program == "{}" || program == ";" || program == "+" {
            return Err(anyhow::anyhow!(
                "Embedded command program '{program}' is not analyzable"
            ));
        }
        if program.contains('/') {
            return Err(anyhow::anyhow!(
                "Embedded command program '{program}' is a path; use the command name so it \
                 resolves through the reviewed route and the pinned PATH"
            ));
        }
        let child_args = &list.tokens[..list.tokens.len().saturating_sub(1)];
        let Some(child) = root_module(program, modules) else {
            if config
                .child_exec
                .trust_uncovered
                .iter()
                .any(|trusted| trusted == program)
            {
                tracing::warn!(
                    parent = %parent.module_id,
                    program,
                    "embedded command admitted by trust_uncovered; its arguments were not analyzed"
                );
                continue;
            }
            return Err(anyhow::anyhow!(
                "Embedded command '{program}' cannot be analyzed: no reviewed binding for it"
            ));
        };
        // The binary the analysis checks must be the binary `find` will run.
        let cwd = std::env::current_dir()?;
        let resolved =
            which::which_in(program, Some(clean_path.clone()), &cwd).map_err(|error| {
                anyhow::anyhow!("Embedded command '{program}' cannot be resolved on PATH: {error}")
            })?;
        let target = crate::scanner::resolver::extract_binary_path(&child.target)
            .ok_or_else(|| anyhow::anyhow!("Invalid CLI target for '{}'", child.module_id))?;
        anyhow::ensure!(
            resolved.canonicalize()? == std::path::Path::new(&target).canonicalize()?,
            "Embedded command '{program}' resolves to {}, but its reviewed binding is {}; \
             refusing to analyze one binary while another runs",
            resolved.display(),
            target
        );
        if delegate {
            let shim = crate::runtime_path::default_shim_dir().join(program);
            anyhow::ensure!(
                shim.is_file() && manifest.commands.contains_key(program),
                "Embedded command '{program}' is not installed as a shim; delegate mode runs \
                 each match through the installed shim, so install it with `apexe env --enable` \
                 (add `--also {program}` for a command the default policy leaves out) or use \
                 `child_exec.mode: analyze`"
            );
        }
        child_modules.push(child.module_id.clone());
        let child_argv = crate::adapter::argv::resolve_command_argv(
            child_args,
            &child.input_schema,
            &[],
            Some(&child.module_id),
        )
        .map_err(|error| {
            anyhow::anyhow!(
                "Embedded command '{program}' ({}) cannot be parsed: {}",
                child.module_id,
                error.message
            )
        })?;
        if !child_argv.exec_lists.is_empty() {
            return Err(anyhow::anyhow!(
                "Embedded command '{program}' itself embeds another command; nested embedded \
                 commands are refused until the analysis is applied recursively"
            ));
        }
        if child
            .annotations
            .as_ref()
            .and_then(|annotations| {
                annotations
                    .extra
                    .get(crate::adapter::annotations::COMPUTES_PATHS_KEY)
            })
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return Err(anyhow::anyhow!(
                "Embedded command '{program}' ({}) computes its own paths; argument analysis cannot \
                 see every write it may perform. Use a confinement backend, or remove it from \
                 this call",
                child.module_id
            ));
        }
        let child_input = child_argv.input;
        if let Some(values) = child_input.as_object() {
            crate::module::executor::reject_exec_parameters(values, Some(&child.input_schema))
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Embedded command '{program}' ({}) refused: {}",
                        child.module_id,
                        error.message
                    )
                })?;
        }
        let child_module = CliModule::from_scanned(child, 0)?;
        let child_mode = child_module.access_mode(&child_input);
        let has_placeholder = list.tokens.iter().any(|token| token == "{}");
        if has_placeholder {
            if parent_roots.is_empty() {
                return Err(anyhow::anyhow!(
                    "Embedded command '{program}' uses `{{}}`, but this call has no path operand \
                     whose value it would stand for"
                ));
            }
            for root in &parent_roots {
                crate::governance::path_guard::active().check(
                    "embedded command starting path",
                    root,
                    child_mode,
                )?;
            }
            if child.input_schema["x-apexe-end-of-options"] != true {
                return Err(anyhow::anyhow!(
                    "Embedded command '{program}' uses `{{}}` but its overlay does not declare \
                     `--`; a matched filename that looks like an option cannot be made safe, so \
                     the call is refused"
                ));
            }
            if let Some(position) = child_args.iter().position(|token| token == "{}") {
                let already_separated = position > 0 && child_args[position - 1] == "--";
                if !already_separated {
                    insertions.push(list.argv_index + position);
                }
            }
        }

        // `validate` covers the ACL and schema; the path guard runs while the
        // argv is rendered, so render it here and refuse on its verdict too.
        if let Err(error) = child_module.planned_argv(&child_input) {
            return Err(anyhow::anyhow!(
                "Embedded command '{program}' ({}) refused: {}",
                child.module_id,
                error.message
            ));
        }
        let io = ExecutionIo::Captured;
        let executor_module = CliModule::from_scanned(child, 0)?
            .with_execution_io(io)
            .with_child_exec_analyzed();
        let executor = registry::build_executor_for_invocation(&options, child, executor_module)?;
        let preflight = executor
            .validate(&child.module_id, &child_input, Some(context))
            .await?;
        if !preflight.valid {
            let reason = preflight
                .checks
                .iter()
                .find(|check| !check.passed)
                .and_then(|check| check.error.as_ref())
                .and_then(|error| error["message"].as_str())
                .unwrap_or("the child call was refused");
            return Err(anyhow::anyhow!(
                "Embedded command '{program}' ({}) refused: {reason}",
                child.module_id
            ));
        }
        tracing::info!(
            parent = %parent.module_id,
            child = %child.module_id,
            program,
            requires_approval = preflight.requires_approval,
            "embedded command analyzed"
        );
    }

    for index in insertions.into_iter().rev() {
        replay.insert(index, "--".to_string());
    }
    if delegate {
        child_modules.sort();
        child_modules.dedup();
        let handle = crate::governance::delegation::create(
            config,
            &parent.module_id,
            &context.trace_id,
            &child_modules,
            &parent_roots,
        )?;
        tracing::info!(
            parent = %parent.module_id,
            children = ?child_modules,
            grant = handle.id(),
            "delegated approval created for embedded commands"
        );
        return Ok(Some(handle));
    }
    Ok(None)
}

/// The reviewed root module for a bare program name, if one is loaded.
///
/// v1 deliberately supports only the tool's own invocation (`rm`, not
/// `git status`): a subcommand route would need the same prefix matching the
/// shim does, and refusing it is safer than guessing which route the child
/// command would select.
fn root_module<'a>(program: &str, modules: &'a [ScannedModule]) -> Option<&'a ScannedModule> {
    modules.iter().find(|module| {
        if !crate::models::route_is_reviewed(&module.metadata) {
            return false;
        }
        let path = module
            .metadata
            .get("command_path")
            .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
            .unwrap_or_default();
        path.len() == 1 && path[0] == program
    })
}

/// Whether every path this child call names is under one of the grant's roots.
///
/// The grant is a scope, not an allow-list: the child shim still runs the path
/// guard, and this check only answers "is this still the tree the parent was
/// approved for?".
pub(super) fn inputs_within(
    grant: &crate::governance::delegation::Delegation,
    input: &Value,
    schema: &Value,
) -> bool {
    let (Some(properties), Some(values)) = (schema["properties"].as_object(), input.as_object())
    else {
        return true;
    };
    for (name, property) in properties {
        if property["x-apexe-path"] != true && property["items"]["x-apexe-path"] != true {
            continue;
        }
        let Some(value) = values.get(name) else {
            continue;
        };
        let paths: Vec<&str> = match value {
            Value::String(path) => vec![path.as_str()],
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        if paths
            .iter()
            .any(|path| !crate::governance::delegation::path_within(grant, path))
        {
            return false;
        }
    }
    true
}

/// The parent call's path values, which are what `{}` can stand for.
fn path_values(input: &Value, schema: &Value) -> Vec<String> {
    let (Some(properties), Some(values)) = (schema["properties"].as_object(), input.as_object())
    else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for (name, property) in properties {
        if property["x-apexe-path"] != true && property["items"]["x-apexe-path"] != true {
            continue;
        }
        let Some(value) = values.get(name) else {
            continue;
        };
        match value {
            Value::String(path) => paths.push(path.clone()),
            Value::Array(items) => {
                paths.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            _ => {}
        }
    }
    paths
}
