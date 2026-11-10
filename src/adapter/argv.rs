//! Strict native argv resolution. Execution always rebuilds argv from JSON.
use apcore::{ErrorCode, ModuleError};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Property<'a> {
    name: &'a str,
    schema: &'a Value,
}

impl Property<'_> {
    fn is_path(&self) -> bool {
        self.schema["x-apexe-path"] == true || self.schema["items"]["x-apexe-path"] == true
    }
}

#[derive(Clone, Copy)]
enum FlagPosition {
    Root,
    BeforeCommand,
    AfterCommand,
}

struct Parser<'a> {
    schema: &'a Value,
    /// Flags usable after the declared subcommand path, and the only flags a
    /// root-level module has.
    flags: BTreeMap<String, Property<'a>>,
    /// Flags that may only precede the subcommand (tool-level options).
    ///
    /// Kept in a second map because a real tool reuses the same spelling on both
    /// sides: `git -C <dir> diff -C` is one command in which `-C` means two
    /// different things, and `git -v` is the version while `git status -v` is
    /// verbose. One flat map turned every such tool into "Flag '-v' names two
    /// properties" for every call, which is the tool's grammar being rejected,
    /// not the caller's arguments.
    global_flags: BTreeMap<String, Property<'a>>,
    operands: Vec<Property<'a>>,
    /// The one property a schema marks as taking a bare `-<digits>` value
    /// (`head -20`). At most one may declare it: two would make the token
    /// ambiguous, and the overlay is rejected rather than guessed at.
    digit_value: Option<Property<'a>>,
    /// Index (among operands) of the operand from which the rest of argv is
    /// forwarded verbatim. `None` when nothing declares it.
    opaque_operand: Option<usize>,
}

/// Whether a property describes a tool-level option, which must precede the
/// subcommand.
///
/// `before-operands` covers the options the scanner renders ahead of the
/// operands (`git -C <path> status`, `find -maxdepth 2 <paths>`); for a command
/// with subcommands those are tool-level options too.
fn is_global_property(schema: &Value) -> bool {
    matches!(
        schema["x-apexe-flag-position"].as_str(),
        Some("before-subcommand") | Some("before-operands")
    )
}

/// Whether the token is exactly a dash followed by one or more ASCII digits.
///
/// `-20` is the count shorthand of `head`/`tail`; `-2.5`, `-20x` and `--20` are
/// not, so they keep their normal meaning.
fn is_digit_run(token: &str) -> bool {
    token
        .strip_prefix('-')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

fn invalid(message: impl Into<String>) -> ModuleError {
    ModuleError::new(ErrorCode::GeneralInvalidInput, message)
        .with_ai_guidance("Re-run apexe scan with a reviewed authoritative overlay covering this command's grammar; unresolved tokens are never forwarded.")
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
pub fn resolve_argv(argv: &[String], schema: &Value) -> Result<Value, ModuleError> {
    resolve_command_argv(argv, schema, &[]).map(|resolved| resolved.input)
}

#[derive(Debug)]
pub(crate) struct ResolvedArgv {
    pub(crate) input: Value,
    pub(crate) opaque_tail: Option<OpaqueTail>,
}

#[derive(Clone, Debug)]
pub(crate) struct OpaqueTail {
    pub(crate) prefix: Map<String, Value>,
    pub(crate) argv: Vec<String>,
    pub(crate) separator: bool,
    pub(crate) trigger_value: Option<(String, bool)>,
    pub(crate) converted_paths: Value,
}

struct OpaqueBoundary<'a, 'b> {
    start: usize,
    preceding_operands: usize,
    separator: bool,
    flags: Vec<ParsedFlag<'a, 'b>>,
}

struct ParsedFlag<'a, 'b> {
    property: Property<'a>,
    raw_value: Option<&'b str>,
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
pub(crate) fn resolve_command_argv(
    argv: &[String],
    schema: &Value,
    command: &[String],
) -> Result<ResolvedArgv, ModuleError> {
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
    /// Arity of the one flag a schema marks as taking a bare `-<digits>` value
    /// (`head -20`). `None` means no schema here declares it, so a digit token
    /// stays a bundle.
    digit_value: Option<FlagArity>,
    opaque_digit_value: bool,
    /// Flag spellings that start an opaque tail (`python3 -m <module>`). Any
    /// token after one belongs to whatever the flag named.
    opaque_flags: std::collections::BTreeSet<String>,
}

impl CommandMatcher {
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    pub(crate) fn new(schemas: &[(&Value, bool)]) -> Result<Self, ModuleError> {
        let mut matcher = Self {
            flags: BTreeMap::new(),
            short_flag_bundling: false,
            digit_value: None,
            opaque_digit_value: false,
            opaque_flags: std::collections::BTreeSet::new(),
        };
        for (schema, root) in schemas {
            matcher.short_flag_bundling |= schema["x-apexe-short-flag-bundling"] == true;
            let properties = schema["properties"]
                .as_object()
                .ok_or_else(|| invalid("Native argv requires declared properties"))?;
            for (name, value) in properties {
                if value["x-apexe-positional"].as_u64().is_some()
                    || !root && !is_global_property(value)
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
                if value["x-apexe-digit-value"] == true {
                    matcher.digit_value = Some(arity);
                    matcher.opaque_digit_value |= value["x-apexe-opaque-after"] == true;
                }
                let opaque = value["x-apexe-opaque-after"] == true;
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
                    if opaque {
                        matcher.opaque_flags.insert(literal.clone());
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
                if self.skip_flag(token, argv, &mut cursor)? {
                    // Everything from here belongs to whatever the flag named,
                    // so the declared subcommand cannot appear after it.
                    return Ok(false);
                }
            } else if token == &command[matched] {
                matched += 1;
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    /// Skip one flag token. Returns whether it starts an opaque tail.
    fn skip_flag(
        &self,
        token: &str,
        argv: &[String],
        cursor: &mut usize,
    ) -> Result<bool, ModuleError> {
        let (literal, attached) = if token.starts_with("--") {
            token
                .split_once('=')
                .map_or((token, false), |(name, _)| (name, true))
        } else {
            (token, false)
        };
        if let Some(arity) = self.flags.get(literal) {
            skip_value(*arity, attached, argv, cursor)?;
            return Ok(self.opaque_flags.contains(literal));
        }
        if let Some(arity) = self.digit_value.filter(|_| is_digit_run(token)) {
            skip_value(arity, true, argv, cursor)?;
            return Ok(self.opaque_digit_value);
        }
        if token.starts_with("--") || !self.short_flag_bundling {
            return Err(invalid(format!(
                "Undeclared flag '{token}' or undeclared short-flag bundling"
            )));
        }
        let mut opaque = false;
        for (offset, character) in token[1..].char_indices() {
            let literal = format!("-{character}");
            let arity = self
                .flags
                .get(&literal)
                .ok_or_else(|| invalid(format!("Undeclared flag '{literal}' in '{token}'")))?;
            let attached = 1 + offset + character.len_utf8() < token.len();
            opaque |= self.opaque_flags.contains(&literal);
            if *arity != FlagArity::Boolean {
                skip_value(*arity, attached, argv, cursor)?;
                break;
            }
        }
        Ok(opaque)
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
            global_flags: BTreeMap::new(),
            operands: vec![],
            digit_value: None,
            opaque_operand: None,
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
            if value["x-apexe-digit-value"] == true {
                if let Some(previous) = parser.digit_value {
                    return Err(invalid(format!(
                        "The -<digits> shorthand is declared on two flags ('{}' and '{}'); \
                         the token would be ambiguous",
                        previous.name, name
                    )));
                }
                parser.digit_value = Some(property);
            }
            let target = if is_global_property(value) {
                &mut parser.global_flags
            } else {
                &mut parser.flags
            };
            for literal in declared_flags(property)? {
                if let Some(previous) = target.insert(literal.clone(), property) {
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
        parser.opaque_operand = parser.locate_opaque_operand()?;
        Ok(parser)
    }

    /// Find the operand that starts the opaque tail, if any.
    ///
    /// An interpreter stops parsing at its script: everything after
    /// `node <script>` belongs to the script, so validating it against `node`'s
    /// own flags refuses `node tsc.js --noEmit`. The declaration is only
    /// meaningful on the last operand, and that operand must be variadic —
    /// otherwise the tokens it covers have nowhere to go and the overlay would
    /// describe a grammar the parser cannot honour.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn locate_opaque_operand(&self) -> Result<Option<usize>, ModuleError> {
        let Some(index) = self
            .operands
            .iter()
            .position(|property| property.schema["x-apexe-opaque-after"] == true)
        else {
            return Ok(None);
        };
        let property = self.operands[index];
        if index + 1 != self.operands.len() || property.schema["type"] != "array" {
            return Err(invalid(format!(
                "'x-apexe-opaque-after' must sit on the last operand and that operand must be \
                 variadic; '{}' is not",
                property.name
            )));
        }
        Ok(Some(index))
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
    fn parse(&self, argv: &[String], command: &[String]) -> Result<ResolvedArgv, ModuleError> {
        let mut input = Map::new();
        let mut operands = Vec::new();
        let mut cursor = 0;
        let mut command_cursor = 0;
        let mut ended = false;
        let boundary = loop {
            let Some(token) = argv.get(cursor) else {
                break None;
            };
            let start = cursor;
            cursor += 1;
            if !ended && token == "--" {
                if self.schema["x-apexe-end-of-options"] != true || command_cursor < command.len() {
                    return Err(invalid("This command does not declare -- at this position"));
                }
                ended = true;
            } else if !ended && token.starts_with('-') && token != "-" {
                if let Some(flags) = self.parse_flag(
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
                )? {
                    if command_cursor < command.len() {
                        return Err(invalid("Opaque arguments cannot start before a subcommand"));
                    }
                    break Some(OpaqueBoundary {
                        start,
                        preceding_operands: operands.len(),
                        separator: false,
                        flags,
                    });
                }
            } else if command_cursor < command.len() {
                if token != &command[command_cursor] {
                    return Err(invalid(
                        "Subcommand path does not match command_path metadata",
                    ));
                }
                command_cursor += 1;
            } else if self.opaque_operand == Some(operands.len()) {
                cursor = start;
                break Some(OpaqueBoundary {
                    start,
                    preceding_operands: operands.len(),
                    separator: ended,
                    flags: Vec::new(),
                });
            } else {
                operands.push(token.as_str());
            }
        };
        if command_cursor != command.len() {
            return Err(invalid("Missing declared subcommand"));
        }
        operands.extend(argv[cursor..].iter().map(String::as_str));
        self.assign_operands(&operands, &mut input)?;
        let opaque_tail =
            boundary.map(|boundary| self.opaque_tail(argv, &operands, &input, boundary));
        Ok(ResolvedArgv {
            input: Value::Object(input),
            opaque_tail,
        })
    }

    /// Keep the normal typed prefix and preserve the original tokens from the
    /// trigger onward. The full input still supplies the schema and path checks.
    fn opaque_tail(
        &self,
        argv: &[String],
        operands: &[&str],
        input: &Map<String, Value>,
        boundary: OpaqueBoundary<'_, '_>,
    ) -> OpaqueTail {
        let mut prefix = input.clone();
        let mut converted_paths = Map::new();
        let trigger_value = boundary.flags.last().and_then(|flag| {
            let raw = flag.raw_value?;
            let value = &input[flag.property.name];
            let scalar = value
                .as_array()
                .and_then(|items| items.last())
                .unwrap_or(value);
            Some((raw.into(), scalar.is_number()))
        });
        self.remove_tail_operands(
            &mut prefix,
            &mut converted_paths,
            &operands[boundary.preceding_operands..],
        );
        for flag in boundary.flags {
            let name = flag.property.name;
            if let Some(items) = prefix.get_mut(name).and_then(Value::as_array_mut) {
                if let (Some(value), Some(raw)) = (items.pop(), flag.raw_value) {
                    if !value.is_string() && flag.property.is_path() {
                        converted_paths.insert(name.into(), Value::Array(vec![raw.into()]));
                    }
                }
                if !items.is_empty() {
                    continue;
                }
            } else if let Some(raw) = flag.raw_value {
                if !input[name].is_string() && flag.property.is_path() {
                    converted_paths.insert(name.into(), raw.into());
                }
            }
            prefix.remove(name);
        }
        OpaqueTail {
            prefix,
            argv: argv[boundary.start..].to_vec(),
            separator: boundary.separator,
            trigger_value,
            converted_paths: Value::Object(converted_paths),
        }
    }

    fn remove_tail_operands(
        &self,
        prefix: &mut Map<String, Value>,
        converted_paths: &mut Map<String, Value>,
        operands: &[&str],
    ) {
        let mut remaining = operands.len();
        for property in self.operands.iter().rev() {
            if remaining == 0 {
                break;
            }
            let Some(value) = prefix.get_mut(property.name) else {
                continue;
            };
            if let Some(items) = value.as_array_mut() {
                let count = items.len().min(remaining);
                let keep = items.len() - count;
                if property.is_path() && items[keep..].iter().any(|item| !item.is_string()) {
                    let paths = &operands[remaining - count..remaining];
                    converted_paths.insert(property.name.into(), serde_json::json!(paths));
                }
                remaining -= count;
                if keep > 0 {
                    items.truncate(keep);
                    break;
                }
            } else {
                remaining -= 1;
                if property.is_path() && !value.is_string() {
                    converted_paths.insert(property.name.into(), operands[remaining].into());
                }
            }
            prefix.remove(property.name);
        }
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    /// Consume one flag token. Returns whether it starts an opaque tail.
    fn parse_flag<'b>(
        &self,
        token: &'b str,
        argv: &'b [String],
        cursor: &mut usize,
        input: &mut Map<String, Value>,
        position: FlagPosition,
    ) -> Result<Option<Vec<ParsedFlag<'a, 'b>>>, ModuleError> {
        let (literal, attached) = if token.starts_with("--") {
            token
                .split_once('=')
                .map_or((token, None), |(name, value)| (name, Some(value)))
        } else {
            (token, None)
        };
        if let Some(property) = self.lookup(literal, position) {
            // `python3 -m json.tool --compact`: the module name is this flag's
            // value and everything after it belongs to the module, so the tail
            // is forwarded rather than validated against python3's grammar.
            let opaque = property.schema["x-apexe-opaque-after"] == true;
            let raw_value = self.consume(property, attached, argv, cursor, input, position)?;
            return Ok(opaque.then(|| {
                vec![ParsedFlag {
                    property,
                    raw_value,
                }]
            }));
        }
        // `head -20`: the digit run is the value of the flag the overlay marked,
        // not a bundle of `-2` and `-0`. Only a declared flag is resolved this
        // way; without the declaration the token still falls through to the
        // bundle path and is refused, which is what `ls -5` must do.
        if let Some(property) = self.digit_value.filter(|_| is_digit_run(token)) {
            let digits = token.trim_start_matches('-');
            let raw_value = self.consume(property, Some(digits), argv, cursor, input, position)?;
            return Ok((property.schema["x-apexe-opaque-after"] == true).then(|| {
                vec![ParsedFlag {
                    property,
                    raw_value,
                }]
            }));
        }
        if token.starts_with("--") || self.schema["x-apexe-short-flag-bundling"] != true {
            return Err(invalid(format!(
                "Undeclared flag '{token}' or undeclared short-flag bundling"
            )));
        }
        // A bundle can carry the marker too: `-qx` with `-x` opaque starts the
        // tail at the same place `-x` alone would.
        let mut opaque = false;
        let mut bundle = Vec::new();
        for (offset, character) in token[1..].char_indices() {
            let literal = format!("-{character}");
            let property = self
                .lookup(&literal, position)
                .ok_or_else(|| invalid(format!("Undeclared flag '{literal}' in '{token}'")))?;
            let tail = &token[1 + offset + character.len_utf8()..];
            let value_option = !boolean_only(property.schema);
            let attached = (value_option && !tail.is_empty()).then_some(tail);
            let raw_value = self.consume(property, attached, argv, cursor, input, position)?;
            opaque |= property.schema["x-apexe-opaque-after"] == true;
            bundle.push(ParsedFlag {
                property,
                raw_value,
            });
            if value_option {
                break;
            }
        }
        Ok(opaque.then_some(bundle))
    }

    /// Resolve one flag spelling for the phase the call is in.
    ///
    /// Each phase prefers its own namespace, then falls back to the other so
    /// [`Parser::consume`] can report "wrong side of the subcommand" rather
    /// than the less useful "undeclared flag".
    fn lookup(&self, literal: &str, position: FlagPosition) -> Option<Property<'a>> {
        match position {
            FlagPosition::Root => self
                .flags
                .get(literal)
                .or_else(|| self.global_flags.get(literal))
                .copied(),
            FlagPosition::BeforeCommand => self
                .global_flags
                .get(literal)
                .or_else(|| self.flags.get(literal))
                .copied(),
            FlagPosition::AfterCommand => self
                .flags
                .get(literal)
                .or_else(|| self.global_flags.get(literal))
                .copied(),
        }
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn consume<'b>(
        &self,
        property: Property<'_>,
        attached: Option<&'b str>,
        argv: &'b [String],
        cursor: &mut usize,
        input: &mut Map<String, Value>,
        position: FlagPosition,
    ) -> Result<Option<&'b str>, ModuleError> {
        let global = is_global_property(property.schema);
        if matches!(position, FlagPosition::BeforeCommand) && !global
            || matches!(position, FlagPosition::AfterCommand) && global
        {
            return Err(invalid(format!(
                "Flag '{}' is on the wrong side of the subcommand",
                property.name
            )));
        }
        let (value, raw_value) = if let Some(text) = attached {
            if boolean_only(property.schema) {
                return Err(invalid(format!(
                    "Boolean flag '{}' takes no value",
                    property.name
                )));
            }
            (parse_value(text, property)?, Some(text))
        } else if boolean_only(property.schema) || property.schema["x-apexe-value-optional"] == true
        {
            (Value::Bool(true), None)
        } else {
            let text = argv
                .get(*cursor)
                .ok_or_else(|| invalid(format!("Flag '{}' requires a value", property.name)))?;
            *cursor += 1;
            (parse_value(text, property)?, Some(text.as_str()))
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
        Ok(raw_value)
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
    /// `python3 -m json.tool --compact`: the tail starts at a *flag's* value, not
    /// at a positional, so the operand marker cannot express it.
    #[test]
    fn test_opaque_flag_forwards_every_later_token() {
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "module": {"type": "string", "x-apexe-flag": "-m",
                           "x-apexe-opaque-after": true},
                "file": {"type": "array", "items": {"type": "string"},
                         "x-apexe-positional": 0}
            }
        });
        let parsed = resolve_argv(
            &args(&["-m", "json.tool", "--compact", "/dev/null"]),
            &schema,
        )
        .unwrap();
        assert_eq!(parsed["module"], json!("json.tool"));
        assert_eq!(parsed["file"], json!(["--compact", "/dev/null"]));
    }

    /// A bundled short flag must start the opaque tail too.
    ///
    /// The single-flag branch returns whether the property is opaque; the bundle
    /// branch used to return `false` unconditionally, so `-x value` forwarded the
    /// tail while `-qx value` validated it against the tool's own grammar.
    #[test]
    fn test_bundled_opaque_flag_starts_the_opaque_tail() {
        let schema = json!({
            "type": "object",
            "x-apexe-short-flag-bundling": true,
            "x-apexe-end-of-options": true,
            "properties": {
                "quiet": {"type": "boolean", "x-apexe-flag": "-q"},
                "exec": {"type": "boolean", "x-apexe-flag": "-x",
                         "x-apexe-opaque-after": true},
                "file": {"type": "array", "items": {"type": "string"},
                         "x-apexe-positional": 0}
            }
        });
        let parsed = resolve_argv(&args(&["-qx", "--whatever"]), &schema).unwrap();
        assert_eq!(parsed["quiet"], json!(true));
        assert_eq!(parsed["exec"], json!(true));
        assert_eq!(parsed["file"], json!(["--whatever"]));
    }

    #[test]
    fn test_without_the_opaque_flag_declaration_the_tail_is_refused() {
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "module": {"type": "string", "x-apexe-flag": "-m"},
                "file": {"type": "array", "items": {"type": "string"},
                         "x-apexe-positional": 0}
            }
        });
        let error = resolve_argv(&args(&["-m", "json.tool", "--compact"]), &schema).unwrap_err();
        assert!(
            error.message.contains("Undeclared flag"),
            "{}",
            error.message
        );
    }

    /// An interpreter stops parsing at its script: `node tsc.js --noEmit` must
    /// not have `--noEmit` validated against `node`'s own flags.
    #[test]
    fn test_opaque_operand_forwards_every_later_token() {
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "script": {"type": "array", "items": {"type": "string"},
                           "x-apexe-positional": 0, "x-apexe-opaque-after": true},
                "verbose": {"type": "boolean", "x-apexe-flag": "--verbose"}
            }
        });
        let parsed = resolve_argv(
            &args(&["--verbose", "tsc.js", "--noEmit", "-p", "tsconfig.json"]),
            &schema,
        )
        .unwrap();
        assert_eq!(parsed["verbose"], json!(true));
        assert_eq!(
            parsed["script"],
            json!(["tsc.js", "--noEmit", "-p", "tsconfig.json"])
        );
    }

    #[test]
    fn test_without_the_opaque_declaration_the_same_argv_is_refused() {
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "script": {"type": "array", "items": {"type": "string"},
                           "x-apexe-positional": 0},
                "verbose": {"type": "boolean", "x-apexe-flag": "--verbose"}
            }
        });
        let error = resolve_argv(&args(&["tsc.js", "--noEmit"]), &schema).unwrap_err();
        assert!(
            error.message.contains("Undeclared flag"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_opaque_declaration_must_be_on_the_last_variadic_operand() {
        let scalar = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "script": {"type": "string", "x-apexe-positional": 0,
                           "x-apexe-opaque-after": true}
            }
        });
        let error = resolve_argv(&args(&["tsc.js"]), &scalar).unwrap_err();
        assert!(
            error.message.contains("must sit on the last operand"),
            "{}",
            error.message
        );
    }

    /// The `head -20` spelling: a digit run is the declared flag's value.
    ///
    /// BSD and GNU `head`/`tail` accept it and neither man page documents it, so
    /// only an overlay that probed the binary can declare it -- see issue #46.
    #[test]
    fn test_digit_shorthand_is_the_declared_flags_value() {
        let mut schema = schema();
        schema["properties"]["count"]["x-apexe-digit-value"] = json!(true);
        let parsed = resolve_argv(&args(&["-20", "f"]), &schema).unwrap();
        assert_eq!(parsed["count"], json!(20));
        assert_eq!(parsed["file"], json!(["f"]));
    }

    #[test]
    fn test_digit_token_still_refuses_without_the_declaration() {
        let error = resolve_argv(&args(&["-20"]), &schema()).unwrap_err();
        assert!(
            error.message.contains("Undeclared flag '-2'"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_two_digit_shorthand_declarations_are_rejected() {
        let mut schema = schema();
        schema["properties"]["count"]["x-apexe-digit-value"] = json!(true);
        schema["properties"]["long"]["x-apexe-digit-value"] = json!(true);
        let error = resolve_argv(&args(&["-20"]), &schema).unwrap_err();
        assert!(
            error.message.contains("declared on two flags"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_a_non_digit_run_keeps_its_normal_meaning() {
        let mut schema = schema();
        schema["properties"]["count"]["x-apexe-digit-value"] = json!(true);
        // `-2.5` is not the shorthand, so it is a bundle of undeclared flags.
        let error = resolve_argv(&args(&["-2.5"]), &schema).unwrap_err();
        assert!(
            error.message.contains("Undeclared flag"),
            "{}",
            error.message
        );
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
    fn test_resolve_argv_refuses_undeclared_end_of_options() {
        let mut schema = schema();
        schema["x-apexe-end-of-options"] = json!(false);
        let error = resolve_argv(&args(&["--", "file"]), &schema).unwrap_err();
        assert!(error.message.contains("does not declare --"));
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
    /// A tool that reuses one spelling on both sides of the subcommand, as git
    /// does: `-v` is the version before `status` and verbose after it, and `-C`
    /// is a tool-level directory before the subcommand and a copy count after
    /// it.
    fn two_sided_schema() -> Value {
        json!({"type":"object","x-apexe-short-flag-bundling":false,"x-apexe-end-of-options":true,"properties":{
            "verbose":{"type":"boolean","x-apexe-flag":"-v"},
            "version":{"type":"boolean","x-apexe-flag":"--version","x-apexe-flag-aliases":["-v","--version"],"x-apexe-flag-position":"before-subcommand"},
            "dir":{"type":"string","x-apexe-flag":"-C","x-apexe-flag-position":"before-operands"},
            "copies":{"type":"integer","x-apexe-flag":"-C"}
        }})
    }
    #[test]
    fn test_resolve_argv_routes_one_spelling_by_its_side_of_the_subcommand() {
        assert_eq!(
            resolve_command_argv(
                &args(&["-C", "/tmp", "status", "-C", "2", "-v"]),
                &two_sided_schema(),
                &args(&["status"])
            )
            .unwrap()
            .input,
            json!({"dir":"/tmp","copies":2,"verbose":true})
        );
        assert_eq!(
            resolve_command_argv(
                &args(&["-v", "status"]),
                &two_sided_schema(),
                &args(&["status"])
            )
            .unwrap()
            .input,
            json!({"version":true})
        );
    }
    #[test]
    fn test_resolve_argv_refuses_a_tool_level_flag_after_the_subcommand() {
        let error = resolve_command_argv(
            &args(&["status", "--version"]),
            &two_sided_schema(),
            &args(&["status"]),
        )
        .unwrap_err();
        assert!(
            error.message.contains("wrong side of the subcommand"),
            "{}",
            error.message
        );
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
            .unwrap()
            .input,
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
