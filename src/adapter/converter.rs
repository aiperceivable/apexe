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

    /// Build a single ScannedModule from a leaf command (or synthesized root).
    fn build_single_module(
        &self,
        tool: &ScannedCLITool,
        path: &[String],
        command_opt: Option<&&ScannedCommand>,
    ) -> ScannedModule {
        let mut segments = vec![self.namespace.clone(), sanitize_id_segment(&tool.name)];
        segments.extend(path.iter().map(|segment| sanitize_id_segment(segment)));
        let module_id = segments.join(".");

        let fields = Self::extract_command_fields(tool, command_opt);

        let help_format_name =
            help_format_to_tag(command_opt.map_or(HelpFormat::Unknown, |c| c.help_format));

        let tags = self.build_tags(tool, command_opt, help_format_name);
        // For root-only tools (no subcommands), target is just the binary path.
        // For subcommands, append the subcommand path only (e.g.
        // "exec:///usr/bin/git commit"). `fields.full_command` cannot be used
        // here: it is the *full* invocation ("git commit"), so prefixing the
        // binary path spelled the tool name twice and produced `git git commit`.
        let target = if path.is_empty() {
            format!("exec://{}", tool.binary_path)
        } else {
            format!("exec://{} {}", tool.binary_path, path.join(" "))
        };
        let version = tool
            .version
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let metadata = self.build_metadata(tool, path, help_format_name);

        let mut module = ScannedModule::new(
            module_id,
            fields.description,
            fields.input_schema,
            fields.output_schema,
            tags,
            target,
        );
        module.version = version;
        module.annotations = Some(fields.annotations);
        module.documentation = fields.documentation;
        module.metadata = metadata;
        module.warnings = tool.warnings.clone();
        // Spec §3.3 step 10: carry the command's parsed examples onto the module
        // so downstream MCP/A2A docs surface them. command.examples is a plain
        // Vec<String> invocation list; wrap each as a titled ModuleExample.
        module.examples = fields
            .examples
            .into_iter()
            .map(|ex| {
                // ModuleExample is #[non_exhaustive]; build via Default + field set.
                let mut example = ModuleExample::default();
                example.title = ex;
                example
            })
            .collect();

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

    /// Extract description, schemas, annotations, docs, and examples from a command.
    fn extract_command_fields(
        tool: &ScannedCLITool,
        command_opt: Option<&&ScannedCommand>,
    ) -> CommandFields {
        if let Some(command) = command_opt {
            let description = if command.description.is_empty() {
                format!("Execute {}", command.full_command)
            } else {
                command.description.clone()
            };
            let documentation = if command.raw_help.is_empty() {
                None
            } else {
                Some(command.raw_help.clone())
            };
            CommandFields {
                description,
                input_schema: schema::build_input_schema(command, &tool.global_flags),
                output_schema: schema::build_output_schema(command),
                annotations: apply_annotation_overrides(annotations::infer(command), tool),
                documentation,
                examples: command.examples.clone(),
            }
        } else {
            let synth = synthesize_root_command(tool);
            let description = [synth.description.as_str(), tool.description.as_str()]
                .into_iter()
                .find(|candidate| !candidate.is_empty())
                .map_or_else(|| format!("Execute {}", tool.name), str::to_string);
            let documentation = if synth.raw_help.is_empty() {
                None
            } else {
                Some(synth.raw_help.clone())
            };
            CommandFields {
                description,
                input_schema: schema::build_input_schema(&synth, &tool.global_flags),
                output_schema: schema::build_output_schema(&synth),
                annotations: apply_annotation_overrides(annotations::infer(&synth), tool),
                documentation,
                examples: synth.examples.clone(),
            }
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
    pub fn convert_all(&self, tools: &[ScannedCLITool]) -> Vec<ScannedModule> {
        tools.iter().flat_map(|t| self.convert(t)).collect()
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
    }
    annotations.cacheable = annotations.readonly && annotations.idempotent;
    annotations
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
