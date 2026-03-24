use serde_json::{json, Value as JsonValue};

use crate::models::{ScannedArg, ScannedCommand, ScannedFlag, StructuredOutputInfo, ValueType};

/// Type mapping: ValueType -> JSON Schema type string.
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

/// Generates JSON Schema dicts from ScannedCommand data.
pub struct SchemaGenerator;

impl SchemaGenerator {
    /// Generate JSON Schema for command inputs (flags + positional args).
    pub fn generate_input_schema(&self, command: &ScannedCommand) -> JsonValue {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();

        for flag in &command.flags {
            let prop_name = flag.canonical_name();
            let prop_schema = self.flag_to_schema(flag);
            properties.insert(prop_name.clone(), prop_schema);
            if flag.required {
                required.push(prop_name);
            }
        }

        for arg in &command.positional_args {
            let prop_name = arg.name.to_lowercase().replace('-', "_");
            let prop_schema = self.arg_to_schema(arg);
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

    /// Convert a ScannedFlag to a JSON Schema property.
    fn flag_to_schema(&self, flag: &ScannedFlag) -> JsonValue {
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

        if !flag.description.is_empty() {
            schema["description"] = json!(flag.description);
        }

        if let Some(ref default) = flag.default {
            // Coerce default to proper type
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

        if let Some(ref enum_values) = flag.enum_values {
            schema["enum"] = json!(enum_values);
        }

        schema
    }

    /// Convert a ScannedArg to a JSON Schema property.
    fn arg_to_schema(&self, arg: &ScannedArg) -> JsonValue {
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
            if !arg.description.is_empty() {
                schema["description"] = json!(arg.description);
            }
            schema
        }
    }

    /// Generate JSON Schema for command output.
    pub fn generate_output_schema(&self, structured_output: &StructuredOutputInfo) -> JsonValue {
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

        if structured_output.supported {
            schema["properties"]["json_output"] = json!({
                "type": "object",
                "description": "Parsed JSON output (when structured output is available)",
            });
        }

        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::HelpFormat;

    #[allow(clippy::too_many_arguments)]
    fn make_flag(
        long_name: Option<&str>,
        short_name: Option<&str>,
        description: &str,
        value_type: ValueType,
        required: bool,
        default: Option<&str>,
        enum_values: Option<Vec<String>>,
        repeatable: bool,
    ) -> ScannedFlag {
        ScannedFlag {
            long_name: long_name.map(|s| s.to_string()),
            short_name: short_name.map(|s| s.to_string()),
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
    fn test_string_required_flag() {
        let flag = make_flag(
            Some("--message"),
            Some("-m"),
            "Commit message",
            ValueType::String,
            true,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["message"]["type"], "string");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("message")));
    }

    #[test]
    fn test_boolean_flag_default_false() {
        let flag = make_flag(
            Some("--all"),
            None,
            "All files",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["all"]["type"], "boolean");
        assert_eq!(schema["properties"]["all"]["default"], false);
    }

    #[test]
    fn test_integer_flag_with_default() {
        let flag = make_flag(
            Some("--count"),
            None,
            "Number of items",
            ValueType::Integer,
            false,
            Some("10"),
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["count"]["type"], "integer");
        assert_eq!(schema["properties"]["count"]["default"], 10);
    }

    #[test]
    fn test_float_flag_with_default() {
        let flag = make_flag(
            Some("--ratio"),
            None,
            "Ratio value",
            ValueType::Float,
            false,
            Some("0.5"),
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["ratio"]["type"], "number");
        assert_eq!(schema["properties"]["ratio"]["default"], 0.5);
    }

    #[test]
    fn test_enum_flag() {
        let flag = make_flag(
            Some("--format"),
            None,
            "Output format",
            ValueType::Enum,
            false,
            None,
            Some(vec!["json".to_string(), "text".to_string()]),
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["format"]["type"], "string");
        let enum_vals = schema["properties"]["format"]["enum"].as_array().unwrap();
        assert_eq!(enum_vals, &[json!("json"), json!("text")]);
    }

    #[test]
    fn test_repeatable_flag() {
        let flag = make_flag(
            Some("--include"),
            None,
            "Include pattern",
            ValueType::String,
            false,
            None,
            None,
            true,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["include"]["type"], "array");
        assert_eq!(schema["properties"]["include"]["items"]["type"], "string");
    }

    #[test]
    fn test_path_flag() {
        let flag = make_flag(
            Some("--config"),
            None,
            "Config file",
            ValueType::Path,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["config"]["type"], "string");
    }

    #[test]
    fn test_positional_required_arg() {
        let arg = ScannedArg {
            name: "file".to_string(),
            description: "Input file".to_string(),
            value_type: ValueType::Path,
            required: true,
            variadic: false,
        };
        let cmd = make_command(vec![], vec![arg]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["file"]["type"], "string");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("file")));
    }

    #[test]
    fn test_variadic_positional_arg() {
        let arg = ScannedArg {
            name: "files".to_string(),
            description: "Input files".to_string(),
            value_type: ValueType::String,
            required: false,
            variadic: true,
        };
        let cmd = make_command(vec![], vec![arg]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["properties"]["files"]["type"], "array");
        assert_eq!(schema["properties"]["files"]["items"]["type"], "string");
    }

    #[test]
    fn test_output_schema_without_json() {
        let gen = SchemaGenerator;
        let info = StructuredOutputInfo::default();
        let schema = gen.generate_output_schema(&info);

        assert_eq!(schema["properties"]["stdout"]["type"], "string");
        assert_eq!(schema["properties"]["stderr"]["type"], "string");
        assert_eq!(schema["properties"]["exit_code"]["type"], "integer");
        assert!(schema["properties"]["json_output"].is_null());
    }

    #[test]
    fn test_output_schema_with_json() {
        let gen = SchemaGenerator;
        let info = StructuredOutputInfo {
            supported: true,
            flag: Some("--json".to_string()),
            format: Some("json".to_string()),
        };
        let schema = gen.generate_output_schema(&info);

        assert_eq!(schema["properties"]["json_output"]["type"], "object");
    }

    #[test]
    fn test_no_required_when_all_optional() {
        let flag = make_flag(
            Some("--verbose"),
            None,
            "",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert!(schema.get("required").is_none() || schema["required"].is_null());
    }

    #[test]
    fn test_additional_properties_false() {
        let cmd = make_command(vec![], vec![]);
        let gen = SchemaGenerator;
        let schema = gen.generate_input_schema(&cmd);

        assert_eq!(schema["additionalProperties"], false);
    }
}
