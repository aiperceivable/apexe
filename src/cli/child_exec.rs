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
use crate::config::{ApexeConfig, ChildExecMode};
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
/// auditing — the audit records the original and the inserted `--`.
#[allow(clippy::too_many_arguments)] // The call sites all own these facts; bundling them would hide them.
pub(super) async fn analyze(
    config: &ApexeConfig,
    parent: &ScannedModule,
    parent_input: &Value,
    exec_lists: &[ExecList],
    replay: &mut Vec<String>,
    modules: &[ScannedModule],
    context: &Context<Value>,
) -> anyhow::Result<()> {
    if config.child_exec.mode == ChildExecMode::Refuse {
        return Err(refusal());
    }
    // `find` resolves a bare program through PATH at execution time. Strip the
    // shim directories so that name resolves to the real binary this analysis
    // resolved, not to a second shim invocation per match.
    if let Some(path) = std::env::var_os("PATH") {
        std::env::set_var("PATH", crate::runtime_path::clean_path(&path));
    }
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
        let resolved = which::which(program).map_err(|error| {
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
    Ok(())
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
