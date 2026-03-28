use serde_json::{json, Value as JsonValue};

use crate::models::{ScannedArg, ScannedCommand, ScannedFlag, ValueType};

/// Map a ValueType to the corresponding JSON Schema type string.
fn value_type_to_json_schema(vt: ValueType) -> &'static str {
    match vt {
        ValueType::String => "string",
        ValueType::Integer => "integer",
        ValueType::Float => "number",
        ValueType::Boolean => "boolean",
        ValueType::Path => "string",
        ValueType::Enum => "string",
        ValueType::Url => "string",
        ValueType::Unknown => "string",
    }
}

/// Apply the default value from a flag to a JSON Schema property, coercing by type.
fn apply_default(schema: &mut JsonValue, flag: &ScannedFlag) {
    if let Some(ref default) = flag.default {
        match flag.value_type {
            ValueType::Integer => {
                if let Ok(n) = default.parse::<i64>() {
                    schema["default"] = json!(n);
                } else {
                    schema["default"] = json!(default);
                }
            }
            ValueType::Float => {
                if let Ok(n) = default.parse::<f64>() {
                    schema["default"] = json!(n);
                } else {
                    schema["default"] = json!(default);
                }
            }
            ValueType::Boolean => {
                schema["default"] = json!(default.parse::<bool>().unwrap_or(false));
            }
            _ => {
                schema["default"] = json!(default);
            }
        }
    } else if flag.value_type == ValueType::Boolean {
        schema["default"] = json!(false);
    }
}

/// Convert a ScannedFlag into a JSON Schema property value.
fn flag_to_schema(flag: &ScannedFlag) -> JsonValue {
    let base_type = value_type_to_json_schema(flag.value_type);

    if flag.repeatable {
        let mut schema = json!({
            "type": "array",
            "items": { "type": base_type },
        });
        if !flag.description.is_empty() {
            schema["description"] = json!(flag.description);
        }
        return schema;
    }

    let mut schema = json!({ "type": base_type });

    // Add format hints so AI agents can distinguish path/URI from plain strings
    match flag.value_type {
        ValueType::Path => {
            schema["format"] = json!("path");
        }
        ValueType::Url => {
            schema["format"] = json!("uri");
        }
        _ => {}
    }

    if !flag.description.is_empty() {
        schema["description"] = json!(flag.description);
    }

    apply_default(&mut schema, flag);

    if let Some(ref enum_values) = flag.enum_values {
        schema["enum"] = json!(enum_values);
    }

    schema
}

/// Convert a ScannedArg into a JSON Schema property value.
fn arg_to_schema(arg: &ScannedArg) -> JsonValue {
    let base_type = value_type_to_json_schema(arg.value_type);

    if arg.variadic {
        let mut schema = json!({
            "type": "array",
            "items": { "type": base_type },
        });
        if !arg.description.is_empty() {
            schema["description"] = json!(arg.description);
        }
        schema
    } else {
        let mut schema = json!({ "type": base_type });
        match arg.value_type {
            ValueType::Path => {
                schema["format"] = json!("path");
            }
            ValueType::Url => {
                schema["format"] = json!("uri");
            }
            _ => {}
        }
        if !arg.description.is_empty() {
            schema["description"] = json!(arg.description);
        }
        schema
    }
}

/// Build a JSON Schema for command inputs, merging command flags with global flags.
///
/// Command-level flags take precedence; global flags are included only when
/// their canonical name does not collide with a command-level flag.
pub fn build_input_schema(command: &ScannedCommand, global_flags: &[ScannedFlag]) -> JsonValue {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();

    // Command flags first.
    for flag in &command.flags {
        let prop_name = flag.canonical_name();
        let prop_schema = flag_to_schema(flag);
        properties.insert(prop_name.clone(), prop_schema);
        if flag.required {
            required.push(prop_name);
        }
    }

    // Global flags, skipping collisions.
    for flag in global_flags {
        let prop_name = flag.canonical_name();
        if !properties.contains_key(&prop_name) {
            let prop_schema = flag_to_schema(flag);
            properties.insert(prop_name.clone(), prop_schema);
            if flag.required {
                required.push(prop_name);
            }
        }
    }

    // Positional args.
    for arg in &command.positional_args {
        let prop_name = arg.name.to_lowercase().replace('-', "_");
        let prop_schema = arg_to_schema(arg);
        properties.insert(prop_name.clone(), prop_schema);
        if arg.required {
            required.push(prop_name);
        }
    }

    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });

    if !required.is_empty() {
        schema["required"] = json!(required);
    }

    schema
}

/// Build a JSON Schema for command output, including structured output when supported.
pub fn build_output_schema(command: &ScannedCommand) -> JsonValue {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "stdout": {
                "type": "string",
                "description": "Standard output from the command",
            },
            "stderr": {
                "type": "string",
                "description": "Standard error output from the command",
            },
            "exit_code": {
                "type": "integer",
                "description": "Process exit code (0 = success)",
            },
        },
        "required": ["stdout", "stderr", "exit_code"],
    });

    if command.structured_output.supported {
        schema["properties"]["json_output"] = json!({
            "type": "object",
            "description": "Parsed JSON output (when structured output is available)",
        });
    }

    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{HelpFormat, StructuredOutputInfo};

    fn make_flag(
        long_name: Option<&str>,
        description: &str,
        value_type: ValueType,
        required: bool,
        default: Option<&str>,
        enum_values: Option<Vec<String>>,
        repeatable: bool,
    ) -> ScannedFlag {
        ScannedFlag {
            long_name: long_name.map(|s| s.to_string()),
            short_name: None,
            description: description.to_string(),
            value_type,
            required,
            default: default.map(|s| s.to_string()),
            enum_values,
            repeatable,
            value_name: None,
        }
    }

    fn make_command(flags: Vec<ScannedFlag>, args: Vec<ScannedArg>) -> ScannedCommand {
        ScannedCommand {
            name: "test".to_string(),
            full_command: "tool test".to_string(),
            description: "A test command".to_string(),
            flags,
            positional_args: args,
            subcommands: vec![],
            examples: vec![],
            help_format: HelpFormat::Gnu,
            structured_output: StructuredOutputInfo::default(),
            raw_help: String::new(),
        }
    }

    #[test]
    fn test_schema_string_flag() {
        let flag = make_flag(
            Some("--output"),
            "Output file",
            ValueType::String,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["output"]["type"], "string");
        assert_eq!(schema["properties"]["output"]["description"], "Output file");
    }

    #[test]
    fn test_schema_boolean_flag() {
        let flag = make_flag(
            Some("--verbose"),
            "Enable verbose",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["verbose"]["type"], "boolean");
        assert_eq!(schema["properties"]["verbose"]["default"], false);
    }

    #[test]
    fn test_schema_enum_flag() {
        let flag = make_flag(
            Some("--format"),
            "Output format",
            ValueType::Enum,
            false,
            None,
            Some(vec!["json".to_string(), "text".to_string()]),
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["format"]["type"], "string");
        let enum_vals = schema["properties"]["format"]["enum"].as_array().unwrap();
        assert_eq!(enum_vals, &[json!("json"), json!("text")]);
    }

    #[test]
    fn test_schema_required_flag() {
        let flag = make_flag(
            Some("--name"),
            "The name",
            ValueType::String,
            true,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("name")));
    }

    #[test]
    fn test_schema_repeatable_flag() {
        let flag = make_flag(
            Some("--include"),
            "Include pattern",
            ValueType::String,
            false,
            None,
            None,
            true,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["include"]["type"], "array");
        assert_eq!(schema["properties"]["include"]["items"]["type"], "string");
    }

    #[test]
    fn test_schema_positional_arg() {
        let arg = ScannedArg {
            name: "file".to_string(),
            description: "Input file".to_string(),
            value_type: ValueType::Path,
            required: true,
            variadic: false,
        };
        let cmd = make_command(vec![], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["file"]["type"], "string");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("file")));
    }

    #[test]
    fn test_schema_variadic_arg() {
        let arg = ScannedArg {
            name: "files".to_string(),
            description: "Input files".to_string(),
            value_type: ValueType::String,
            required: false,
            variadic: true,
        };
        let cmd = make_command(vec![], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["files"]["type"], "array");
        assert_eq!(schema["properties"]["files"]["items"]["type"], "string");
    }

    #[test]
    fn test_schema_global_flags_included() {
        let cmd_flag = make_flag(
            Some("--local"),
            "Local flag",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        let global_flag = make_flag(
            Some("--verbose"),
            "Global verbose",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        // Global flag with same name as command flag should be skipped.
        let global_collision = make_flag(
            Some("--local"),
            "Global local",
            ValueType::String,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![cmd_flag], vec![]);
        let schema = build_input_schema(&cmd, &[global_flag, global_collision]);

        // Global --verbose should be included.
        assert_eq!(schema["properties"]["verbose"]["type"], "boolean");
        // --local should be the command version (boolean), not the global one (string).
        assert_eq!(schema["properties"]["local"]["type"], "boolean");
    }

    #[test]
    fn test_schema_output_json() {
        let mut cmd = make_command(vec![], vec![]);
        cmd.structured_output = StructuredOutputInfo {
            supported: true,
            flag: Some("--json".to_string()),
            format: Some("json".to_string()),
        };
        let schema = build_output_schema(&cmd);

        assert_eq!(schema["properties"]["json_output"]["type"], "object");
        assert_eq!(schema["properties"]["stdout"]["type"], "string");
    }

    #[test]
    fn test_schema_output_raw() {
        let cmd = make_command(vec![], vec![]);
        let schema = build_output_schema(&cmd);

        assert_eq!(schema["properties"]["stdout"]["type"], "string");
        assert_eq!(schema["properties"]["stderr"]["type"], "string");
        assert_eq!(schema["properties"]["exit_code"]["type"], "integer");
        assert!(schema["properties"]["json_output"].is_null());
    }

    #[test]
    fn test_schema_path_flag_has_format() {
        let flag = make_flag(
            Some("--config"),
            "Config file",
            ValueType::Path,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);
        assert_eq!(schema["properties"]["config"]["type"], "string");
        assert_eq!(schema["properties"]["config"]["format"], "path");
    }

    #[test]
    fn test_schema_url_flag_has_format() {
        let flag = make_flag(
            Some("--url"),
            "Remote URL",
            ValueType::Url,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);
        assert_eq!(schema["properties"]["url"]["type"], "string");
        assert_eq!(schema["properties"]["url"]["format"], "uri");
    }
}
