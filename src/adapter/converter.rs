use std::collections::HashMap;

use apcore_toolkit::{DisplayResolver, ScannedModule};
use serde_json::json;
use tracing::warn;

use crate::models::{HelpFormat, ScannedCLITool, ScannedCommand};

use super::{annotations, schema};

/// Converts ScannedCLITool instances into ScannedModule instances.
pub struct CliToolConverter {
    namespace: String,
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
        let module_id = if path.is_empty() {
            format!("{}.{}", self.namespace, tool.name)
        } else {
            format!("{}.{}.{}", self.namespace, tool.name, path.join("."))
        };

        let (description, input_schema, output_schema, ann, documentation, full_command) =
            Self::extract_command_fields(tool, command_opt);

        let help_format_name =
            help_format_to_tag(command_opt.map_or(HelpFormat::Unknown, |c| c.help_format));

        let tags = self.build_tags(tool, command_opt, help_format_name);
        // For root-only tools (no subcommands), target is just the binary path.
        // For subcommands, include the command path (e.g., "exec:///usr/bin/git commit").
        let target = if path.is_empty() {
            format!("exec://{}", tool.binary_path)
        } else {
            format!("exec://{} {}", tool.binary_path, full_command)
        };
        let version = tool
            .version
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let metadata = self.build_metadata(tool, path, help_format_name);

        let mut module = ScannedModule::new(
            module_id,
            description,
            input_schema,
            output_schema,
            tags,
            target,
        );
        module.version = version;
        module.annotations = Some(ann);
        module.documentation = documentation;
        module.metadata = metadata;
        module.warnings = tool.warnings.clone();

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
            tool.name.clone()
        } else {
            format!("{}_{}", tool.name, path.join("_"))
        };
        let mut metadata = HashMap::new();
        metadata.insert("scan_tier".to_string(), json!(tool.scan_tier));
        metadata.insert("help_format".to_string(), json!(help_format_name));
        metadata.insert("binary_path".to_string(), json!(tool.binary_path));
        metadata.insert("suggested_alias".to_string(), json!(suggested_alias));
        metadata
    }

    /// Extract description, schemas, annotations, docs, and full_command from a command.
    fn extract_command_fields(
        tool: &ScannedCLITool,
        command_opt: Option<&&ScannedCommand>,
    ) -> (
        String,
        serde_json::Value,
        serde_json::Value,
        apcore::module::ModuleAnnotations,
        Option<String>,
        String,
    ) {
        if let Some(command) = command_opt {
            let desc = if command.description.is_empty() {
                format!("Execute {}", command.full_command)
            } else {
                command.description.clone()
            };
            let input = schema::build_input_schema(command, &tool.global_flags);
            let output = schema::build_output_schema(command);
            let ann = annotations::infer(command);
            let doc = if command.raw_help.is_empty() {
                None
            } else {
                Some(command.raw_help.clone())
            };
            (desc, input, output, ann, doc, command.full_command.clone())
        } else {
            let synth = synthesize_root_command(tool);
            let desc = if synth.description.is_empty() {
                format!("Execute {}", tool.name)
            } else {
                synth.description.clone()
            };
            let input = schema::build_input_schema(&synth, &tool.global_flags);
            let output = schema::build_output_schema(&synth);
            let ann = annotations::infer(&synth);
            let doc = if synth.raw_help.is_empty() {
                None
            } else {
                Some(synth.raw_help.clone())
            };
            (desc, input, output, ann, doc, tool.name.clone())
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
        positional_args: vec![],
        subcommands: vec![],
        examples: vec![],
        help_format: HelpFormat::Unknown,
        structured_output: tool.structured_output.clone(),
        raw_help: String::new(),
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
            binary_path: format!("/usr/bin/{name}"),
            version: Some("1.0.0".to_string()),
            subcommands,
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
        }
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

        assert_eq!(modules[0].target, "exec:///usr/bin/mytool mytool list");
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
