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

/// Annotate a property with the flag's "may not terminate on its own" warning.
///
/// Emitted as a JSON Schema extension keyword (`x-` prefix), which validators
/// ignore, because it is not a constraint on the *value*: `follow: true` is a
/// perfectly valid input. It tells an executor that accepting it may mean the
/// process never exits, so the invocation needs a bounded timeout or a refusal.
/// The claim is possibility, not certainty — BSD `tail -f` returns immediately
/// when its input is a pipe.
fn apply_long_running(schema: &mut JsonValue, flag: &ScannedFlag) {
    if flag.long_running {
        schema["x-apexe-long-running"] = json!(true);
    }
}

/// Record the literal token this flag is spelled with on a command line.
///
/// A property key cannot be turned back into a flag by rule. The key is
/// derived by [`ScannedFlag::canonical_name`], which strips leading dashes and
/// folds `-` to `_` — a lossy mapping whose inverse is ambiguous in three ways
/// that all occur in practice:
///
/// - `-l` and `--l` both yield the key `l`, and 45 of `ls`'s 47 properties are
///   single-character short options that must be spelled with one dash.
/// - BSD/GNU `find` spells multi-character options with a single dash
///   (`-daystart`, `-regextype`); no "one character means one dash" heuristic
///   recovers those.
/// - A long flag containing an underscore round-trips to a hyphen.
///
/// Emitting the literal removes the guesswork: the renderer reproduces what the
/// scan actually saw. Long form is preferred over short, matching
/// `canonical_name`, so the key and the literal always describe the same flag.
fn apply_flag_literal(schema: &mut JsonValue, flag: &ScannedFlag) {
    if let Some(literal) = flag.long_name.as_deref().or(flag.short_name.as_deref()) {
        schema["x-apexe-flag"] = json!(literal);
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
        apply_long_running(&mut schema, flag);
        apply_flag_literal(&mut schema, flag);
        return schema;
    }

    // An optional-value flag has two legal spellings and the contract has to
    // admit both, or one of them is unreachable: `git --exec-path` prints the
    // exec path while `git --exec-path=<p>` sets it. `true` selects the bare
    // form and a string supplies a value; the executor already renders long
    // options as `--flag=value`, which is the spelling an optional value
    // requires.
    let mut schema = if flag.value_optional {
        json!({ "type": [base_type, "boolean"], "x-apexe-value-optional": true })
    } else {
        json!({ "type": base_type })
    };

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

    apply_long_running(&mut schema, flag);
    apply_flag_literal(&mut schema, flag);

    schema
}

/// Convert a ScannedArg into a JSON Schema property value.
///
/// `index` is the argument's position in the command's `positional_args`, and
/// is recorded as `x-apexe-positional`. It carries two facts the property name
/// alone cannot: that the value is passed bare rather than behind a flag, and
/// what order it goes in. Order is not recoverable at call time — an input
/// object has no inherent ordering, and `cp source target` inverts its meaning
/// if the two are swapped.
///
/// `x-apexe-operand-position` is emitted only for the minority of operands that
/// precede the flags. It is a separate keyword rather than a negative index
/// because the two orderings are independent: `find path ... [expression]` has
/// one operand on each side of the flags, and both still need their relative
/// order recorded.
fn arg_to_schema(arg: &ScannedArg, index: usize) -> JsonValue {
    let base_type = value_type_to_json_schema(arg.value_type);

    let mut schema = if arg.variadic {
        json!({
            "type": "array",
            "items": { "type": base_type },
        })
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
        schema
    };

    if !arg.description.is_empty() {
        schema["description"] = json!(arg.description);
    }
    schema["x-apexe-positional"] = json!(index);
    if arg.before_flags {
        schema["x-apexe-operand-position"] = json!("before-flags");
    }

    schema
}

/// Map each spelling of a flag (`-l`, `--long`) to its schema property name, so
/// a `conflicts_with` entry — which names flags the way the command line does —
/// can be rewritten into the property names a caller actually sends.
fn property_names_by_literal(
    command: &ScannedCommand,
    global_flags: &[ScannedFlag],
) -> std::collections::HashMap<String, String> {
    let mut by_literal = std::collections::HashMap::new();
    for flag in command.flags.iter().chain(global_flags) {
        let prop_name = flag.canonical_name();
        for literal in [flag.short_name.as_deref(), flag.long_name.as_deref()]
            .into_iter()
            .flatten()
        {
            by_literal
                .entry(literal.to_string())
                .or_insert_with(|| prop_name.clone());
        }
    }
    by_literal
}

/// Record the flags that must not be sent together with this one.
///
/// Translated from command-line spellings into property names, because that is
/// what a caller sends and what [`crate::module::executor`] checks. Entries
/// naming a flag this command does not declare are dropped rather than passed
/// through: the overlay loader already rejects those, so anything left here
/// would be a flag filtered out after validation, and a dangling name in the
/// contract is worse than a missing one.
fn apply_conflicts(
    schema: &mut JsonValue,
    flag: &ScannedFlag,
    by_literal: &std::collections::HashMap<String, String>,
) {
    let conflicts: Vec<String> = flag
        .conflicts_with
        .iter()
        .filter_map(|literal| by_literal.get(literal).cloned())
        .collect();
    if !conflicts.is_empty() {
        schema["x-apexe-conflicts-with"] = json!(conflicts);
    }
}

/// Build a JSON Schema for command inputs, merging command flags with global flags.
///
/// Command-level flags take precedence; global flags are included only when
/// their canonical name does not collide with a command-level flag.
pub fn build_input_schema(command: &ScannedCommand, global_flags: &[ScannedFlag]) -> JsonValue {
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    let by_literal = property_names_by_literal(command, global_flags);

    // Command flags first.
    for flag in &command.flags {
        let prop_name = flag.canonical_name();
        let mut prop_schema = flag_to_schema(flag);
        apply_conflicts(&mut prop_schema, flag, &by_literal);
        properties.insert(prop_name.clone(), prop_schema);
        if flag.required {
            required.push(prop_name);
        }
    }

    // Global flags, skipping collisions.
    for flag in global_flags {
        let prop_name = flag.canonical_name();
        if !properties.contains_key(&prop_name) {
            let mut prop_schema = flag_to_schema(flag);
            apply_conflicts(&mut prop_schema, flag, &by_literal);
            properties.insert(prop_name.clone(), prop_schema);
            if flag.required {
                required.push(prop_name);
            }
        }
    }

    // Positional args. A positional replaces a same-named flag rather than
    // being skipped: a name that appears in the usage line as an operand is
    // passed bare, and that is the form every tool accepts. `curl` is the case
    // in point -- `curl [options...] <url>` and a `--url` option both exist,
    // and only the operand form is universal.
    //
    // The flag's description is inherited when the positional has none, which
    // is the common shape: usage lines carry no prose, so the description only
    // exists on the flag. Overwriting outright used to discard it.
    for (index, arg) in command.positional_args.iter().enumerate() {
        let prop_name = arg.name.to_lowercase().replace('-', "_");
        let mut prop_schema = arg_to_schema(arg, index);

        if let Some(displaced) = properties.get(&prop_name) {
            if prop_schema.get("description").is_none() {
                if let Some(description) = displaced.get("description") {
                    prop_schema["description"] = description.clone();
                }
            }
        }

        properties.insert(prop_name.clone(), prop_schema);
        // A flag and a positional of the same name can both be required; the
        // property is one entry, so it must not appear in `required` twice.
        if arg.required && !required.contains(&prop_name) {
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

    // Spec §3.4: json_output is only meaningful when the structured format is
    // actually JSON. A tool that emits structured CSV/XML/table output must not
    // advertise a `json_output` object (the executor only parses JSON stdout).
    let is_json = command.structured_output.supported
        && command
            .structured_output
            .format
            .as_deref()
            .is_some_and(|f| f.eq_ignore_ascii_case("json"));
    if is_json {
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
            ..Default::default()
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
    fn test_schema_long_running_flag_is_annotated() {
        // `tail -f`: the executor cannot learn this from the flag's type, and
        // prose in the description is not actionable.
        let mut flag = make_flag(
            Some("--follow"),
            "Output appended data as the file grows.",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        flag.long_running = true;
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["follow"]["x-apexe-long-running"], true);
    }

    #[test]
    fn test_schema_long_running_survives_repeatable_branch() {
        // Repeatable flags return early from flag_to_schema, so the annotation
        // has to be applied on that path too.
        let mut flag = make_flag(
            Some("--watch"),
            "Keep watching.",
            ValueType::String,
            false,
            None,
            None,
            true,
        );
        flag.long_running = true;
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["watch"]["type"], "array");
        assert_eq!(schema["properties"]["watch"]["x-apexe-long-running"], true);
    }

    #[test]
    fn test_schema_omits_long_running_when_unset() {
        // Absent, not `false`: an ordinary flag makes no claim either way.
        let flag = make_flag(
            Some("--lines"),
            "Number of lines.",
            ValueType::Integer,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert!(schema["properties"]["lines"]["x-apexe-long-running"].is_null());
    }

    #[test]
    fn test_schema_positional_arg() {
        let arg = ScannedArg {
            name: "file".to_string(),
            description: "Input file".to_string(),
            value_type: ValueType::Path,
            required: true,
            variadic: false,
            before_flags: false,
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
            before_flags: false,
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
    fn test_schema_output_structured_non_json_has_no_json_output() {
        // Spec §3.4: a tool with structured but non-JSON output (e.g. csv) must
        // NOT advertise a json_output property.
        let mut cmd = make_command(vec![], vec![]);
        cmd.structured_output = StructuredOutputInfo {
            supported: true,
            flag: Some("--format".to_string()),
            format: Some("csv".to_string()),
        };
        let schema = build_output_schema(&cmd);

        assert!(schema["properties"]["json_output"].is_null());
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
