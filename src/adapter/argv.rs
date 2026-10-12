//! Strict native argv resolution. Execution always rebuilds argv from JSON.
use apcore::{ErrorCode, ModuleError};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Property<'a> {
    name: &'a str,
    schema: &'a Value,
}

#[derive(Clone, Copy)]
enum FlagPosition {
    Root,
    BeforeCommand,
    AfterCommand,
}

struct Parser<'a> {
    schema: &'a Value,
    flags: BTreeMap<String, Property<'a>>,
    operands: Vec<Property<'a>>,
}

fn invalid(message: impl Into<String>) -> ModuleError {
    ModuleError::new(ErrorCode::GeneralInvalidInput, message)
        .with_ai_guidance("Re-run apexe scan with a reviewed authoritative overlay covering this command's grammar; unresolved tokens are never forwarded.")
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
pub fn resolve_argv(argv: &[String], schema: &Value) -> Result<Value, ModuleError> {
    resolve_command_argv(argv, schema, &[])
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
pub(crate) fn resolve_command_argv(
    argv: &[String],
    schema: &Value,
    command: &[String],
) -> Result<Value, ModuleError> {
    Parser::new(schema)?.parse(argv, command)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlagArity {
    Boolean,
    Required,
    Optional,
}

/// Locate command tokens using shared global option arity, without validating
/// values or granting those options to the selected module.
pub(crate) struct CommandMatcher {
    flags: BTreeMap<String, FlagArity>,
    short_flag_bundling: bool,
}

impl CommandMatcher {
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    pub(crate) fn new(schemas: &[(&Value, bool)]) -> Result<Self, ModuleError> {
        let mut matcher = Self {
            flags: BTreeMap::new(),
            short_flag_bundling: false,
        };
        for (schema, root) in schemas {
            matcher.short_flag_bundling |= schema["x-apexe-short-flag-bundling"] == true;
            let properties = schema["properties"]
                .as_object()
                .ok_or_else(|| invalid("Native argv requires declared properties"))?;
            for (name, value) in properties {
                if value["x-apexe-positional"].as_u64().is_some()
                    || !root && value["x-apexe-flag-position"] != "before-subcommand"
                {
                    continue;
                }
                let arity = if boolean_only(value) {
                    FlagArity::Boolean
                } else if value["x-apexe-value-optional"] == true {
                    FlagArity::Optional
                } else {
                    FlagArity::Required
                };
                for literal in declared_flags(Property {
                    name,
                    schema: value,
                })? {
                    if matcher
                        .flags
                        .insert(literal.clone(), arity)
                        .is_some_and(|previous| previous != arity)
                    {
                        return Err(invalid(format!(
                            "Ambiguous global flag arity for '{literal}'"
                        )));
                    }
                }
            }
        }
        Ok(matcher)
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    pub(crate) fn matches(&self, argv: &[String], command: &[String]) -> Result<bool, ModuleError> {
        let mut cursor = 0;
        let mut matched = 0;
        while matched < command.len() {
            let Some(token) = argv.get(cursor) else {
                return Ok(false);
            };
            cursor += 1;
            if token == "--" {
                return Err(invalid(
                    "End-of-options before a declared subcommand is ambiguous",
                ));
            } else if token.starts_with('-') && token != "-" {
                self.skip_flag(token, argv, &mut cursor)?;
            } else if token == &command[matched] {
                matched += 1;
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn skip_flag(
        &self,
        token: &str,
        argv: &[String],
        cursor: &mut usize,
    ) -> Result<(), ModuleError> {
        let (literal, attached) = if token.starts_with("--") {
            token
                .split_once('=')
                .map_or((token, false), |(name, _)| (name, true))
        } else {
            (token, false)
        };
        if let Some(arity) = self.flags.get(literal) {
            return skip_value(*arity, attached, argv, cursor);
        }
        if token.starts_with("--") || !self.short_flag_bundling {
            return Err(invalid(format!(
                "Undeclared flag '{token}' or undeclared short-flag bundling"
            )));
        }
        for (offset, character) in token[1..].char_indices() {
            let literal = format!("-{character}");
            let arity = self
                .flags
                .get(&literal)
                .ok_or_else(|| invalid(format!("Undeclared flag '{literal}' in '{token}'")))?;
            let attached = 1 + offset + character.len_utf8() < token.len();
            if *arity != FlagArity::Boolean {
                return skip_value(*arity, attached, argv, cursor);
            }
        }
        Ok(())
    }
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
fn skip_value(
    arity: FlagArity,
    attached: bool,
    argv: &[String],
    cursor: &mut usize,
) -> Result<(), ModuleError> {
    if arity == FlagArity::Required && !attached {
        if argv.get(*cursor).is_none() {
            return Err(invalid("Global flag requires a value"));
        }
        *cursor += 1;
    }
    Ok(())
}

impl<'a> Parser<'a> {
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn new(schema: &'a Value) -> Result<Self, ModuleError> {
        if schema["x-apexe-cross-flag-ordering"] == true {
            return Err(invalid(
                "Cross-flag ordering cannot be represented by a JSON object",
            ));
        }
        let properties = schema["properties"].as_object().ok_or_else(|| {
            invalid("Native argv requires an object schema with declared properties")
        })?;
        let mut parser = Self {
            schema,
            flags: BTreeMap::new(),
            operands: vec![],
        };
        for (name, value) in properties {
            let property = Property {
                name,
                schema: value,
            };
            if value["x-apexe-positional"].as_u64().is_some() {
                parser.operands.push(property);
                continue;
            }
            let names = declared_flags(property)?;
            for literal in names {
                if let Some(previous) = parser.flags.insert(literal.clone(), property) {
                    if previous.name != name {
                        return Err(invalid(format!("Flag '{literal}' names two properties")));
                    }
                }
            }
        }
        parser
            .operands
            .sort_by_key(|property| property.schema["x-apexe-positional"].as_u64());
        parser.validate_operands()?;
        Ok(parser)
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn validate_operands(&self) -> Result<(), ModuleError> {
        if self.operands.windows(2).any(|pair| {
            pair[0].schema["x-apexe-positional"] == pair[1].schema["x-apexe-positional"]
        }) || self
            .operands
            .iter()
            .filter(|property| property.schema["type"] == "array")
            .count()
            > 1
        {
            return Err(invalid("Ambiguous declared positional grammar"));
        }
        Ok(())
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn parse(&self, argv: &[String], command: &[String]) -> Result<Value, ModuleError> {
        let mut input = Map::new();
        let mut operands = Vec::new();
        let mut cursor = 0;
        let mut command_cursor = 0;
        let mut ended = false;
        while cursor < argv.len() {
            let token = &argv[cursor];
            cursor += 1;
            if !ended && token == "--" {
                if self.schema["x-apexe-end-of-options"] != true || command_cursor < command.len() {
                    return Err(invalid("This command does not declare -- at this position"));
                }
                ended = true;
            } else if !ended && token.starts_with('-') && token != "-" {
                self.parse_flag(
                    token,
                    argv,
                    &mut cursor,
                    &mut input,
                    if command.is_empty() {
                        FlagPosition::Root
                    } else if command_cursor < command.len() {
                        FlagPosition::BeforeCommand
                    } else {
                        FlagPosition::AfterCommand
                    },
                )?;
            } else if command_cursor < command.len() {
                if token != &command[command_cursor] {
                    return Err(invalid(
                        "Subcommand path does not match command_path metadata",
                    ));
                }
                command_cursor += 1;
            } else {
                operands.push(token.as_str());
            }
        }
        if command_cursor != command.len() {
            return Err(invalid("Missing declared subcommand"));
        }
        self.assign_operands(&operands, &mut input)?;
        Ok(Value::Object(input))
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn parse_flag(
        &self,
        token: &str,
        argv: &[String],
        cursor: &mut usize,
        input: &mut Map<String, Value>,
        position: FlagPosition,
    ) -> Result<(), ModuleError> {
        let (literal, attached) = if token.starts_with("--") {
            token
                .split_once('=')
                .map_or((token, None), |(name, value)| (name, Some(value)))
        } else {
            (token, None)
        };
        if let Some(property) = self.flags.get(literal) {
            return self.consume(*property, attached, argv, cursor, input, position);
        }
        if token.starts_with("--") || self.schema["x-apexe-short-flag-bundling"] != true {
            return Err(invalid(format!(
                "Undeclared flag '{token}' or undeclared short-flag bundling"
            )));
        }
        for (offset, character) in token[1..].char_indices() {
            let literal = format!("-{character}");
            let property = self
                .flags
                .get(&literal)
                .ok_or_else(|| invalid(format!("Undeclared flag '{literal}' in '{token}'")))?;
            let tail = &token[1 + offset + character.len_utf8()..];
            let value_option = !boolean_only(property.schema);
            let attached = (value_option && !tail.is_empty()).then_some(tail);
            self.consume(*property, attached, argv, cursor, input, position)?;
            if value_option {
                break;
            }
        }
        Ok(())
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn consume(
        &self,
        property: Property<'_>,
        attached: Option<&str>,
        argv: &[String],
        cursor: &mut usize,
        input: &mut Map<String, Value>,
        position: FlagPosition,
    ) -> Result<(), ModuleError> {
        let global = property.schema["x-apexe-flag-position"] == "before-subcommand";
        if matches!(position, FlagPosition::BeforeCommand) && !global
            || matches!(position, FlagPosition::AfterCommand) && global
        {
            return Err(invalid(format!(
                "Flag '{}' is on the wrong side of the subcommand",
                property.name
            )));
        }
        let value = if let Some(text) = attached {
            if boolean_only(property.schema) {
                return Err(invalid(format!(
                    "Boolean flag '{}' takes no value",
                    property.name
                )));
            }
            parse_value(text, property)?
        } else if boolean_only(property.schema) || property.schema["x-apexe-value-optional"] == true
        {
            Value::Bool(true)
        } else {
            let text = argv
                .get(*cursor)
                .ok_or_else(|| invalid(format!("Flag '{}' requires a value", property.name)))?;
            *cursor += 1;
            parse_value(text, property)?
        };
        if property.schema["type"] == "array" {
            let values = input
                .entry(property.name)
                .or_insert_with(|| Value::Array(vec![]));
            values
                .as_array_mut()
                .ok_or_else(|| invalid("Invalid array property"))?
                .push(value);
        } else {
            input.insert(property.name.into(), value);
        }
        Ok(())
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn assign_operands(
        &self,
        values: &[&str],
        input: &mut Map<String, Value>,
    ) -> Result<(), ModuleError> {
        let mut cursor = 0;
        let mut variadic_seen = false;
        for (index, property) in self.operands.iter().enumerate() {
            let available = values.len().saturating_sub(cursor);
            if property.schema["type"] == "array" {
                if variadic_seen && available > 0 {
                    return Err(invalid(
                        "Multiple variadic operands have an ambiguous native grammar",
                    ));
                }
                variadic_seen = true;
                let reserved = self.operands[index + 1..]
                    .iter()
                    .filter(|property| {
                        property.schema["type"] != "array" && self.required(property.name)
                    })
                    .count();
                let count = available.saturating_sub(reserved);
                if count > 0 {
                    let items = values[cursor..cursor + count]
                        .iter()
                        .map(|value| parse_value(value, *property))
                        .collect::<Result<Vec<_>, _>>()?;
                    input.insert(property.name.into(), Value::Array(items));
                    cursor += count;
                }
            } else if available > 0 {
                input.insert(
                    property.name.into(),
                    parse_value(values[cursor], *property)?,
                );
                cursor += 1;
            }
        }
        if cursor != values.len() {
            return Err(invalid(format!("Unexpected operand '{}'", values[cursor])));
        }
        Ok(())
    }

    fn required(&self, name: &str) -> bool {
        self.schema["required"]
            .as_array()
            .is_some_and(|names| names.iter().any(|value| value == name))
    }
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
fn declared_flags(property: Property<'_>) -> Result<Vec<String>, ModuleError> {
    let value = property.schema;
    let name = property.name;
    let aliases = value["x-apexe-flag-aliases"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<String> = aliases
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if let Some(literal) = value["x-apexe-flag"].as_str() {
        names.push(literal.into());
    }
    if names.is_empty() {
        return Err(invalid(format!(
            "Property '{name}' has no declared flag spelling; rescan before installing shims"
        )));
    }
    Ok(names)
}

fn boolean_only(schema: &Value) -> bool {
    schema["type"] == "boolean"
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
fn parse_value(text: &str, property: Property<'_>) -> Result<Value, ModuleError> {
    let schema = if property.schema["type"] == "array" {
        &property.schema["items"]
    } else {
        property.schema
    };
    let types: Vec<&str> = match &schema["type"] {
        Value::String(kind) => vec![kind],
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let value = if types.contains(&"integer") {
        text.parse::<i64>()
            .ok()
            .map(|number| Value::Number(number.into()))
    } else if types.contains(&"number") {
        text.parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
    } else if types == ["boolean"] {
        text.parse::<bool>().ok().map(Value::Bool)
    } else if types.contains(&"string") {
        Some(Value::String(text.into()))
    } else {
        None
    };
    value.ok_or_else(|| {
        invalid(format!(
            "Value for '{}' cannot be represented by its declared type",
            property.name
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }
    fn schema() -> Value {
        json!({"type":"object","x-apexe-short-flag-bundling":true,"x-apexe-end-of-options":true,"properties":{
            "all":{"type":"boolean","x-apexe-flag":"--all","x-apexe-flag-aliases":["-a","--all"]},
            "long":{"type":"boolean","x-apexe-flag":"-l"},
            "count":{"type":"integer","x-apexe-flag":"--count","x-apexe-flag-aliases":["-n"]},
            "patterns":{"type":"array","items":{"type":"string"},"x-apexe-flag":"-e"},
            "file":{"type":"array","items":{"type":"string"},"x-apexe-positional":0}
        }})
    }
    #[test]
    fn test_resolve_argv_bundles_and_attached_short_values() {
        assert_eq!(
            resolve_argv(&args(&["-lan20", "file"]), &schema()).unwrap(),
            json!({"all":true,"long":true,"count":20,"file":["file"]})
        );
    }
    #[test]
    fn test_resolve_argv_last_wins_and_array_order() {
        assert_eq!(
            resolve_argv(
                &args(&["--count=2", "-n", "3", "-e", "A", "-eB"]),
                &schema()
            )
            .unwrap(),
            json!({"count":3,"patterns":["A","B"]})
        );
    }
    #[test]
    fn test_resolve_argv_end_of_options_preserves_dash_operand() {
        assert_eq!(
            resolve_argv(&args(&["--", "-rf"]), &schema()).unwrap(),
            json!({"file":["-rf"]})
        );
    }
    #[test]
    fn test_resolve_argv_refuses_unknown_and_undeclared_bundling() {
        assert!(resolve_argv(&args(&["--unknown"]), &schema()).is_err());
        let mut schema = schema();
        schema["x-apexe-short-flag-bundling"] = json!(false);
        assert!(resolve_argv(&args(&["-la"]), &schema).is_err());
        assert!(resolve_argv(&args(&["-l", "-a"]), &schema).is_ok());
    }
    #[test]
    fn test_resolve_argv_refuses_cross_flag_ordering() {
        let mut schema = schema();
        schema["x-apexe-cross-flag-ordering"] = json!(true);
        assert!(resolve_argv(&[], &schema).is_err());
    }
    #[test]
    fn test_resolve_argv_reserves_required_final_operand() {
        let schema = json!({"type":"object","properties":{"sources":{"type":"array","items":{"type":"string"},"x-apexe-positional":0},"target":{"type":"string","x-apexe-positional":1}},"required":["sources","target"]});
        assert_eq!(
            resolve_argv(&args(&["a", "b", "c"]), &schema).unwrap(),
            json!({"sources":["a","b"],"target":"c"})
        );
    }
    #[test]
    fn test_resolve_argv_refuses_ambiguous_positional_schema() {
        for position in [0, 1] {
            let schema = json!({"type":"object","properties":{
                "left":{"type":"array","items":{"type":"string"},"x-apexe-positional":0},
                "right":{"type":"array","items":{"type":"string"},"x-apexe-positional":position}
            }});
            assert!(resolve_argv(&args(&["a", "b"]), &schema).is_err());
        }
    }
    #[test]
    fn test_resolve_argv_routes_original_hyphenated_subcommand_with_global_option() {
        let schema = json!({"type":"object","properties":{"directory":{"type":"string","x-apexe-flag":"-C","x-apexe-flag-position":"before-subcommand"},"file":{"type":"string","x-apexe-positional":0}}});
        assert_eq!(
            resolve_command_argv(
                &args(&["-C", "tmp", "cat-file", "object"]),
                &schema,
                &args(&["cat-file"])
            )
            .unwrap(),
            json!({"directory":"tmp","file":"object"})
        );
    }

    #[test]
    fn test_command_matcher_skips_bundled_and_attached_global_values() {
        let schema = json!({"type":"object","x-apexe-short-flag-bundling":true,"properties":{
            "verbose":{"type":"boolean","x-apexe-flag":"-v","x-apexe-flag-position":"before-subcommand"},
            "directory":{"type":"string","x-apexe-flag":"-C","x-apexe-flag-position":"before-subcommand"}
        }});
        let matcher = CommandMatcher::new(&[(&schema, false)]).unwrap();
        for argv in [
            args(&["-vCdanger", "danger"]),
            args(&["-vC", "danger", "danger"]),
        ] {
            assert!(matcher.matches(&argv, &args(&["danger"])).unwrap());
        }
        assert!(!matcher
            .matches(&args(&["-Cdanger"]), &args(&["danger"]))
            .unwrap());
        assert!(matcher.matches(&args(&["-C"]), &args(&["danger"])).is_err());
    }

    #[test]
    fn test_command_matcher_defers_global_value_validation_to_selected_route() {
        let schema = json!({"type":"object","properties":{
            "count":{"type":"integer","x-apexe-flag":"--count","x-apexe-flag-position":"before-subcommand"}
        }});
        let matcher = CommandMatcher::new(&[(&schema, false)]).unwrap();
        let argv = args(&["--count", "invalid", "danger"]);
        let command = args(&["danger"]);
        assert!(matcher.matches(&argv, &command).unwrap());
        assert!(resolve_command_argv(&argv, &schema, &command).is_err());
    }

    #[test]
    fn test_command_matcher_optional_values_do_not_consume_command_tokens() {
        let schema = json!({"type":"object","properties":{
            "color":{"type":["string","boolean"],"x-apexe-value-optional":true,"x-apexe-flag":"--color"}
        }});
        let matcher = CommandMatcher::new(&[(&schema, true)]).unwrap();
        assert!(matcher
            .matches(&args(&["--color", "danger"]), &args(&["danger"]))
            .unwrap());
        assert!(!matcher
            .matches(&args(&["--color=danger"]), &args(&["danger"]))
            .unwrap());
    }

    #[test]
    fn test_command_matcher_refuses_ambiguous_global_flag_arity() {
        let root = json!({"type":"object","properties":{
            "shared":{"type":"boolean","x-apexe-flag":"--shared"}
        }});
        let leaf = json!({"type":"object","properties":{
            "shared":{"type":"string","x-apexe-flag":"--shared","x-apexe-flag-position":"before-subcommand"}
        }});
        assert!(CommandMatcher::new(&[(&root, true), (&leaf, false)]).is_err());
    }
}
