use apexe::binding::{BindingGenerator, BindingYAMLWriter, GeneratedBinding};
use apexe::models::*;

fn make_mock_git_tool() -> ScannedCLITool {
    let status_cmd = ScannedCommand {
        name: "status".to_string(),
        full_command: "git status".to_string(),
        description: "Show the working tree status".to_string(),
        flags: vec![
            ScannedFlag {
                long_name: Some("--short".to_string()),
                short_name: Some("-s".to_string()),
                description: "Give output in short format".to_string(),
                value_type: ValueType::Boolean,
                required: false,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: None,
            },
            ScannedFlag {
                long_name: Some("--branch".to_string()),
                short_name: Some("-b".to_string()),
                description: "Show branch info".to_string(),
                value_type: ValueType::Boolean,
                required: false,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: None,
            },
        ],
        positional_args: vec![ScannedArg {
            name: "pathspec".to_string(),
            description: "Path to filter status".to_string(),
            value_type: ValueType::Path,
            required: false,
            variadic: true,
        }],
        subcommands: vec![],
        examples: vec!["git status -s".to_string()],
        help_format: HelpFormat::Gnu,
        structured_output: StructuredOutputInfo::default(),
        raw_help: String::new(),
    };

    let commit_cmd = ScannedCommand {
        name: "commit".to_string(),
        full_command: "git commit".to_string(),
        description: "Record changes to the repository".to_string(),
        flags: vec![
            ScannedFlag {
                long_name: Some("--message".to_string()),
                short_name: Some("-m".to_string()),
                description: "Commit message".to_string(),
                value_type: ValueType::String,
                required: true,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: Some("MSG".to_string()),
            },
            ScannedFlag {
                long_name: Some("--all".to_string()),
                short_name: Some("-a".to_string()),
                description: "Stage all modified files".to_string(),
                value_type: ValueType::Boolean,
                required: false,
                default: None,
                enum_values: None,
                repeatable: false,
                value_name: None,
            },
        ],
        positional_args: vec![],
        subcommands: vec![],
        examples: vec![],
        help_format: HelpFormat::Gnu,
        structured_output: StructuredOutputInfo::default(),
        raw_help: String::new(),
    };

    let remote_add_cmd = ScannedCommand {
        name: "add".to_string(),
        full_command: "git remote add".to_string(),
        description: "Add a remote".to_string(),
        flags: vec![],
        positional_args: vec![
            ScannedArg {
                name: "name".to_string(),
                description: "Remote name".to_string(),
                value_type: ValueType::String,
                required: true,
                variadic: false,
            },
            ScannedArg {
                name: "url".to_string(),
                description: "Remote URL".to_string(),
                value_type: ValueType::Url,
                required: true,
                variadic: false,
            },
        ],
        subcommands: vec![],
        examples: vec![],
        help_format: HelpFormat::Gnu,
        structured_output: StructuredOutputInfo::default(),
        raw_help: String::new(),
    };

    let remote_cmd = ScannedCommand {
        name: "remote".to_string(),
        full_command: "git remote".to_string(),
        description: "Manage set of tracked repositories".to_string(),
        flags: vec![ScannedFlag {
            long_name: Some("--verbose".to_string()),
            short_name: Some("-v".to_string()),
            description: "Be verbose".to_string(),
            value_type: ValueType::Boolean,
            required: false,
            default: None,
            enum_values: None,
            repeatable: false,
            value_name: None,
        }],
        positional_args: vec![],
        subcommands: vec![remote_add_cmd],
        examples: vec![],
        help_format: HelpFormat::Gnu,
        structured_output: StructuredOutputInfo::default(),
        raw_help: String::new(),
    };

    ScannedCLITool {
        name: "git".to_string(),
        binary_path: "/usr/bin/git".to_string(),
        version: Some("2.43.0".to_string()),
        subcommands: vec![status_cmd, commit_cmd, remote_cmd],
        global_flags: vec![],
        structured_output: StructuredOutputInfo::default(),
        scan_tier: 1,
        warnings: vec![],
    }
}

#[test]
fn test_end_to_end_generate_and_write() {
    let tool = make_mock_git_tool();
    let generator = BindingGenerator::new();
    let binding_file = generator.generate(&tool).unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let writer = BindingYAMLWriter;
    let path = writer.write(&binding_file, tmp.path()).unwrap();

    // F3-T8: Verify file exists
    assert!(path.exists());
    assert_eq!(path.file_name().unwrap(), "git.binding.yaml");

    // Read and parse YAML
    let content = std::fs::read_to_string(&path).unwrap();
    let yaml_content: String = content
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<&str>>()
        .join("\n");

    #[derive(serde::Deserialize)]
    struct BindingDoc {
        bindings: Vec<GeneratedBinding>,
    }

    let doc: BindingDoc = serde_yaml::from_str(&yaml_content).unwrap();

    // F3-T8: Verify correct number of bindings
    // status, commit, remote, remote.add = 4
    assert_eq!(doc.bindings.len(), 4);

    // F3-T8: Verify module IDs follow cli. prefix convention
    let module_ids: Vec<&str> = doc.bindings.iter().map(|b| b.module_id.as_str()).collect();
    for id in &module_ids {
        assert!(id.starts_with("cli."), "Module ID '{id}' should start with 'cli.'");
    }

    // Verify specific module IDs
    assert!(module_ids.contains(&"cli.git.status"));
    assert!(module_ids.contains(&"cli.git.commit"));
    assert!(module_ids.contains(&"cli.git.remote"));
    assert!(module_ids.contains(&"cli.git.remote.add"));
}

#[test]
fn test_binding_schemas_present() {
    let tool = make_mock_git_tool();
    let generator = BindingGenerator::new();
    let binding_file = generator.generate(&tool).unwrap();

    for binding in &binding_file.bindings {
        // Every binding should have input and output schemas
        assert!(binding.input_schema.is_object(), "input_schema should be object for {}", binding.module_id);
        assert!(binding.output_schema.is_object(), "output_schema should be object for {}", binding.module_id);

        // Every binding should have a target
        assert_eq!(binding.target, "apexe::executor::execute_cli");

        // Every binding should have tags
        assert!(binding.tags.contains(&"cli".to_string()));
        assert!(binding.tags.contains(&"git".to_string()));
    }
}

#[test]
fn test_binding_metadata_fields() {
    let tool = make_mock_git_tool();
    let generator = BindingGenerator::new();
    let binding_file = generator.generate(&tool).unwrap();

    for binding in &binding_file.bindings {
        assert!(
            binding.metadata.contains_key("apexe_binary"),
            "Missing apexe_binary in {}",
            binding.module_id
        );
        assert!(
            binding.metadata.contains_key("apexe_command"),
            "Missing apexe_command in {}",
            binding.module_id
        );
        assert!(
            binding.metadata.contains_key("apexe_timeout"),
            "Missing apexe_timeout in {}",
            binding.module_id
        );
    }
}

#[test]
fn test_module_id_regex_validation() {
    let tool = make_mock_git_tool();
    let generator = BindingGenerator::new();
    let binding_file = generator.generate(&tool).unwrap();

    let re = regex::Regex::new(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$").unwrap();
    for binding in &binding_file.bindings {
        assert!(
            re.is_match(&binding.module_id),
            "Module ID '{}' does not match required pattern",
            binding.module_id
        );
    }
}

#[test]
fn test_commit_command_has_required_message() {
    let tool = make_mock_git_tool();
    let generator = BindingGenerator::new();
    let binding_file = generator.generate(&tool).unwrap();

    let commit = binding_file
        .bindings
        .iter()
        .find(|b| b.module_id == "cli.git.commit")
        .unwrap();

    let required = commit.input_schema["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "message"));
}

#[test]
fn test_yaml_round_trip_preserves_data() {
    let tool = make_mock_git_tool();
    let generator = BindingGenerator::new();
    let original = generator.generate(&tool).unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let writer = BindingYAMLWriter;
    let path = writer.write(&original, tmp.path()).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let yaml_content: String = content
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<&str>>()
        .join("\n");

    #[derive(serde::Deserialize)]
    struct BindingDoc {
        bindings: Vec<GeneratedBinding>,
    }

    let doc: BindingDoc = serde_yaml::from_str(&yaml_content).unwrap();

    // Verify data survived the round trip
    for (orig, loaded) in original.bindings.iter().zip(doc.bindings.iter()) {
        assert_eq!(orig.module_id, loaded.module_id);
        assert_eq!(orig.description, loaded.description);
        assert_eq!(orig.target, loaded.target);
        assert_eq!(orig.tags, loaded.tags);
        assert_eq!(orig.version, loaded.version);
    }
}
