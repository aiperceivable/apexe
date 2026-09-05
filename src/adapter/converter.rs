use std::collections::HashMap;

use apcore::module::{ModuleAnnotations, ModuleExample};
use apcore_toolkit::{deduplicate_ids, DisplayResolver, ScannedModule};
use serde_json::json;
use tracing::warn;

use crate::models::{HelpFormat, ScannedCLITool, ScannedCommand};

use super::{annotations, schema};

/// Converts ScannedCLITool instances into ScannedModule instances.
pub struct CliToolConverter {
    namespace: String,
}

/// Fields extracted from a (real or synthesized) command, used to assemble a module.
struct CommandFields {
    description: String,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
    annotations: ModuleAnnotations,
    documentation: Option<String>,
    examples: Vec<String>,
}

impl CliToolConverter {
    /// Create a new converter with the default "cli" namespace.
    pub fn new() -> Self {
        Self {
            namespace: "cli".to_string(),
        }
    }

    /// Create a converter with a custom namespace prefix.
    pub fn with_namespace(namespace: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
        }
    }

    /// Convert a single ScannedCLITool into a list of ScannedModules (one per leaf command).
    ///
    /// Applies [`DisplayResolver`] to populate `metadata["display"]` on each module.
    pub fn convert(&self, tool: &ScannedCLITool) -> Vec<ScannedModule> {
        let modules = self.build_modules(tool);
        // Spec §5: disambiguate colliding module_ids after flattening (e.g. a
        // tool exposing an aliased subcommand twice) before anything keys on them.
        let modules = deduplicate_ids(modules);
        Self::apply_display_resolver(modules)
    }

    /// Build ScannedModules from a ScannedCLITool without applying DisplayResolver.
    fn build_modules(&self, tool: &ScannedCLITool) -> Vec<ScannedModule> {
        let mut leaves: Vec<(Vec<String>, Option<&ScannedCommand>)> = Vec::new();

        if tool.subcommands.is_empty() {
            leaves.push((vec![], None));
        } else {
            self.collect_leaves(&tool.subcommands, &mut vec![], &mut leaves);
        }

        leaves
            .iter()
            .map(|(path, command_opt)| self.build_single_module(tool, path, command_opt.as_ref()))
            .collect()
    }

    /// The `exec://` target a module is invoked through.
    ///
    /// For a root-only tool this is just the binary path; for a subcommand the
    /// subcommand path is appended ("exec:///usr/bin/git commit").
    /// `fields.full_command` cannot be used here: it is the *full* invocation
    /// ("git commit"), so prefixing the binary path spelled the tool name twice
    /// and produced `git git commit`.
    fn build_target(tool: &ScannedCLITool, path: &[String]) -> String {
        if path.is_empty() {
            format!("exec://{}", tool.binary_path)
        } else {
            format!("exec://{} {}", tool.binary_path, path.join(" "))
        }
    }

    /// Wrap each parsed invocation as a titled example.
    ///
    /// Spec §3.3 step 10: the command's examples are carried onto the module so
    /// downstream MCP/A2A docs surface them. `command.examples` is a plain
    /// `Vec<String>` invocation list.
    fn build_examples(invocations: Vec<String>) -> Vec<ModuleExample> {
        invocations
            .into_iter()
            .map(|invocation| {
                // ModuleExample is #[non_exhaustive]; build via Default + field set.
                let mut example = ModuleExample::default();
                example.title = invocation;
                example
            })
            .collect()
    }

    /// Build a single ScannedModule from a leaf command (or synthesized root).
    fn build_single_module(
        &self,
        tool: &ScannedCLITool,
        path: &[String],
        command_opt: Option<&&ScannedCommand>,
    ) -> ScannedModule {
        let mut segments = vec![self.namespace.clone(), sanitize_id_segment(&tool.name)];
        segments.extend(path.iter().map(|segment| sanitize_id_segment(segment)));

        let fields = Self::extract_command_fields(tool, command_opt);
        let help_format_name =
            help_format_to_tag(command_opt.map_or(HelpFormat::Unknown, |c| c.help_format));

        let mut module = ScannedModule::new(
            segments.join("."),
            fields.description,
            fields.input_schema,
            fields.output_schema,
            self.build_tags(tool, command_opt, help_format_name),
            Self::build_target(tool, path),
        );
        module.version = tool
            .version
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        module.annotations = Some(fields.annotations);
        module.documentation = fields.documentation;
        module.metadata = self.build_metadata(tool, path, help_format_name);
        module.warnings = tool.warnings.clone();
        module.examples = Self::build_examples(fields.examples);
        module
    }

    /// Build metadata HashMap for a module.
    fn build_metadata(
        &self,
        tool: &ScannedCLITool,
        path: &[String],
        help_format_name: &str,
    ) -> HashMap<String, serde_json::Value> {
        let suggested_alias = if path.is_empty() {
            sanitize_id_segment(&tool.name)
        } else {
            format!(
                "{}_{}",
                sanitize_id_segment(&tool.name),
                path.iter()
                    .map(|segment| sanitize_id_segment(segment))
                    .collect::<Vec<_>>()
                    .join("_")
            )
        };
        let mut metadata = HashMap::new();
        metadata.insert("scan_tier".to_string(), json!(tool.scan_tier));
        metadata.insert("help_format".to_string(), json!(help_format_name));
        metadata.insert("binary_path".to_string(), json!(tool.binary_path));
        metadata.insert("suggested_alias".to_string(), json!(suggested_alias));
        // The module id folds `-` to `_` to satisfy the apcore id grammar, so
        // `git cat-file` is served as `cli.git.cat_file`. The hyphenated form is
        // what argv needs and what a user searches for, and it is not
        // recoverable from the id — an underscore is a legal subcommand
        // character too. Record it rather than lose it.
        let mut command_path = vec![tool.name.clone()];
        command_path.extend(path.iter().cloned());
        metadata.insert("command_path".to_string(), json!(command_path));
        metadata
    }

    /// The description a command contributes, or a synthesized one.
    ///
    /// A command with no help text of its own still needs a summary an agent
    /// can read, so `Execute <command>` is the floor rather than an empty
    /// string.
    fn command_description(candidates: [&str; 2], fallback_name: &str) -> String {
        candidates
            .into_iter()
            .find(|candidate| !candidate.is_empty())
            .map_or_else(|| format!("Execute {fallback_name}"), str::to_string)
    }

    /// Raw help text, or `None` when the command produced none.
    fn command_documentation(raw_help: &str) -> Option<String> {
        (!raw_help.is_empty()).then(|| raw_help.to_string())
    }

    /// Gather the per-command fields a module is assembled from.
    ///
    /// `command_opt` is `None` for a tool's own invocation, where a root command
    /// is synthesized from the tool. The two differ in exactly one thing beyond
    /// their source: a real command is reached only through `collect_leaves`,
    /// which runs only when the tool has subcommands and always pushes a
    /// non-empty path — so it always sits behind at least one subcommand token,
    /// and its global flags have to precede that token. The synthesized root has
    /// no such token, so its global flags are ordinary flags and must keep their
    /// default placement.
    fn extract_command_fields(
        tool: &ScannedCLITool,
        command_opt: Option<&&ScannedCommand>,
    ) -> CommandFields {
        // Borrowed, not cloned: `raw_help` holds a whole help text, and this
        // runs once per module. `synth` outlives the borrow because it is
        // declared here and initialized only on the branch that needs it.
        let synth;
        let (command, position): (&ScannedCommand, _) = match command_opt {
            Some(command) => (command, schema::CommandPosition::Subcommand),
            None => {
                synth = synthesize_root_command(tool);
                (&synth, schema::CommandPosition::Root)
            }
        };

        // A real command speaks only for itself; the synthesized root may fall
        // back to the tool's own description.
        let fallback = match command_opt {
            Some(_) => "",
            None => tool.description.as_str(),
        };
        let fallback_name = match command_opt {
            Some(command) => command.full_command.as_str(),
            None => tool.name.as_str(),
        };

        // Built once and shared: `mark_escalating_params` reads the property
        // names off it, and those must be the names the module ships with.
        let input_schema = schema::build_input_schema(command, &tool.global_flags, position);

        CommandFields {
            description: Self::command_description(
                [command.description.as_str(), fallback],
                fallback_name,
            ),
            input_schema: input_schema.clone(),
            output_schema: schema::build_output_schema(command),
            annotations: apply_annotation_overrides(
                mark_escalating_params(annotations::infer(command), &input_schema),
                tool,
            ),
            documentation: Self::command_documentation(&command.raw_help),
            examples: command.examples.clone(),
        }
    }

    /// Assemble the tags list for a module.
    fn build_tags(
        &self,
        tool: &ScannedCLITool,
        command_opt: Option<&&ScannedCommand>,
        help_format_name: &str,
    ) -> Vec<String> {
        let mut tags = vec![
            "cli".to_string(),
            tool.name.clone(),
            help_format_name.to_string(),
        ];
        if command_opt.is_some_and(|c| c.structured_output.supported)
            || (command_opt.is_none() && tool.structured_output.supported)
        {
            tags.push("structured-output".to_string());
        }
        tags
    }

    /// Apply DisplayResolver to populate `metadata["display"]` on each module.
    ///
    /// On the extremely rare validation failure (alias >64 chars or invalid pattern),
    /// logs a warning and returns the modules without display metadata.
    fn apply_display_resolver(modules: Vec<ScannedModule>) -> Vec<ScannedModule> {
        let resolver = DisplayResolver::new();
        let backup = modules.clone();
        resolver.resolve(modules, None, None).unwrap_or_else(|e| {
            warn!(error = %e, "DisplayResolver failed, skipping display metadata");
            backup
        })
    }

    /// Convert multiple ScannedCLITools into ScannedModules.
    ///
    /// Deduplicates and resolves display metadata over the FLATTENED batch,
    /// not per tool: `convert()`'s own dedup pass only sees one tool's
    /// leaves, so two tools that resolve to the same command (e.g. `apexe
    /// scan ls /bin/ls`, two path arguments naming the same binary) would
    /// otherwise each produce "cli.ls" with nothing to disambiguate them.
    pub fn convert_all(&self, tools: &[ScannedCLITool]) -> Vec<ScannedModule> {
        let modules: Vec<ScannedModule> =
            tools.iter().flat_map(|t| self.build_modules(t)).collect();
        let modules = deduplicate_ids(modules);
        Self::apply_display_resolver(modules)
    }

    /// Recursively collect leaf commands (commands with no subcommands).
    fn collect_leaves<'a>(
        &self,
        commands: &'a [ScannedCommand],
        path: &mut Vec<String>,
        leaves: &mut Vec<(Vec<String>, Option<&'a ScannedCommand>)>,
    ) {
        for cmd in commands {
            path.push(cmd.name.clone());
            if cmd.subcommands.is_empty() {
                leaves.push((path.clone(), Some(cmd)));
            } else {
                self.collect_leaves(&cmd.subcommands, path, leaves);
            }
            path.pop();
        }
    }
}

impl Default for CliToolConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&ScannedCLITool> for Vec<ScannedModule> {
    fn from(tool: &ScannedCLITool) -> Vec<ScannedModule> {
        CliToolConverter::new().convert(tool)
    }
}

/// Synthesize a ScannedCommand from a root-only ScannedCLITool.
fn synthesize_root_command(tool: &ScannedCLITool) -> ScannedCommand {
    ScannedCommand {
        name: tool.name.clone(),
        full_command: tool.name.clone(),
        description: String::new(),
        flags: vec![],
        // Tool-level positional args (`ls [file ...]`) belong to the root
        // invocation, so the synthesized root command is where they surface in
        // the generated input schema.
        positional_args: tool.positional_args.clone(),
        subcommands: vec![],
        // Same reasoning as positional_args: the man page's EXAMPLES describe
        // invoking the tool itself, so they belong to the root command.
        examples: tool.examples.clone(),
        help_format: HelpFormat::Unknown,
        structured_output: tool.structured_output.clone(),
        end_of_options: tool.end_of_options,
        raw_help: String::new(),
    }
}

/// Apply an overlay's behavioral assertions over the inferred annotations.
///
/// Inference reads command names and flag names; an overlay states the answer.
/// Only fields the overlay actually set are replaced.
fn apply_annotation_overrides(
    mut annotations: ModuleAnnotations,
    tool: &ScannedCLITool,
) -> ModuleAnnotations {
    let overrides = &tool.annotation_overrides;
    if let Some(readonly) = overrides.readonly {
        annotations.readonly = readonly;
    }
    if let Some(destructive) = overrides.destructive {
        annotations.destructive = destructive;
    }
    if let Some(idempotent) = overrides.idempotent {
        annotations.idempotent = idempotent;
    }
    if let Some(requires_approval) = overrides.requires_approval {
        annotations.requires_approval = requires_approval;
        // The overlay states the answer, so a flag-derived basis no longer
        // describes why the mark is (or is not) there. Leaving it behind would
        // let the gate stand down on a call a human said to gate.
        annotations.extra.remove(annotations::APPROVAL_BASIS_KEY);
        annotations.extra.remove(annotations::ESCALATING_PARAMS_KEY);
    }
    if let Some(open_world) = overrides.open_world {
        annotations.open_world = open_world;
    }
    annotations.cacheable = annotations.readonly && annotations.idempotent;
    annotations
}

/// Record which schema properties would escalate a call to `requires_approval`.
///
/// `requires_approval` stays a *ceiling* rather than a verdict. apcore decides
/// whether to run the approval gate from this static annotation and never sees
/// a call's arguments — its `ExecutionPolicy` hook resolves from `module_id`
/// and annotations alone — so leaving the flag false for a command that accepts
/// `--force` would remove the gate from `git push --force` outright. Marking it
/// true and naming the properties instead lets
/// [`ApprovalGate`](crate::module::ApprovalGate), which *is* handed the
/// arguments, stand down for the calls that carry none of them.
///
/// A module already marked by [`annotations::infer`] is left alone: it is
/// destructive by name, which no absent flag makes safe. Runs before
/// [`apply_annotation_overrides`] so an overlay keeps the last word.
fn mark_escalating_params(
    mut annotations: ModuleAnnotations,
    input_schema: &serde_json::Value,
) -> ModuleAnnotations {
    if annotations.requires_approval {
        return annotations;
    }
    let escalating = escalating_property_names(input_schema);
    if escalating.is_empty() {
        return annotations;
    }
    annotations.requires_approval = true;
    annotations.extra.insert(
        annotations::APPROVAL_BASIS_KEY.to_string(),
        json!(annotations::APPROVAL_BASIS_FLAGS),
    );
    annotations.extra.insert(
        annotations::ESCALATING_PARAMS_KEY.to_string(),
        json!(escalating),
    );
    annotations
}

/// The property names in `input_schema` that escalate a call.
///
/// Two sources, unioned. The flag-name list ([`annotations::flag_literal_escalates`])
/// is a floor that applies to every tool without anyone writing anything down;
/// `x-apexe-escalates` is an overlay's assertion about one specific flag, and it
/// is the only way to reach the ones no name list can generalize — `docker run
/// --privileged` is not called `--force` anywhere.
///
/// `x-apexe-exec` is deliberately *not* collected. Such a parameter is refused
/// by `executor::reject_exec_parameters` before it can run, so listing it here
/// would prompt a human about a call that cannot proceed either way.
///
/// Read off the built schema rather than `ScannedCommand::flags` because a
/// positional argument can displace a same-named flag onto another key (see
/// `schema::rekey_displaced_flag`), and global flags land here too. The schema
/// is the only place the name a caller will actually send is settled.
///
/// Sorted so the recorded list is stable across runs — it is written into
/// module bindings that get committed and diffed.
fn escalating_property_names(input_schema: &serde_json::Value) -> Vec<String> {
    let Some(properties) = input_schema.get("properties").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = properties
        .iter()
        .filter(|(_, property)| property_escalates(property))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// Whether one built property escalates the call that fills it.
///
/// `x-apexe-escalates` is three-state and outranks the name list in *both*
/// directions. An explicit `false` is a human saying the floor is wrong here,
/// and it has to win: `bsdtar -y` is bzip2 compression, not "assume yes", and a
/// gate that prompts for it spends the operator's attention on a call that
/// never needed it. Only an absent keyword defers to the name list.
fn property_escalates(property: &serde_json::Value) -> bool {
    match property
        .get("x-apexe-escalates")
        .and_then(serde_json::Value::as_bool)
    {
        Some(asserted) => asserted,
        None => property
            .get("x-apexe-flag")
            .and_then(|literal| literal.as_str())
            .is_some_and(annotations::flag_literal_escalates),
    }
}

/// Fold one command name into a segment the apcore module-id grammar accepts.
///
/// apcore requires `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$`, and rejects
/// anything else at *registration* time. Deriving the id straight from the
/// command name therefore produced ids that a scan happily wrote to disk, that
/// `apexe list` counted, and that the server then refused: 63 of git's 142
/// subcommands — every hyphenated one, `cat-file` through `rev-parse` — were
/// dropped with only a warning on stderr. Applying the charset here makes the
/// generated surface and the served surface the same set.
///
/// The mapping is deliberately lossy and one-way: `-` becomes `_`, uppercase
/// folds down, and any remaining out-of-charset byte becomes `_`. The command
/// name as argv needs it is preserved in `metadata["command_path"]`, and
/// collisions introduced by the folding (`cat-file` vs a hypothetical
/// `cat_file`) are resolved downstream by [`deduplicate_ids`].
fn sanitize_id_segment(name: &str) -> String {
    let folded: String = name
        .chars()
        .map(|c| {
            let lowered = c.to_ascii_lowercase();
            if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() {
                lowered
            } else {
                '_'
            }
        })
        .collect();
    // The grammar also requires a leading letter, which a subcommand such as
    // `7z` or one folded down to `_x` would not have.
    if folded.starts_with(|c: char| c.is_ascii_lowercase()) {
        folded
    } else {
        format!("t{folded}")
    }
}

/// Convert a HelpFormat variant to a lowercase tag string.
fn help_format_to_tag(format: HelpFormat) -> &'static str {
    match format {
        HelpFormat::Gnu => "gnu",
        HelpFormat::Click => "click",
        HelpFormat::Argparse => "argparse",
        HelpFormat::Cobra => "cobra",
        HelpFormat::Clap => "clap",
        HelpFormat::Man => "man",
        HelpFormat::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::StructuredOutputInfo;

    fn make_command(name: &str, full_command: &str) -> ScannedCommand {
        ScannedCommand {
            name: name.to_string(),
            full_command: full_command.to_string(),
            description: format!("{name} description"),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            end_of_options: false,
            raw_help: String::new(),
        }
    }

    fn make_tool(name: &str, subcommands: Vec<ScannedCommand>) -> ScannedCLITool {
        ScannedCLITool {
            name: name.to_string(),
            description: String::new(),
            binary_path: format!("/usr/bin/{name}"),
            version: Some("1.0.0".to_string()),
            subcommands,
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
            ..Default::default()
        }
    }

    /// Build a command that accepts `literals`, the way a scan records them.
    fn command_accepting(name: &str, literals: &[&str]) -> ScannedCommand {
        let flags = literals
            .iter()
            .map(|literal| crate::models::ScannedFlag {
                long_name: Some((*literal).to_string()),
                ..Default::default()
            })
            .collect();
        ScannedCommand {
            flags,
            ..make_command(name, name)
        }
    }

    /// The ceiling: a reader that merely *accepts* `--all` is still marked, so
    /// apcore runs the gate at all — but the basis says why, so the gate can
    /// stand down for the calls that send nothing escalating.
    #[test]
    fn test_accepted_approval_flag_marks_the_module_with_a_flag_basis() {
        let tool = make_tool("git", vec![command_accepting("log", &["--all"])]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(annotations.requires_approval, "the ceiling must stay up");
        assert_eq!(
            annotations.extra.get(annotations::APPROVAL_BASIS_KEY),
            Some(&json!(annotations::APPROVAL_BASIS_FLAGS))
        );
        assert_eq!(
            annotations.extra.get(annotations::ESCALATING_PARAMS_KEY),
            Some(&json!(["all"])),
            "the recorded name must be the schema property a caller will send"
        );
    }

    #[test]
    fn test_a_command_accepting_nothing_escalating_is_not_marked() {
        let tool = make_tool("git", vec![command_accepting("log", &["--oneline"])]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(!annotations.requires_approval);
        assert!(!annotations
            .extra
            .contains_key(annotations::APPROVAL_BASIS_KEY));
    }

    /// Destructive-by-name carries no basis, so the gate never stands down on
    /// it: `rm` prompts whether or not `-f` was sent.
    #[test]
    fn test_a_destructive_name_is_marked_without_a_flag_basis() {
        let tool = make_tool("rm", vec![command_accepting("delete", &["--force"])]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(annotations.requires_approval);
        assert!(
            !annotations
                .extra
                .contains_key(annotations::APPROVAL_BASIS_KEY),
            "an unconditional mark must not be downgradable to a conditional one"
        );
    }

    /// An overlay that states the answer outranks the flag list, and must take
    /// the basis with it — otherwise the gate could stand down on a call a
    /// human said to gate.
    #[test]
    fn test_an_overlay_assertion_clears_a_flag_basis() {
        let mut tool = make_tool("git", vec![command_accepting("push", &["--force"])]);
        tool.annotation_overrides = crate::models::AnnotationOverrides {
            requires_approval: Some(true),
            ..Default::default()
        };
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(annotations.requires_approval);
        assert!(!annotations
            .extra
            .contains_key(annotations::APPROVAL_BASIS_KEY));
        assert!(!annotations
            .extra
            .contains_key(annotations::ESCALATING_PARAMS_KEY));
    }

    /// The case no name list can reach: `--privileged` is not called `--force`
    /// anywhere, so only an overlay's assertion puts it under the gate.
    #[test]
    fn test_an_overlay_asserted_escalating_flag_joins_the_list() {
        let mut command = command_accepting("run", &["--privileged"]);
        command.flags[0].risk = crate::models::FlagRisk::Escalates;
        let tool = make_tool("docker", vec![command]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(annotations.requires_approval);
        assert_eq!(
            annotations.extra.get(annotations::ESCALATING_PARAMS_KEY),
            Some(&json!(["privileged"]))
        );
    }

    /// The false positive that motivated the variant: `bsdtar -y` is bzip2
    /// compression, and the name list reads it as "assume yes". Without a way
    /// to say so, every `tar` overlay author's only recourse was to assert
    /// `requires_approval: false` for the whole command — which would also say
    /// `tar` may overwrite files unattended.
    #[test]
    fn test_an_overlay_asserted_benign_flag_overrides_the_name_list() {
        let mut command = command_accepting("tar", &["-y"]);
        command.flags[0].risk = crate::models::FlagRisk::Benign;
        let tool = make_tool("tar", vec![command]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(
            !annotations.requires_approval,
            "the only escalating candidate was suppressed, so nothing is left to gate"
        );
        assert!(!annotations
            .extra
            .contains_key(annotations::APPROVAL_BASIS_KEY));
    }

    /// Suppression is per flag, not per command: the rest of the gating stands.
    #[test]
    fn test_a_benign_flag_does_not_suppress_its_neighbours() {
        let mut command = command_accepting("tar", &["-y", "--force"]);
        command.flags[0].risk = crate::models::FlagRisk::Benign;
        let tool = make_tool("tar", vec![command]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(annotations.requires_approval);
        assert_eq!(
            annotations.extra.get(annotations::ESCALATING_PARAMS_KEY),
            Some(&json!(["force"])),
            "-y is suppressed; --force is not"
        );
    }

    /// An exec parameter is refused by the executor before it can run, so
    /// putting it under the approval gate would ask a human to decide about a
    /// call that cannot proceed either way.
    #[test]
    fn test_an_exec_parameter_is_not_treated_as_merely_escalating() {
        let mut command = command_accepting("fetch", &["--upload-pack"]);
        command.flags[0].risk = crate::models::FlagRisk::Executes;
        let tool = make_tool("git", vec![command]);
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(!annotations.requires_approval);
        assert!(!annotations
            .extra
            .contains_key(annotations::ESCALATING_PARAMS_KEY));
    }

    #[test]
    fn test_escalating_property_names_are_sorted_for_a_stable_binding() {
        let tool = make_tool(
            "git",
            vec![command_accepting(
                "push",
                &["--recursive", "--all", "--force"],
            )],
        );
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert_eq!(
            annotations.extra.get(annotations::ESCALATING_PARAMS_KEY),
            Some(&json!(["all", "force", "recursive"]))
        );
    }

    #[test]
    fn test_annotation_overrides_beat_name_inference() {
        // `rm` is inferred destructive from its name alone. An overlay that
        // states otherwise must win, because it was reviewed and the inference
        // was a guess.
        let mut tool = make_tool("rm", vec![]);
        tool.annotation_overrides = crate::models::AnnotationOverrides {
            readonly: Some(true),
            destructive: Some(false),
            idempotent: Some(true),
            requires_approval: Some(false),
            open_world: None,
        };
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();

        assert!(annotations.readonly);
        assert!(!annotations.destructive);
        assert!(annotations.idempotent);
        assert!(!annotations.requires_approval);
        assert!(
            annotations.cacheable,
            "cacheable is derived, so it must be recomputed after an override"
        );
    }

    #[test]
    fn test_overlay_can_state_open_world_where_the_name_cannot() {
        // `sed` is on no name list, so inference calls it closed-world. That is
        // right for BSD sed and wrong for GNU sed, whose `s///e` runs its
        // replacement as a shell command -- verified as
        // `sed 's/x/echo PWNED/e'` printing PWNED, and refused by --sandbox
        // with "e/r/w commands disabled in sandbox mode". One name, two
        // answers: the variant is knowable only to an overlay.
        let mut tool = make_tool("sed", vec![]);
        assert!(
            !CliToolConverter::new().convert(&tool)[0]
                .annotations
                .as_ref()
                .unwrap()
                .open_world,
            "inference must call sed closed-world, or this test proves nothing"
        );

        tool.annotation_overrides = crate::models::AnnotationOverrides {
            open_world: Some(true),
            ..Default::default()
        };
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();
        assert!(annotations.open_world);
    }

    #[test]
    fn test_overlay_can_clear_an_inferred_open_world() {
        // The override has to work downward too. `curl` is on the name list, so
        // a build of it that genuinely cannot reach the network has no way to
        // say so except here.
        let mut tool = make_tool("curl", vec![]);
        assert!(
            CliToolConverter::new().convert(&tool)[0]
                .annotations
                .as_ref()
                .unwrap()
                .open_world,
            "curl must be inferred open-world, or this test proves nothing"
        );

        tool.annotation_overrides = crate::models::AnnotationOverrides {
            open_world: Some(false),
            ..Default::default()
        };
        let modules = CliToolConverter::new().convert(&tool);
        assert!(!modules[0].annotations.as_ref().unwrap().open_world);
    }

    #[test]
    fn test_absent_annotation_overrides_leave_inference_alone() {
        let tool = make_tool("delete", vec![]);
        assert!(tool.annotation_overrides.is_empty());
        let modules = CliToolConverter::new().convert(&tool);
        let annotations = modules[0].annotations.as_ref().unwrap();
        assert!(annotations.destructive, "inference must still apply");
    }

    #[test]
    fn test_tool_positional_args_reach_the_root_input_schema() {
        // `ls [file ...]` has no subcommand to hang its positional argument on,
        // so tool-level positional args must surface through the synthesized
        // root command or they vanish from the generated schema.
        let mut tool = make_tool("ls", vec![]);
        tool.positional_args = vec![crate::models::ScannedArg {
            name: "file".to_string(),
            description: "Files to list.".to_string(),
            value_type: crate::models::ValueType::Path,
            required: false,
            variadic: true,
            before_flags: false,
        }];
        let modules = CliToolConverter::new().convert(&tool);
        let properties = modules[0].input_schema["properties"].as_object().unwrap();
        assert!(
            properties.contains_key("file"),
            "positional arg missing from input schema: {properties:?}"
        );
    }

    #[test]
    fn test_converter_single_command_tool() {
        let cmd = make_command("status", "git status");
        let tool = make_tool("git", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module_id, "cli.git.status");
    }

    #[test]
    fn test_converter_root_module_uses_tool_description() {
        // A subcommand-less tool used to fall back to "Execute <name>" even when
        // the man page supplied a real description.
        let mut tool = make_tool("ls", vec![]);
        tool.description = "List directory contents.".to_string();
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].description, "List directory contents.");
    }

    #[test]
    fn test_converter_root_module_falls_back_without_description() {
        let tool = make_tool("mytool", vec![]);
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules[0].description, "Execute mytool");
    }

    #[test]
    fn test_converter_tool_with_subcommands() {
        let cmd1 = make_command("status", "git status");
        let cmd2 = make_command("commit", "git commit");
        let tool = make_tool("git", vec![cmd1, cmd2]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules.len(), 2);
        let ids: Vec<&str> = modules.iter().map(|m| m.module_id.as_str()).collect();
        assert!(ids.contains(&"cli.git.status"));
        assert!(ids.contains(&"cli.git.commit"));
    }

    #[test]
    fn test_converter_nested_subcommands() {
        let leaf = make_command("ls", "docker container ls");
        let mut parent = make_command("container", "docker container");
        parent.subcommands = vec![leaf];

        let tool = make_tool("docker", vec![parent]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module_id, "cli.docker.container.ls");
    }

    #[test]
    fn test_converter_propagates_examples() {
        // Spec §3.3 step 10: command.examples must land on module.examples.
        let mut cmd = make_command("status", "git status");
        cmd.examples = vec![
            "git status -s".to_string(),
            "git status --short".to_string(),
        ];
        let tool = make_tool("git", vec![cmd]);
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules.len(), 1);
        let titles: Vec<&str> = modules[0]
            .examples
            .iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(titles, vec!["git status -s", "git status --short"]);
    }

    #[test]
    fn test_converter_preserves_unicode_tool_name_outside_the_id() {
        // F1 §5 edge. This used to assert that unicode survives *into* the
        // module_id, which is the same defect as `cli.git.cat-file`: the apcore
        // grammar is ASCII, so such an id is written, counted, and then refused
        // at registration. The name is preserved — in metadata, where it does
        // not have to satisfy a grammar — and the id is folded.
        let cmd = make_command("état", "café état");
        let tool = make_tool("café", vec![cmd]);
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules.len(), 1);
        assert!(
            apcore::module_id_pattern().is_match(&modules[0].module_id),
            "unregistrable id: {}",
            modules[0].module_id
        );
        assert_eq!(
            modules[0].metadata.get("command_path"),
            Some(&json!(["café", "état"])),
            "the real command name must survive folding"
        );
        assert_eq!(modules[0].target, "exec:///usr/bin/café état");
    }

    #[test]
    fn test_converter_flag_without_name_uses_unknown_key() {
        // F1 §5 edge: a flag with neither long nor short name falls back to the
        // "unknown" canonical key rather than panicking or being dropped.
        use crate::models::{ScannedFlag, ValueType};
        let mut cmd = make_command("run", "tool run");
        cmd.flags = vec![ScannedFlag {
            long_name: None,
            short_name: None,
            description: "a nameless flag".to_string(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
            ..Default::default()
        }];
        let tool = make_tool("tool", vec![cmd]);
        let modules = CliToolConverter::new().convert(&tool);

        let props = &modules[0].input_schema["properties"];
        assert!(
            props.get("unknown").is_some(),
            "no-name flag should map to the 'unknown' key; got {props:?}"
        );
    }

    #[test]
    fn test_converter_deduplicates_colliding_module_ids() {
        // Spec §5: two leaf commands flattening to the same dotted path must
        // both survive with disambiguated ids, not silently collide.
        let cmd1 = make_command("status", "git status");
        let cmd2 = make_command("status", "git status");
        let tool = make_tool("git", vec![cmd1, cmd2]);
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules.len(), 2);
        let ids: Vec<&str> = modules.iter().map(|m| m.module_id.as_str()).collect();
        assert!(ids.contains(&"cli.git.status"));
        assert!(ids.contains(&"cli.git.status_2"));
    }

    #[test]
    fn test_converter_deeply_nested() {
        let deep = make_command("info", "k8s cluster node info");
        let mut mid = make_command("node", "k8s cluster node");
        mid.subcommands = vec![deep];
        let mut top = make_command("cluster", "k8s cluster");
        top.subcommands = vec![mid];

        let tool = make_tool("k8s", vec![top]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module_id, "cli.k8s.cluster.node.info");
    }

    #[test]
    fn test_converter_module_id_format() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert!(modules[0].module_id.starts_with("cli."));
        assert!(modules[0].module_id.contains("mytool"));
    }

    #[test]
    fn test_converter_custom_namespace() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::with_namespace("custom");
        let modules = converter.convert(&tool);

        assert_eq!(modules[0].module_id, "custom.mytool.list");
    }

    #[test]
    fn test_converter_description_copied() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules[0].description, "list description");
    }

    #[test]
    fn test_converter_version_present() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules[0].version, "1.0.0");
    }

    #[test]
    fn test_converter_version_missing() {
        let cmd = make_command("list", "mytool list");
        let mut tool = make_tool("mytool", vec![cmd]);
        tool.version = None;
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules[0].version, "unknown");
    }

    #[test]
    fn test_converter_tags_include_tool_name() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert!(modules[0].tags.contains(&"cli".to_string()));
        assert!(modules[0].tags.contains(&"mytool".to_string()));
    }

    #[test]
    fn test_converter_tags_include_help_format() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert!(modules[0].tags.contains(&"gnu".to_string()));
    }

    #[test]
    fn test_converter_tags_structured_output() {
        let mut cmd = make_command("list", "mytool list");
        cmd.structured_output = StructuredOutputInfo {
            supported: true,
            flag: Some("--json".to_string()),
            format: Some("json".to_string()),
        };
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert!(modules[0].tags.contains(&"structured-output".to_string()));
    }

    #[test]
    fn test_converter_target_format() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        // The target is the binary plus the *subcommand* path. Asserting
        // "mytool mytool list" here used to pin the `git git log` defect as
        // correct behaviour; the argv it produced was rejected by every tool
        // that has subcommands.
        assert_eq!(modules[0].target, "exec:///usr/bin/mytool list");
    }

    #[test]
    fn test_converter_nested_subcommand_target_keeps_full_path() {
        let mut parent = make_command("remote", "mytool remote");
        parent.subcommands = vec![make_command("add", "mytool remote add")];
        let tool = make_tool("mytool", vec![parent]);
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules[0].module_id, "cli.mytool.remote.add");
        assert_eq!(modules[0].target, "exec:///usr/bin/mytool remote add");
    }

    #[test]
    fn test_converter_hyphenated_subcommand_gets_registrable_id() {
        // `git cat-file` produced `cli.git.cat-file`, which apcore rejects at
        // registration; the module was written, counted by `apexe list`, and
        // then silently never served.
        let tool = make_tool("git", vec![make_command("cat-file", "git cat-file")]);
        let modules = CliToolConverter::new().convert(&tool);

        assert_eq!(modules[0].module_id, "cli.git.cat_file");
        assert!(
            apcore::module_id_pattern().is_match(&modules[0].module_id),
            "generated id must satisfy the grammar apcore enforces at registration"
        );
        // argv still needs the hyphen, so the real name has to survive.
        assert_eq!(
            modules[0].target, "exec:///usr/bin/git cat-file",
            "the folded id must not reach the command line"
        );
        assert_eq!(
            modules[0].metadata.get("command_path"),
            Some(&json!(["git", "cat-file"]))
        );
    }

    #[test]
    fn test_sanitize_id_segment_folds_out_of_charset_characters() {
        assert_eq!(sanitize_id_segment("cat-file"), "cat_file");
        assert_eq!(sanitize_id_segment("for-each-ref"), "for_each_ref");
        assert_eq!(sanitize_id_segment("MyTool"), "mytool");
        assert_eq!(sanitize_id_segment("plain"), "plain");
        // A leading non-letter would still fail the grammar.
        assert_eq!(sanitize_id_segment("7z"), "t7z");
    }

    #[test]
    fn test_converter_warnings_propagated() {
        let cmd = make_command("list", "mytool list");
        let mut tool = make_tool("mytool", vec![cmd]);
        tool.warnings = vec!["scan warning".to_string()];
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert!(modules[0].warnings.contains(&"scan warning".to_string()));
    }

    #[test]
    fn test_converter_from_trait() {
        let cmd = make_command("list", "mytool list");
        let tool = make_tool("mytool", vec![cmd]);
        let modules: Vec<ScannedModule> = Vec::from(&tool);

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module_id, "cli.mytool.list");
    }

    #[test]
    fn test_converter_convert_all() {
        let tool1 = make_tool("tool1", vec![make_command("cmd1", "tool1 cmd1")]);
        let tool2 = make_tool("tool2", vec![make_command("cmd2", "tool2 cmd2")]);
        let converter = CliToolConverter::new();
        let modules = converter.convert_all(&[tool1, tool2]);

        assert_eq!(modules.len(), 2);
        let ids: Vec<&str> = modules.iter().map(|m| m.module_id.as_str()).collect();
        assert!(ids.contains(&"cli.tool1.cmd1"));
        assert!(ids.contains(&"cli.tool2.cmd2"));
    }

    #[test]
    fn test_convert_all_deduplicates_module_ids_across_tools() {
        // Regression: `apexe scan ls /bin/ls` (two path arguments that
        // resolve to the same command) produces two ScannedCLITool entries
        // both named "ls". convert() dedupes within one tool's own leaves;
        // convert_all flat_mapped convert() per tool, so a collision ACROSS
        // tools survived into the returned Vec -- e.g. two files on disk
        // both carrying module_id "cli.ls", which the registry then refuses
        // to register both halves of (registers one, warns and drops the
        // other) while `apexe list` still counts both.
        let tool1 = make_tool("ls", vec![]);
        let tool2 = make_tool("ls", vec![]);
        let converter = CliToolConverter::new();
        let modules = converter.convert_all(&[tool1, tool2]);

        assert_eq!(modules.len(), 2);
        let ids: Vec<&str> = modules.iter().map(|m| m.module_id.as_str()).collect();
        assert!(ids.contains(&"cli.ls"), "{ids:?}");
        assert!(
            ids.contains(&"cli.ls_2"),
            "the second tool's collision must be disambiguated, not left duplicate: {ids:?}"
        );
    }

    #[test]
    fn test_converter_empty_tool() {
        // Root-only tool with no subcommands.
        let tool = make_tool("ffmpeg", vec![]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module_id, "cli.ffmpeg");
        // Root-only target should NOT repeat the tool name as an argument
        assert_eq!(modules[0].target, "exec:///usr/bin/ffmpeg");
    }

    #[test]
    fn test_converter_empty_description_fallback() {
        let mut cmd = make_command("run", "mytool run");
        cmd.description = String::new();
        let tool = make_tool("mytool", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules[0].description, "Execute mytool run");
    }

    #[test]
    fn test_converter_display_metadata_populated() {
        let cmd = make_command("status", "git status");
        let tool = make_tool("git", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        assert_eq!(modules.len(), 1);
        let display = modules[0].metadata.get("display");
        assert!(
            display.is_some(),
            "metadata[\"display\"] should be populated"
        );
        let display = display.unwrap();
        assert!(display.get("alias").is_some());
        assert!(display.get("cli").is_some());
        assert!(display.get("mcp").is_some());
        assert!(display.get("a2a").is_some());
    }

    #[test]
    fn test_converter_display_alias_set() {
        let cmd = make_command("commit", "git commit");
        let tool = make_tool("git", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        let display = &modules[0].metadata["display"];
        // suggested_alias for path ["commit"] is "git_commit"
        assert_eq!(display["alias"], "git_commit");
    }

    #[test]
    fn test_converter_display_mcp_alias_sanitized() {
        // module_id will be "cli.git.commit" — dots should be replaced with underscores in MCP alias
        let cmd = make_command("commit", "git commit");
        let tool = make_tool("git", vec![cmd]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        let mcp_alias = modules[0].metadata["display"]["mcp"]["alias"]
            .as_str()
            .unwrap();
        assert!(
            !mcp_alias.contains('.'),
            "MCP alias should not contain dots, got: {mcp_alias}"
        );
        assert_eq!(mcp_alias, "git_commit");
    }

    #[test]
    fn test_converter_suggested_alias_root_only() {
        let tool = make_tool("ffmpeg", vec![]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        let display = &modules[0].metadata["display"];
        assert_eq!(display["alias"], "ffmpeg");
    }

    #[test]
    fn test_converter_suggested_alias_nested() {
        let leaf = make_command("ls", "docker container ls");
        let mut parent = make_command("container", "docker container");
        parent.subcommands = vec![leaf];

        let tool = make_tool("docker", vec![parent]);
        let converter = CliToolConverter::new();
        let modules = converter.convert(&tool);

        let display = &modules[0].metadata["display"];
        assert_eq!(display["alias"], "docker_container_ls");
    }
}
