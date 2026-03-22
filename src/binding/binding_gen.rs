use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use super::module_id::generate_module_id;
use super::schema_gen::SchemaGenerator;
use crate::errors::ApexeError;
use crate::models::{ScannedCLITool, ScannedCommand};

/// A single generated binding entry for a CLI command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedBinding {
    pub module_id: String,
    pub description: String,
    pub target: String,
    pub input_schema: JsonValue,
    pub output_schema: JsonValue,
    pub tags: Vec<String>,
    pub version: String,
    pub annotations: HashMap<String, JsonValue>,
    pub metadata: HashMap<String, JsonValue>,
}

/// A complete binding file for one CLI tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedBindingFile {
    pub tool_name: String,
    pub file_name: String,
    pub bindings: Vec<GeneratedBinding>,
}

/// Transforms ScannedCLITool into GeneratedBindingFile.
pub struct BindingGenerator {
    schema_gen: SchemaGenerator,
}

impl Default for BindingGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl BindingGenerator {
    pub fn new() -> Self {
        Self {
            schema_gen: SchemaGenerator,
        }
    }

    /// Generate a complete binding file from a scanned tool.
    pub fn generate(&self, tool: &ScannedCLITool) -> Result<GeneratedBindingFile, ApexeError> {
        let mut bindings = Vec::new();
        self.generate_command_bindings(
            &tool.name,
            &tool.subcommands,
            &[],
            &mut bindings,
        )?;

        // Deduplicate module IDs
        deduplicate_ids(&mut bindings);

        Ok(GeneratedBindingFile {
            tool_name: tool.name.clone(),
            file_name: format!("{}.binding.yaml", tool.name),
            bindings,
        })
    }

    /// Recursively generate bindings for commands and subcommands.
    fn generate_command_bindings(
        &self,
        tool_name: &str,
        commands: &[ScannedCommand],
        parent_path: &[String],
        bindings: &mut Vec<GeneratedBinding>,
    ) -> Result<(), ApexeError> {
        for command in commands {
            let mut command_path: Vec<String> = parent_path.to_vec();
            command_path.push(command.name.clone());

            let module_id = generate_module_id(tool_name, &command_path)?;
            let input_schema = self.schema_gen.generate_input_schema(command);
            let output_schema = self.schema_gen.generate_output_schema(&command.structured_output);

            let mut metadata = HashMap::new();
            metadata.insert("apexe_binary".to_string(), json!(tool_name));

            let full_cmd: Vec<String> = std::iter::once(tool_name.to_string())
                .chain(command_path.iter().cloned())
                .collect();
            metadata.insert("apexe_command".to_string(), json!(full_cmd));
            metadata.insert("apexe_timeout".to_string(), json!(30));

            if command.structured_output.supported {
                if let Some(ref flag) = command.structured_output.flag {
                    metadata.insert("apexe_json_flag".to_string(), json!(flag));
                }
            }

            let description: String = command.description.chars().take(200).collect();

            let binding = GeneratedBinding {
                module_id,
                description,
                target: "apexe::executor::execute_cli".to_string(),
                input_schema,
                output_schema,
                tags: vec!["cli".to_string(), tool_name.to_string()],
                version: "1.0.0".to_string(),
                annotations: HashMap::new(),
                metadata,
            };

            bindings.push(binding);

            if !command.subcommands.is_empty() {
                self.generate_command_bindings(
                    tool_name,
                    &command.subcommands,
                    &command_path,
                    bindings,
                )?;
            }
        }

        Ok(())
    }
}

/// Deduplicate module IDs by appending _2, _3, etc. for collisions.
fn deduplicate_ids(bindings: &mut [GeneratedBinding]) {
    let mut seen: HashMap<String, usize> = HashMap::new();

    for binding in bindings.iter_mut() {
        let count = seen.entry(binding.module_id.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            binding.module_id = format!("{}_{}", binding.module_id, count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::*;

    fn make_tool_with_commands(commands: Vec<ScannedCommand>) -> ScannedCLITool {
        ScannedCLITool {
            name: "git".to_string(),
            binary_path: "/usr/bin/git".to_string(),
            version: Some("2.43.0".to_string()),
            subcommands: commands,
            global_flags: vec![],
            structured_output: StructuredOutputInfo::default(),
            scan_tier: 1,
            warnings: vec![],
        }
    }

    fn make_simple_command(name: &str, description: &str) -> ScannedCommand {
        ScannedCommand {
            name: name.to_string(),
            full_command: format!("git {name}"),
            description: description.to_string(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        }
    }

    #[test]
    fn test_generate_simple_tool() {
        let tool = make_tool_with_commands(vec![
            make_simple_command("status", "Show the working tree status"),
            make_simple_command("commit", "Record changes to the repository"),
        ]);

        let gen = BindingGenerator::new();
        let result = gen.generate(&tool).unwrap();

        assert_eq!(result.tool_name, "git");
        assert_eq!(result.file_name, "git.binding.yaml");
        assert_eq!(result.bindings.len(), 2);
        assert_eq!(result.bindings[0].module_id, "cli.git.status");
        assert_eq!(result.bindings[1].module_id, "cli.git.commit");
    }

    #[test]
    fn test_generate_nested_commands() {
        let inner = ScannedCommand {
            name: "add".to_string(),
            full_command: "git remote add".to_string(),
            description: "Add a remote".to_string(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        };
        let remote = ScannedCommand {
            name: "remote".to_string(),
            full_command: "git remote".to_string(),
            description: "Manage remotes".to_string(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![inner],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        };

        let tool = make_tool_with_commands(vec![remote]);
        let gen = BindingGenerator::new();
        let result = gen.generate(&tool).unwrap();

        assert_eq!(result.bindings.len(), 2);
        assert_eq!(result.bindings[0].module_id, "cli.git.remote");
        assert_eq!(result.bindings[1].module_id, "cli.git.remote.add");
    }

    #[test]
    fn test_binding_metadata() {
        let tool = make_tool_with_commands(vec![
            make_simple_command("status", "Show status"),
        ]);

        let gen = BindingGenerator::new();
        let result = gen.generate(&tool).unwrap();
        let binding = &result.bindings[0];

        assert_eq!(binding.target, "apexe::executor::execute_cli");
        assert_eq!(binding.tags, vec!["cli", "git"]);
        assert_eq!(binding.version, "1.0.0");
        assert_eq!(binding.metadata["apexe_binary"], json!("git"));
        assert_eq!(binding.metadata["apexe_command"], json!(["git", "status"]));
        assert_eq!(binding.metadata["apexe_timeout"], json!(30));
    }

    #[test]
    fn test_binding_with_structured_output() {
        let cmd = ScannedCommand {
            name: "status".to_string(),
            full_command: "git status".to_string(),
            description: "Show status".to_string(),
            flags: vec![],
            positional_args: vec![],
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo {
                supported: true,
                flag: Some("--format=json".to_string()),
                format: Some("json".to_string()),
            },
            raw_help: String::new(),
        };

        let tool = make_tool_with_commands(vec![cmd]);
        let gen = BindingGenerator::new();
        let result = gen.generate(&tool).unwrap();

        assert_eq!(result.bindings[0].metadata["apexe_json_flag"], json!("--format=json"));
        assert!(result.bindings[0].output_schema["properties"]["json_output"].is_object());
    }

    #[test]
    fn test_deduplication() {
        let mut bindings = vec![
            GeneratedBinding {
                module_id: "cli.git.status".to_string(),
                description: "First".to_string(),
                target: String::new(),
                input_schema: json!({}),
                output_schema: json!({}),
                tags: vec![],
                version: "1.0.0".to_string(),
                annotations: HashMap::new(),
                metadata: HashMap::new(),
            },
            GeneratedBinding {
                module_id: "cli.git.status".to_string(),
                description: "Second".to_string(),
                target: String::new(),
                input_schema: json!({}),
                output_schema: json!({}),
                tags: vec![],
                version: "1.0.0".to_string(),
                annotations: HashMap::new(),
                metadata: HashMap::new(),
            },
        ];

        deduplicate_ids(&mut bindings);
        assert_eq!(bindings[0].module_id, "cli.git.status");
        assert_eq!(bindings[1].module_id, "cli.git.status_2");
    }

    #[test]
    fn test_default_impl() {
        let gen = BindingGenerator::default();
        let tool = make_tool_with_commands(vec![
            make_simple_command("test", "A test"),
        ]);
        let result = gen.generate(&tool);
        assert!(result.is_ok());
    }
}
