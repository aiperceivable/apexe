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

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use apcore::Context;
use apcore_toolkit::ScannedModule;
use serde_json::{json, Value};

use crate::adapter::argv::{ExecList, ResolvedArgv};
use crate::cli::shim::Manifest;
use crate::config::{ApexeConfig, ChildExecMode};
use crate::governance::delegation::{ChildScope, DelegationPlan};
use crate::module::registry;
use crate::module::{CliModule, ExecutionIo};

/// Whether the mode admits an embedded command list at all.
///
/// Kept separate from [`analyze`] so the shim and `exec` refuse before building
/// any executor, and so the refusal is the same sentence on both surfaces.
pub(super) fn refusal() -> anyhow::Error {
    anyhow::anyhow!(
        "This command embeds another program's argv (`-exec`), which is refused by \
         `child_exec.mode: refuse` (the default). Set `child_exec.mode: analyze` (or `delegate` \
         to run each match through the child shim) to have the child command resolved and \
         checked against its own module, or use a confinement backend"
    )
}

pub(super) struct AnalyzedChildren {
    pub(super) prepared: crate::module::cli_module::PreparedChildren,
    pub(super) approval_scope: Vec<String>,
}

/// Apply a call-scoped child approval requirement to this parent's executor.
/// An unrelated flag ceiling on the parent cannot stand down this requirement.
pub(super) fn require_child_approval(parent: &mut ScannedModule, scope: &[String]) {
    if scope.is_empty() {
        return;
    }
    let annotations = parent.annotations.get_or_insert_with(Default::default);
    annotations.requires_approval = true;
    annotations
        .extra
        .remove(crate::adapter::annotations::APPROVAL_BASIS_KEY);
    parent.description.push_str(&format!(
        "\nApproval also covers embedded modules: {}.",
        scope.join(", ")
    ));
}

/// Analyze every embedded command list and, where the child module supports it,
/// insert `--` before the child's first positional operand.
///
/// The caller's argv is mutated only for that standard separator insertion; the
/// program token is left as typed, because the pinned `PATH` makes the executed
/// binary the one the analysis resolved. Nothing here rewrites the command for
/// auditing; the caller's argv carries the insertion and the dry-run reports it.
#[allow(clippy::too_many_arguments)] // The call sites own each fact; bundling them would hide the policy inputs.
pub(super) async fn analyze(
    config: &ApexeConfig,
    parent: &ScannedModule,
    parent_input: &Value,
    exec_lists: &[ExecList],
    replay: &mut Vec<String>,
    modules: &[ScannedModule],
    manifest: &Manifest,
    shim_dir: &Path,
    acl: Option<&Path>,
    context: &Context<Value>,
) -> anyhow::Result<AnalyzedChildren> {
    let analysis = Analysis::new(
        config,
        parent,
        parent_input,
        modules,
        manifest,
        shim_dir,
        acl,
        context,
    )?;
    let mut insertions = Vec::new();
    let mut children = Vec::new();
    let mut policy_digest = None;
    let mut approval_scope = Vec::new();
    for list in exec_lists {
        match analysis.analyze_list(list, &mut insertions).await {
            Ok(Some((child, requires_approval, digest))) => {
                if !children.is_empty() {
                    anyhow::ensure!(
                        policy_digest == digest,
                        "ACL changed during embedded command analysis; retry"
                    );
                }
                policy_digest = digest;
                if requires_approval {
                    approval_scope.push(child.module_id.clone());
                }
                children.push(child);
            }
            Ok(None) => {}
            Err(error) => {
                analysis.refuse(list, "analysis_refused");
                return Err(error);
            }
        }
    }
    for index in insertions.into_iter().rev() {
        replay.insert(index, "--".to_string());
    }
    approval_scope.sort();
    approval_scope.dedup();
    Ok(AnalyzedChildren {
        prepared: crate::module::cli_module::PreparedChildren {
            path: pinned_path(analysis.shim_dir.as_deref(), &analysis.clean_path)?,
            delegation: analysis.finish(children, policy_digest)?,
        },
        approval_scope,
    })
}

/// One resolved child command: its module, parsed argv and raw child operands.
struct Child<'a> {
    program: String,
    module: &'a ScannedModule,
    argv: ResolvedArgv,
    /// Tokens after the program and before the terminator.
    operands: Vec<String>,
}

/// The immutable facts and the audit sink one parent call shares across lists.
struct Analysis<'a> {
    config: &'a ApexeConfig,
    parent: &'a ScannedModule,
    modules: &'a [ScannedModule],
    manifest: &'a Manifest,
    shim_dir: Option<PathBuf>,
    acl: Option<&'a Path>,
    context: &'a Context<Value>,
    parent_roots: Vec<String>,
    /// `PATH` without shim directories, used to resolve the real binary.
    clean_path: OsString,
    audit: crate::governance::AuditManager,
}

impl<'a> Analysis<'a> {
    #[allow(clippy::too_many_arguments)] // Resolved policy is supplied by the parent surface.
    fn new(
        config: &'a ApexeConfig,
        parent: &'a ScannedModule,
        parent_input: &'a Value,
        modules: &'a [ScannedModule],
        manifest: &'a Manifest,
        shim_dir: &Path,
        acl: Option<&'a Path>,
        context: &'a Context<Value>,
    ) -> anyhow::Result<Self> {
        let audit = crate::governance::AuditManager::new(&config.audit_log);
        let delegate = match config.child_exec.mode {
            ChildExecMode::Refuse => {
                audit.log_governance_event(
                    "child_exec",
                    json!({
                        "parent_module": parent.module_id,
                        "parent_trace": context.trace_id,
                        "decision": "refused",
                        "reason": "mode_refuse",
                    }),
                );
                return Err(refusal());
            }
            ChildExecMode::Analyze => false,
            ChildExecMode::Delegate => true,
        };
        // `find` resolves a bare program through PATH at execution time. In
        // analyze mode the shim directories are stripped, so the name resolves
        // to the real binary that was checked. In delegate mode the shim
        // directory is first, so each match re-enters the installed child shim
        // and the per-match scope is activated only when the parent executes.
        let clean_path =
            crate::runtime_path::clean_path(&std::env::var_os("PATH").unwrap_or_default());
        let shim_dir = delegate
            .then(|| std::path::absolute(shim_dir))
            .transpose()?;
        Ok(Self {
            config,
            parent,
            modules,
            manifest,
            shim_dir,
            acl,
            context,
            parent_roots: path_values(parent_input, &parent.input_schema),
            clean_path,
            audit,
        })
    }

    /// Record a refused list. The reason is a stable code; the caller-facing
    /// error carries the detail, and the parent refusal row carries the same
    /// trace so the two can be joined.
    fn refuse(&self, list: &ExecList, reason: &str) {
        self.audit.log_governance_event(
            "child_exec",
            json!({
                "parent_module": self.parent.module_id,
                "parent_trace": self.context.trace_id,
                "program": list.tokens.first(),
                "decision": "refused",
                "reason": reason,
            }),
        );
    }

    async fn analyze_list(
        &self,
        list: &ExecList,
        insertions: &mut Vec<usize>,
    ) -> anyhow::Result<Option<(ChildScope, bool, Option<String>)>> {
        let Some(child) = self.resolve_child(list)? else {
            return Ok(None);
        };
        self.check_child(list, &child, insertions)?;
        let (requires_approval, policy_digest) = self.preflight_child(&child).await?;
        let batched = list
            .tokens
            .last()
            .is_some_and(|terminator| terminator == "+");
        let scope = ChildScope::new(child.module, child.argv.input, batched, requires_approval);
        Ok(Some((scope, requires_approval, policy_digest)))
    }

    /// Resolve the program, check the executed binary, and parse the child argv.
    ///
    /// Returns `None` for a `trust_uncovered` program: the operator vouched for
    /// it, so there is nothing to parse, and the parent approval is the only
    /// remaining gate. That use is audited as `trusted_unanalyzed`.
    fn resolve_child<'b>(&self, list: &'b ExecList) -> anyhow::Result<Option<Child<'a>>> {
        let Some((module, program)) = self.resolve_program(list)? else {
            return Ok(None);
        };
        self.parse_child(list, module, program).map(Some)
    }

    /// Find the reviewed module and verify the binary `find` will execute.
    fn resolve_program<'b>(
        &self,
        list: &'b ExecList,
    ) -> anyhow::Result<Option<(&'a ScannedModule, &'b str)>> {
        let program = program_name(list)?;
        let Some(module) = root_module(program, self.modules) else {
            if self
                .config
                .child_exec
                .trust_uncovered
                .iter()
                .any(|trusted| trusted == program)
            {
                self.audit.log_governance_event(
                    "child_exec",
                    json!({
                        "parent_module": self.parent.module_id,
                        "parent_trace": self.context.trace_id,
                        "program": program,
                        "decision": "trusted_unanalyzed",
                    }),
                );
                return Ok(None);
            }
            return Err(anyhow::anyhow!(
                "Embedded command '{program}' cannot be analyzed: no reviewed binding for it"
            ));
        };
        self.check_binary(program, module)?;
        if let Some(directory) = &self.shim_dir {
            let shim = directory.join(program);
            let installed = crate::cli::shim::read_manifest(directory)?;
            anyhow::ensure!(
                shim.is_file()
                    && installed.bindings_dir.canonicalize()?
                        == self.manifest.bindings_dir.canonicalize()?
                    && installed
                        .commands
                        .get(program)
                        .is_some_and(|routes| routes.iter().any(|route| {
                            route.module_id == module.module_id
                                && route.tier == crate::cli::shim::RouteTier::Verified
                                && !route.tombstone
                        })),
                "Embedded command '{program}' is not installed as a shim; delegate mode runs \
                 each match through the installed shim, so install it with `apexe env --enable` \
                 (add `--also {program}` for a command the default policy leaves out) or use \
                 `child_exec.mode: analyze`"
            );
        }
        Ok(Some((module, program)))
    }

    /// Parse the child argv after the program and apply the child-only gates.
    fn parse_child<'b>(
        &self,
        list: &'b ExecList,
        module: &'a ScannedModule,
        program: &str,
    ) -> anyhow::Result<Child<'a>> {
        let operands = &list.tokens[1..list.tokens.len().saturating_sub(1)];
        let argv = parse_child_argv(operands, module, program)?;
        reject_computes_paths(module, program)?;
        reject_child_parameters(&argv.input, module, program)?;
        Ok(Child {
            program: program.to_string(),
            module,
            argv,
            operands: operands.to_vec(),
        })
    }

    /// The binary the analysis checks must be the binary the parent will run.
    fn check_binary(&self, program: &str, module: &ScannedModule) -> anyhow::Result<()> {
        let cwd = std::env::current_dir()?;
        let resolved =
            which::which_in(program, Some(self.clean_path.clone()), &cwd).map_err(|error| {
                anyhow::anyhow!("Embedded command '{program}' cannot be resolved on PATH: {error}")
            })?;
        let target = crate::scanner::resolver::extract_binary_path(&module.target)
            .ok_or_else(|| anyhow::anyhow!("Invalid CLI target for '{}'", module.module_id))?;
        anyhow::ensure!(
            resolved.canonicalize()? == std::path::Path::new(&target).canonicalize()?,
            "Embedded command '{program}' resolves to {}, but its reviewed binding is {}; \
             refusing to analyze one binary while another runs",
            resolved.display(),
            target
        );
        Ok(())
    }

    /// Check `{}` against the parent's tree and place `--` before the first
    /// positional operand when the child declares it.
    ///
    /// `--` is inserted before the operand list, not before the `{}`: the
    /// placeholder may be a later operand or a flag value, and inserting
    /// between operands would make `--` an argument (`printf '%s' -- file`).
    fn check_child(
        &self,
        list: &ExecList,
        child: &Child<'a>,
        insertions: &mut Vec<usize>,
    ) -> anyhow::Result<()> {
        let module = CliModule::from_scanned(child.module, 0)?;
        let mode = module.access_mode(&child.argv.input);
        if path_placeholder(&child.argv.input, &child.module.input_schema) {
            if self.parent_roots.is_empty() {
                return Err(anyhow::anyhow!(
                    "Embedded command '{}' uses `{{}}`, but this call has no path operand whose \
                     value it would stand for",
                    child.program
                ));
            }
            for root in &self.parent_roots {
                crate::governance::path_guard::active().check(
                    "embedded command starting path",
                    root,
                    mode,
                )?;
            }
        }
        if positional_placeholder(&child.argv.input, &child.module.input_schema) {
            if child.module.input_schema["x-apexe-end-of-options"] != true {
                return Err(anyhow::anyhow!(
                    "Embedded command '{}' uses `{{}}` but its overlay does not declare `--`; a \
                     matched filename that looks like an option cannot be made safe, so the call \
                     is refused",
                    child.program
                ));
            }
            if let Some(first) = child.argv.operand_indices.first().copied() {
                let already_separated = child.operands[..first].iter().any(|token| token == "--");
                if !already_separated {
                    insertions.push(list.argv_index + 1 + first);
                }
            }
        }
        Ok(())
    }

    /// Run the child module's real preflight for the same caller.
    async fn preflight_child(&self, child: &Child<'a>) -> anyhow::Result<(bool, Option<String>)> {
        let input = &child.argv.input;
        let module = CliModule::from_scanned(child.module, 0)?;
        if let Err(error) = module.planned_argv(input) {
            return Err(anyhow::anyhow!(
                "Embedded command '{}' ({}) refused: {}",
                child.program,
                child.module.module_id,
                error.message
            ));
        }
        let (executor, acl) = self.child_executor(child)?;
        let preflight = executor
            .validate(&child.module.module_id, input, Some(self.context))
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
                "Embedded command '{}' ({}) refused: {reason}",
                child.program,
                child.module.module_id
            ));
        }
        // The SDK merges ACL and module approval into one boolean. Apply
        // the same conservative rule as the regular gate: if an ACL may be
        // the source, only a human decision can satisfy the requirement.
        let annotations = apcore::Module::annotations(&module);
        let requires_approval = preflight.requires_approval
            && (acl.requires_approval
                || !annotations.requires_approval
                || crate::module::approval::needs_prompt_for_input(&annotations, input));
        self.audit.log_governance_event(
            "child_exec",
            json!({
                "parent_module": self.parent.module_id,
                "parent_trace": self.context.trace_id,
                "program": child.program,
                "child_module": child.module.module_id,
                "decision": "analyzed",
                "requires_approval": requires_approval,
            }),
        );
        Ok((requires_approval, acl.policy_digest))
    }

    /// Prepare a per-match scope. Dry-run must never activate it.
    fn finish(
        &self,
        children: Vec<ChildScope>,
        policy_digest: Option<String>,
    ) -> anyhow::Result<Option<DelegationPlan>> {
        let Some(shim_dir) = self.shim_dir.as_ref().filter(|_| !children.is_empty()) else {
            return Ok(None);
        };
        Ok(Some(DelegationPlan {
            config_dir: self.config.config_dir.clone(),
            children,
            roots: self.parent_roots.clone(),
            shim_dir: shim_dir.canonicalize()?,
            acl: self.acl.map(Path::canonicalize).transpose()?,
            policy_digest,
            caller: self.context.caller_id.clone(),
            human_approved: Default::default(),
        }))
    }
}

impl<'a> Analysis<'a> {
    /// Build a one-module executor for the child's real preflight.
    fn child_executor(
        &self,
        child: &Child<'a>,
    ) -> anyhow::Result<(std::sync::Arc<apcore::Executor>, registry::AclSummary)> {
        let options = crate::cli::local::options(
            self.config,
            &self.manifest.bindings_dir,
            self.manifest.bindings_pattern.as_deref(),
            self.acl,
            self.manifest.filter.clone(),
        );
        let module = CliModule::from_scanned(child.module, 0)?
            .with_execution_io(ExecutionIo::Captured)
            .with_child_exec_analyzed();
        Ok(registry::build_invocation_with_acl_summary(
            &options,
            child.module,
            module,
        )?)
    }
}

/// The PATH the parent gets: shim-first in delegate, shim-free otherwise.
fn pinned_path(shim_dir: Option<&Path>, clean_path: &std::ffi::OsStr) -> anyhow::Result<OsString> {
    Ok(match shim_dir {
        Some(directory) => std::env::join_paths(
            std::iter::once(directory.to_path_buf()).chain(std::env::split_paths(clean_path)),
        )?,
        None => clean_path.to_os_string(),
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

/// The program token of a list, with the non-analyzable forms refused.
fn program_name(list: &ExecList) -> anyhow::Result<&str> {
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
    Ok(program)
}

/// Parse the child's own argv, refusing a nested command list.
fn parse_child_argv(
    operands: &[String],
    module: &ScannedModule,
    program: &str,
) -> anyhow::Result<ResolvedArgv> {
    let argv = crate::adapter::argv::resolve_command_argv(
        operands,
        &module.input_schema,
        &[],
        Some(&module.module_id),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "Embedded command '{program}' ({}) cannot be parsed: {}",
            module.module_id,
            error.message
        )
    })?;
    if !argv.exec_lists.is_empty() {
        return Err(anyhow::anyhow!(
            "Embedded command '{program}' itself embeds another command; nested embedded \
             commands are refused until the analysis is applied recursively"
        ));
    }
    Ok(argv)
}

/// Refuse a child whose paths are computed inside its own logic.
fn reject_computes_paths(module: &ScannedModule, program: &str) -> anyhow::Result<()> {
    if module
        .annotations
        .as_ref()
        .and_then(|annotations| {
            annotations
                .extra
                .get(crate::adapter::annotations::COMPUTES_PATHS_KEY)
        })
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Err(anyhow::anyhow!(
            "Embedded command '{program}' ({}) computes its own paths; argument analysis \
             cannot see every write it may perform. Use a confinement backend, or remove it \
             from this call",
            module.module_id
        ));
    }
    Ok(())
}

/// Refuse a child parameter the wrapped tool would execute.
fn reject_child_parameters(
    input: &Value,
    module: &ScannedModule,
    program: &str,
) -> anyhow::Result<()> {
    if let Some(values) = input.as_object() {
        crate::module::executor::reject_exec_parameters(values, Some(&module.input_schema))
            .map_err(|error| {
                anyhow::anyhow!(
                    "Embedded command '{program}' ({}) refused: {}",
                    module.module_id,
                    error.message
                )
            })?;
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

/// Whether a positional operand contains the `{}` placeholder.
fn positional_placeholder(input: &Value, schema: &Value) -> bool {
    has_placeholder(input, schema, |property| {
        property["x-apexe-positional"].as_u64().is_some()
    })
}

/// Whether a path-typed value contains the `{}` placeholder.
fn path_placeholder(input: &Value, schema: &Value) -> bool {
    has_placeholder(input, schema, |property| {
        property["x-apexe-path"] == true || property["items"]["x-apexe-path"] == true
    })
}

fn has_placeholder(input: &Value, schema: &Value, matches: impl Fn(&Value) -> bool) -> bool {
    let (Some(properties), Some(values)) = (schema["properties"].as_object(), input.as_object())
    else {
        return false;
    };
    properties.iter().any(|(name, property)| {
        if !matches(property) {
            return false;
        }
        let Some(value) = values.get(name) else {
            return false;
        };
        match value {
            Value::String(text) => text == "{}",
            Value::Array(items) => items.iter().any(|item| item == "{}"),
            _ => false,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_placeholder_checks_distinguish_flag_values_from_positionals() {
        let schema = json!({
            "type": "object",
            "properties": {
                "patterns": {"type": "string", "x-apexe-flag": "-e"},
                "files": {
                    "type": "array",
                    "items": {"type": "string", "x-apexe-path": true},
                    "x-apexe-positional": 0
                }
            }
        });
        let flag_value = json!({"patterns": "{}"});
        assert!(
            !positional_placeholder(&flag_value, &schema),
            "a `{{}}` in a flag value is not a positional operand"
        );
        assert!(!path_placeholder(&flag_value, &schema));

        let positional = json!({"files": ["{}"]});
        assert!(positional_placeholder(&positional, &schema));
        assert!(path_placeholder(&positional, &schema));
    }

    #[test]
    fn test_path_placeholder_ignores_non_path_operands() {
        let schema = json!({
            "type": "object",
            "properties": {
                "format": {"type": "string", "x-apexe-positional": 0}
            }
        });
        let input = json!({"format": "{}"});
        assert!(positional_placeholder(&input, &schema));
        assert!(
            !path_placeholder(&input, &schema),
            "a format placeholder is not a filesystem path"
        );
    }
}
