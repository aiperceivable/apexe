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
    /// The module this argv will be dispatched to, when the caller knows it.
    ///
    /// Used only to name the contract a refusal tells the caller to use
    /// instead (`apexe exec cli.find --input …`); parsing never reads it.
    module: Option<&'a str>,
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
    /// Index (among operands) of the trailing mini-language that is parsed
    /// rather than forwarded. `None` when nothing declares it.
    tail_parsed_operand: Option<usize>,
    /// Non-dash tokens that belong to that mini-language (`find`'s `(`, `)`,
    /// `!`, `,`). They are neither flags nor values: the resolver treats one as
    /// the start of the tail and skips it inside the tail, without writing
    /// anything to the input.
    tail_tokens: Vec<&'a str>,
}

/// Whether a property includes the operand in the *leading* group, which is
/// what `x-apexe-tail-parsed` requires of everything before the tail.
///
/// Deliberately not [`is_global_property`]: that one also covers
/// `before-subcommand`, which is about the namespace a flag lives in rather
/// than about which side of the operands it is rendered on.
fn is_before_operands(schema: &Value) -> bool {
    schema["x-apexe-flag-position"].as_str() == Some("before-operands")
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
    resolve_command_argv(argv, schema, &[], None).map(|resolved| resolved.input)
}

#[derive(Debug)]
pub(crate) struct ResolvedArgv {
    pub(crate) input: Value,
    pub(crate) opaque_tail: Option<OpaqueTail>,
    /// Token positions (in the parsed argv) of the leading operands, in order.
    ///
    /// The child-execution policy needs to know where the first positional
    /// operand begins so a `--` separator is inserted before the operands, not
    /// before an arbitrary `{}` that may be a flag value or a later operand.
    pub(crate) operand_indices: Vec<usize>,
    /// Embedded child command lists (`find -exec rm {} ;`), in argv order.
    ///
    /// The list is not a value of this command's grammar: its tokens belong to
    /// another program, so they are collected here for the child-execution
    /// policy and never classified as this command's flags.
    pub(crate) exec_lists: Vec<ExecList>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecList {
    /// Absolute index in the caller's argv of the program token (the first
    /// token after the flag).
    pub(crate) argv_index: usize,
    /// Program, arguments and the `;`/`+` terminator, in argv order.
    pub(crate) tokens: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct OpaqueTail {
    pub(crate) prefix: Map<String, Value>,
    pub(crate) argv: Vec<String>,
    pub(crate) separator: bool,
    pub(crate) trigger_value: Option<(String, bool)>,
    pub(crate) converted_paths: Value,
}

struct TailBoundary<'a, 'b> {
    start: usize,
    preceding_operands: usize,
    separator: bool,
    /// Every flag parsed at or after `start`, so the tail can be forwarded
    /// verbatim without the prefix rendering it a second time.
    flags: Vec<ParsedFlag<'a, 'b>>,
    /// Whether every token from `start` on was validated. `false` for an opaque
    /// tail, whose tokens are another program's argv and were never parsed.
    parsed: bool,
}

struct ParsedFlag<'a, 'b> {
    property: Property<'a>,
    raw_value: Option<&'b str>,
}

/// One flag token's effect: the values it wrote and whether it starts an opaque
/// tail.
struct ParsedFlags<'a, 'b> {
    flags: Vec<ParsedFlag<'a, 'b>>,
    opaque: bool,
    /// Set when the flag introduces an embedded command list.
    exec_list: Option<ExecList>,
}

/// The state one left-to-right pass over argv accumulates.
struct Scan<'a, 'b> {
    input: Map<String, Value>,
    operands: Vec<&'b str>,
    /// Parallel to `operands`: the token index in the input argv.
    operand_indices: Vec<usize>,
    cursor: usize,
    command_cursor: usize,
    ended: bool,
    boundary: Option<TailBoundary<'a, 'b>>,
    /// A parsed tail's boundary token, the token after its flag, and the flags
    /// parsed so far. The remaining tokens are parsed in [`Parser::finish`],
    /// where the complete `input` is in hand.
    parsed_tail: Option<(usize, usize, Vec<ParsedFlag<'a, 'b>>)>,
    exec_lists: Vec<ExecList>,
}

#[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
pub(crate) fn resolve_command_argv(
    argv: &[String],
    schema: &Value,
    command: &[String],
    module: Option<&str>,
) -> Result<ResolvedArgv, ModuleError> {
    Parser::new(schema, module)?.parse(argv, command)
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
    fn new(schema: &'a Value, module: Option<&'a str>) -> Result<Self, ModuleError> {
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
            module,
            flags: BTreeMap::new(),
            global_flags: BTreeMap::new(),
            operands: vec![],
            digit_value: None,
            opaque_operand: None,
            tail_parsed_operand: None,
            tail_tokens: Vec::new(),
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
        parser.tail_parsed_operand = parser.locate_tail_parsed_operand()?;
        parser.tail_tokens = parser.locate_tail_tokens()?;
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

    /// Read the non-dash tokens a parsed tail may contain.
    ///
    /// They exist because `find`'s expression mixes dash primaries with `(`,
    /// `)`, `!` and `,`, which are neither flags nor operand values. Declaring
    /// them is only meaningful next to a `tail_parsed` operand, and an
    /// `-exec` terminator is refused outright: a tail token is skipped rather
    /// than parsed, so `;` here would let `-exec utility ;` run a program.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn locate_tail_tokens(&self) -> Result<Vec<&'a str>, ModuleError> {
        let Some(values) = self.schema["x-apexe-tail-literal"].as_array() else {
            return Ok(Vec::new());
        };
        let mut tokens = Vec::with_capacity(values.len());
        for value in values {
            let token = value
                .as_str()
                .ok_or_else(|| invalid("'x-apexe-tail-literal' must be an array of strings"))?;
            if token.is_empty() || token.starts_with('-') {
                return Err(invalid(format!(
                    "'x-apexe-tail-literal' entry '{token}' must be non-empty and must not \
                     start with '-'; flag spellings are resolved by the flag list"
                )));
            }
            if matches!(token, ";" | "+" | "{}") {
                return Err(invalid(format!(
                    "'x-apexe-tail-literal' entry '{token}' is an -exec terminator; declaring \
                     it would reopen the execution path the parsed tail keeps closed"
                )));
            }
            tokens.push(token);
        }
        if !tokens.is_empty() && self.tail_parsed_operand.is_none() {
            return Err(invalid(
                "'x-apexe-tail-literal' requires an operand marked 'x-apexe-tail-parsed'",
            ));
        }
        Ok(tokens)
    }

    fn is_tail_token(&self, token: &str) -> bool {
        self.tail_tokens.contains(&token)
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn validate_operands(&self) -> Result<(), ModuleError> {
        if self.operands.windows(2).any(|pair| {
            pair[0].schema["x-apexe-positional"] == pair[1].schema["x-apexe-positional"]
        }) {
            return Err(invalid("Ambiguous declared positional grammar"));
        }
        let variadic = self
            .operands
            .iter()
            .filter(|property| property.schema["type"] == "array")
            .count();
        if variadic > 1 && !self.declares_parsed_tail() {
            return Err(invalid("Ambiguous declared positional grammar"));
        }
        Ok(())
    }

    /// Whether the operands describe the one grammar that needs two variadic
    /// slots: a leading `before_flags` operand and a trailing parsed tail.
    ///
    /// The exception is stated here, before any marker is located, because this
    /// is the check that refuses two variadic operands. Everything else keeps
    /// the old refusal: an overlay cannot make an ambiguous grammar readable by
    /// adding the marker to a shape where the boundary means nothing.
    fn declares_parsed_tail(&self) -> bool {
        let Some((tail, leading)) = self.operands.split_last() else {
            return false;
        };
        let leading_variadic = leading
            .iter()
            .filter(|property| property.schema["type"] == "array")
            .count();
        tail.schema["x-apexe-tail-parsed"] == true
            && leading_variadic == 1
            && leading
                .iter()
                .all(|property| property.schema["x-apexe-operand-position"] == "before-flags")
    }

    /// Find the operand that starts the parsed tail, if any.
    ///
    /// Like the opaque marker, the declaration is only meaningful on the last,
    /// variadic operand: everything from it onward is the tail, so a later
    /// declared operand would have nothing left to match. Declaring both
    /// treatments on one grammar is refused rather than guessed at, because one
    /// of them would silently win.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn locate_tail_parsed_operand(&self) -> Result<Option<usize>, ModuleError> {
        let Some(index) = self
            .operands
            .iter()
            .position(|property| property.schema["x-apexe-tail-parsed"] == true)
        else {
            return Ok(None);
        };
        let property = self.operands[index];
        if index + 1 != self.operands.len() || property.schema["type"] != "array" {
            return Err(invalid(format!(
                "'x-apexe-tail-parsed' must sit on the last operand and that operand must be \
                 variadic; '{}' is not",
                property.name
            )));
        }
        if let Some(opaque) = self
            .operands
            .iter()
            .find(|operand| operand.schema["x-apexe-opaque-after"] == true)
        {
            return Err(invalid(format!(
                "'x-apexe-tail-parsed' and 'x-apexe-opaque-after' are opposite treatments of \
                 the same tail and cannot both be declared; '{}' and '{}' carry them",
                property.name, opaque.name
            )));
        }
        Ok(Some(index))
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn parse(&self, argv: &[String], command: &[String]) -> Result<ResolvedArgv, ModuleError> {
        let scan = self.scan_argv(argv, command)?;
        self.finish(argv, command, scan)
    }

    /// The phase a flag token is resolved in.
    fn position(&self, command: &[String], command_cursor: usize) -> FlagPosition {
        if command.is_empty() {
            FlagPosition::Root
        } else if command_cursor < command.len() {
            FlagPosition::BeforeCommand
        } else {
            FlagPosition::AfterCommand
        }
    }

    /// Walk argv once, separating the declared subcommand, the flags, the
    /// leading operands and, for a parsed tail, the token the tail starts at.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn scan_argv<'b>(
        &self,
        argv: &'b [String],
        command: &[String],
    ) -> Result<Scan<'a, 'b>, ModuleError> {
        let mut scan = Scan {
            input: Map::new(),
            operands: Vec::new(),
            operand_indices: Vec::new(),
            cursor: 0,
            command_cursor: 0,
            ended: false,
            boundary: None,
            parsed_tail: None,
            exec_lists: Vec::new(),
        };
        while let Some(token) = argv.get(scan.cursor) {
            let start = scan.cursor;
            scan.cursor += 1;
            let position = self.position(command, scan.command_cursor);
            if !scan.ended && token == "--" {
                if self.schema["x-apexe-end-of-options"] != true
                    || scan.command_cursor < command.len()
                {
                    return Err(invalid("This command does not declare -- at this position"));
                }
                scan.ended = true;
            } else if token.starts_with('-')
                && token != "-"
                && (!scan.ended || self.token_starts_flag(token, position))
            {
                let before_subcommand = scan.command_cursor < command.len();
                if !self.scan_flag(token, argv, start, before_subcommand, position, &mut scan)? {
                    break;
                }
            } else if scan.command_cursor < command.len() {
                if token != &command[scan.command_cursor] {
                    return Err(invalid(
                        "Subcommand path does not match command_path metadata",
                    ));
                }
                scan.command_cursor += 1;
            } else if self.opaque_operand == Some(scan.operands.len()) {
                scan.cursor = start;
                scan.boundary = Some(TailBoundary {
                    start,
                    preceding_operands: scan.operands.len(),
                    separator: scan.ended,
                    flags: Vec::new(),
                    parsed: false,
                });
                break;
            } else if self.is_tail_token(token) {
                // `find . \( -name a -o -name b \)`: the operator opens the
                // expression, so it ends the leading operands exactly as a
                // declared flag would.
                scan.parsed_tail = Some((start, scan.cursor, Vec::new()));
                break;
            } else {
                scan.operands.push(token.as_str());
                scan.operand_indices.push(start);
            }
        }
        if scan.command_cursor != command.len() {
            return Err(invalid("Missing declared subcommand"));
        }
        Ok(scan)
    }

    /// Consume one flag token and decide what it does to the scan.
    ///
    /// Returns `false` when the token ended the scan: either it triggers an
    /// opaque tail, or it is the first token of a parsed tail.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn scan_flag<'b>(
        &self,
        token: &'b str,
        argv: &'b [String],
        start: usize,
        before_subcommand: bool,
        position: FlagPosition,
        scan: &mut Scan<'a, 'b>,
    ) -> Result<bool, ModuleError> {
        let parsed = self.parse_flag(token, argv, &mut scan.cursor, &mut scan.input, position)?;
        if let Some(exec_list) = parsed.exec_list {
            if before_subcommand {
                return Err(invalid(
                    "Embedded commands cannot start before a subcommand",
                ));
            }
            scan.exec_lists.push(exec_list);
            scan.parsed_tail = Some((start, scan.cursor, parsed.flags));
            return Ok(false);
        }
        if parsed.opaque {
            if before_subcommand {
                return Err(invalid("Opaque arguments cannot start before a subcommand"));
            }
            if self.tail_parsed_operand.is_some() {
                return Err(invalid(format!(
                    "Flag '{token}' declares an opaque tail inside an operand marked \
                     'x-apexe-tail-parsed'; the two treatments of the same tail cannot both apply"
                )));
            }
            scan.boundary = Some(TailBoundary {
                start,
                preceding_operands: scan.operands.len(),
                separator: false,
                flags: parsed.flags,
                parsed: false,
            });
            return Ok(false);
        }
        // A tool-level option may still precede the leading operand it is
        // rendered before (`find -L path ... -name x`). Only a flag on the other
        // side of the operands closes them and starts the tail.
        let closes_leading_operand = self.tail_parsed_operand.is_some()
            && !parsed
                .flags
                .iter()
                .all(|flag| is_before_operands(flag.property.schema));
        if closes_leading_operand {
            scan.parsed_tail = Some((start, scan.cursor, parsed.flags));
            return Ok(false);
        }
        Ok(true)
    }

    /// Finish the call: parse a trailing mini-language, assign the operands and
    /// build the tail the executor forwards.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn finish<'b>(
        &self,
        argv: &'b [String],
        command: &[String],
        scan: Scan<'a, 'b>,
    ) -> Result<ResolvedArgv, ModuleError> {
        let Scan {
            mut input,
            mut operands,
            mut operand_indices,
            mut exec_lists,
            cursor,
            mut boundary,
            parsed_tail,
            ended,
            ..
        } = scan;
        if let Some((start, after_flag, flags)) = parsed_tail {
            boundary = Some(TailBoundary {
                start,
                preceding_operands: operands.len(),
                separator: ended,
                flags: self.parse_shadowed_tail(
                    argv,
                    after_flag,
                    flags,
                    &mut input,
                    command.is_empty(),
                    &mut exec_lists,
                )?,
                parsed: true,
            });
        } else {
            operands.extend(argv[cursor..].iter().map(String::as_str));
            operand_indices.extend(cursor..argv.len());
        }
        self.assign_operands(&operands, &mut input)?;
        let opaque_tail =
            boundary.map(|boundary| self.opaque_tail(argv, &operands, &input, boundary));
        Ok(ResolvedArgv {
            input: Value::Object(input),
            opaque_tail,
            operand_indices,
            exec_lists,
        })
    }

    /// Whether a token after `--` is still one of this command's own flags.
    ///
    /// Only a parsed tail needs the question. Everywhere else `--` means "no
    /// more flags", and answering differently here would change the meaning of
    /// the separator for grammars that never asked for it.
    fn token_starts_flag(&self, token: &str, position: FlagPosition) -> bool {
        self.tail_parsed_operand.is_some() && self.lookup_token(token, position).is_some()
    }

    /// Resolve a token to a declared property without consuming anything.
    ///
    /// The read-only mirror of [`Self::parse_flag`]: it applies the same
    /// spelling rules (long form, `=value`, the digit shorthand, short bundles)
    /// so that "is this a flag?" and "parse this flag" cannot disagree.
    fn lookup_token(&self, token: &str, position: FlagPosition) -> Option<Property<'a>> {
        let literal = if token.starts_with("--") {
            token.split_once('=').map_or(token, |(name, _value)| name)
        } else {
            token
        };
        if let Some(property) = self.lookup(literal, position) {
            return Some(property);
        }
        if self.digit_value.is_some() && is_digit_run(token) {
            return self.digit_value;
        }
        if token.starts_with("--") || self.schema["x-apexe-short-flag-bundling"] != true {
            return None;
        }
        let mut first = None;
        for character in token[1..].chars() {
            let literal = format!("-{character}");
            let property = self.lookup(&literal, position)?;
            first.get_or_insert(property);
            if !boolean_only(property.schema) {
                break;
            }
        }
        first
    }

    /// Parse every token after a parsed tail's boundary.
    ///
    /// The tokens are forwarded verbatim so argv order survives, but each one is
    /// also resolved here: the approval gate and the path guard read `input`,
    /// and would otherwise never see `-delete` or `-exec`. A token no declared
    /// flag explains is refused rather than forwarded, which is the one shape
    /// this grammar cannot express: `find . -exec rm {} \;` ends in another
    /// program's argv.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn parse_shadowed_tail<'b>(
        &self,
        argv: &'b [String],
        mut cursor: usize,
        mut flags: Vec<ParsedFlag<'a, 'b>>,
        input: &mut Map<String, Value>,
        root: bool,
        exec_lists: &mut Vec<ExecList>,
    ) -> Result<Vec<ParsedFlag<'a, 'b>>, ModuleError> {
        let position = if root {
            FlagPosition::Root
        } else {
            FlagPosition::AfterCommand
        };
        while let Some(token) = argv.get(cursor) {
            let index = cursor;
            cursor += 1;
            if token == "--" {
                // The tool stops reading options, but its expression grammar
                // continues. Keep the token in the forwarded argv and keep
                // classifying, so `-delete` cannot hide behind the separator.
                if self.schema["x-apexe-end-of-options"] != true {
                    return Err(invalid("This command does not declare -- at this position"));
                }
                continue;
            }
            if self.is_tail_token(token) {
                // `(`, `)`, `!`, `,`: punctuation the tool itself interprets.
                // Nothing is written to the input, and the token still reaches
                // the forwarded argv because the whole tail is forwarded.
                continue;
            }
            if !token.starts_with('-') || token == "-" {
                return Err(self.unclassified_tail_token(token, index));
            }
            let parsed = self.parse_flag(token, argv, &mut cursor, input, position)?;
            if let Some(exec_list) = parsed.exec_list {
                exec_lists.push(exec_list);
                flags.extend(parsed.flags);
                continue;
            }
            if parsed.opaque {
                return Err(invalid(format!(
                    "Flag '{token}' declares an opaque tail inside one that is parsed; the two \
                     treatments cannot both apply"
                )));
            }
            flags.extend(parsed.flags);
        }
        Ok(flags)
    }

    /// Refuse a tail token that is not a declared flag.
    ///
    /// Some parsed tails are mini-languages whose tokens are another program's
    /// argv, and no schema can validate those. Forwarding them would hide a
    /// write from the approval gate, which is the failure the marker exists to
    /// prevent, so the call is refused and the token is named.
    fn unclassified_tail_token(&self, token: &str, index: usize) -> ModuleError {
        let call = self.module.map_or_else(
            || "`apexe exec <module> --input …`".to_string(),
            |module| format!("`apexe exec {module} --input …`"),
        );
        ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Token '{token}' at argument {index} cannot be classified: after the expression \
                 boundary every token must be a declared flag, and an -exec/-ok argument list is \
                 another program's command line rather than a flag"
            ),
        )
        .with_ai_guidance(format!(
            "Send the call through the module contract instead: {call}, so every value still \
             reaches the approval gate and the path guard."
        ))
    }

    /// Keep the normal typed prefix and preserve the original tokens from the
    /// trigger onward. The full input still supplies the schema and path checks.
    fn opaque_tail(
        &self,
        argv: &[String],
        operands: &[&str],
        input: &Map<String, Value>,
        boundary: TailBoundary<'_, '_>,
    ) -> OpaqueTail {
        let mut prefix = input.clone();
        let mut converted_paths = Map::new();
        let trigger_value = if boundary.parsed {
            // A parsed tail was validated token by token while it was shadow-
            // parsed; there is no single trigger value left to re-check.
            None
        } else {
            boundary.flags.last().and_then(|flag| {
                let raw = flag.raw_value?;
                let value = &input[flag.property.name];
                let scalar = value
                    .as_array()
                    .and_then(|items| items.last())
                    .unwrap_or(value);
                Some((raw.into(), scalar.is_number()))
            })
        };
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

    /// Consume a flag that introduces another program's argv.
    ///
    /// The list runs from the token after the flag through `;` or `+`. Its
    /// contents are not this command's grammar, so they are recorded verbatim
    /// and only the program name lands in the input, where the approval gate
    /// and the child-execution policy read it.
    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    fn collect_exec_list<'b>(
        &self,
        property: Property<'a>,
        attached: Option<&'b str>,
        argv: &'b [String],
        cursor: &mut usize,
        input: &mut Map<String, Value>,
    ) -> Result<ParsedFlags<'a, 'b>, ModuleError> {
        if attached.is_some() {
            return Err(invalid(format!(
                "Flag '{}' introduces a command list and takes no attached value",
                property.name
            )));
        }
        let start = *cursor;
        let mut tokens = Vec::new();
        while let Some(token) = argv.get(*cursor) {
            *cursor += 1;
            tokens.push(token.clone());
            if token == ";" || token == "+" {
                break;
            }
        }
        if tokens
            .last()
            .is_none_or(|token| token != ";" && token != "+")
        {
            return Err(invalid(format!(
                "Flag '{}' introduces a command list that must end with ';' or '+'",
                property.name
            )));
        }
        let program = tokens.first().cloned().unwrap_or_default();
        if program.is_empty() || program == ";" || program == "+" {
            return Err(invalid(format!(
                "Flag '{}' must name a program before its terminator",
                property.name
            )));
        }
        input.insert(property.name.into(), Value::String(program));
        Ok(ParsedFlags {
            flags: vec![ParsedFlag {
                property,
                raw_value: None,
            }],
            opaque: false,
            exec_list: Some(ExecList {
                argv_index: start,
                tokens,
            }),
        })
    }

    #[allow(clippy::result_large_err)] // Preserve the crate-wide ModuleError boundary.
    /// Consume one flag token, recording the values it carries.
    fn parse_flag<'b>(
        &self,
        token: &'b str,
        argv: &'b [String],
        cursor: &mut usize,
        input: &mut Map<String, Value>,
        position: FlagPosition,
    ) -> Result<ParsedFlags<'a, 'b>, ModuleError> {
        let (literal, attached) = if token.starts_with("--") {
            token
                .split_once('=')
                .map_or((token, None), |(name, value)| (name, Some(value)))
        } else {
            (token, None)
        };
        if let Some(property) = self.lookup(literal, position) {
            if property.schema["x-apexe-exec-list"] == true {
                return self.collect_exec_list(property, attached, argv, cursor, input);
            }
            // `python3 -m json.tool --compact`: the module name is this flag's
            // value and everything after it belongs to the module, so the tail
            // is forwarded rather than validated against python3's grammar.
            let opaque = property.schema["x-apexe-opaque-after"] == true;
            let raw_value = self.consume(property, attached, argv, cursor, input, position)?;
            return Ok(ParsedFlags {
                flags: vec![ParsedFlag {
                    property,
                    raw_value,
                }],
                opaque,
                exec_list: None,
            });
        }
        // `head -20`: the digit run is the value of the flag the overlay marked,
        // not a bundle of `-2` and `-0`. Only a declared flag is resolved this
        // way; without the declaration the token still falls through to the
        // bundle path and is refused, which is what `ls -5` must do.
        if let Some(property) = self.digit_value.filter(|_| is_digit_run(token)) {
            let digits = token.trim_start_matches('-');
            let raw_value = self.consume(property, Some(digits), argv, cursor, input, position)?;
            return Ok(ParsedFlags {
                flags: vec![ParsedFlag {
                    property,
                    raw_value,
                }],
                opaque: property.schema["x-apexe-opaque-after"] == true,
                exec_list: None,
            });
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
        Ok(ParsedFlags {
            flags: bundle,
            opaque,
            exec_list: None,
        })
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
                &args(&["status"]),
                None,
            )
            .unwrap()
            .input,
            json!({"dir":"/tmp","copies":2,"verbose":true})
        );
        assert_eq!(
            resolve_command_argv(
                &args(&["-v", "status"]),
                &two_sided_schema(),
                &args(&["status"]),
                None,
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
            None,
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
                &args(&["cat-file"]),
                None,
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
        assert!(resolve_command_argv(&argv, &schema, &command, None).is_err());
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

    /// `find [option] path ... [expression]`: the two-variadic shape the
    /// `x-apexe-tail-parsed` marker exists for.
    fn find_schema() -> Value {
        json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "x-apexe-tail-literal": ["(", ")", "!", ","],
            "properties": {
                "path": {
                    "type": "array",
                    "items": {"type": "string", "x-apexe-path": true},
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "expression": {
                    "type": "array",
                    "items": {"type": "string"},
                    "x-apexe-positional": 1,
                    "x-apexe-tail-parsed": true
                },
                "L": {
                    "type": "boolean",
                    "x-apexe-flag": "-L",
                    "x-apexe-flag-position": "before-operands"
                },
                "name": {"type": "string", "x-apexe-flag": "-name"},
                "o": {"type": "boolean", "x-apexe-flag": "-o"},
                "not": {"type": "boolean", "x-apexe-flag": "-not"},
                "delete": {"type": "boolean", "x-apexe-flag": "-delete"},
                "print": {"type": "boolean", "x-apexe-flag": "-print"},
                "exec": {"type": "string", "x-apexe-flag": "-exec"}
            }
        })
    }

    fn find_argv(argv: &[&str]) -> ResolvedArgv {
        resolve_command_argv(&args(argv), &find_schema(), &[], Some("cli.find"))
            .unwrap_or_else(|error| panic!("{error:?}"))
    }

    /// The boundary is the first token matching a declared trailing flag: before
    /// it the leading operand keeps taking values, from it on the tokens are the
    /// command's expression.
    #[test]
    fn test_parsed_tail_splits_paths_from_the_expression() {
        let resolved = find_argv(&["/etc", "-name", "*.conf", "-print"]);
        assert_eq!(
            resolved.input,
            json!({"path": ["/etc"], "name": "*.conf", "print": true})
        );
        let tail = resolved.opaque_tail.expect("the expression is forwarded");
        assert_eq!(tail.argv, args(&["-name", "*.conf", "-print"]));
        assert_eq!(Value::Object(tail.prefix), json!({"path": ["/etc"]}));
        assert!(!tail.separator);
    }

    /// The security invariant: `-delete` must reach `input`, because that is
    /// what the approval gate and `x-apexe-writes-with` read. A tail that is
    /// forwarded without being parsed would run at read strength.
    #[test]
    fn test_parsed_tail_makes_a_write_flag_visible_to_the_input() {
        let resolved = find_argv(&[".", "-delete"]);
        assert_eq!(resolved.input, json!({"path": ["."], "delete": true}));
        assert_eq!(
            resolved.opaque_tail.expect("forwarded").argv,
            args(&["-delete"])
        );
    }

    /// `-exec`'s arguments are another program's argv. Refusing is the honest
    /// ceiling, and the refusal has to name the token and the way out.
    #[test]
    fn test_parsed_tail_refuses_a_token_that_is_not_a_flag() {
        let error = resolve_command_argv(
            &args(&[".", "-exec", "rm", "{}", ";"]),
            &find_schema(),
            &[],
            Some("cli.find"),
        )
        .unwrap_err();
        assert!(
            error.message.contains("'{}'") && error.message.contains("cannot be classified"),
            "{}",
            error.message
        );
        let guidance = error.ai_guidance.unwrap_or_default();
        assert!(
            guidance.contains("apexe exec cli.find --input"),
            "{guidance}"
        );
    }

    #[test]
    fn test_parsed_tail_refuses_a_flag_missing_its_value() {
        let error =
            resolve_command_argv(&args(&[".", "-name"]), &find_schema(), &[], None).unwrap_err();
        assert!(
            error.message.contains("requires a value"),
            "{}",
            error.message
        );
    }

    /// The rendered module JSON puts a pre-operand option ahead of the paths
    /// (`find -L . -name x`), so the resolver has to accept that spelling too or
    /// the shim and `--input` disagree about the same call.
    #[test]
    fn test_parsed_tail_keeps_a_pre_operand_option_ahead_of_the_paths() {
        let resolved = find_argv(&["-L", ".", "-name", "x"]);
        assert_eq!(
            resolved.input,
            json!({"L": true, "path": ["."], "name": "x"})
        );
        assert_eq!(
            resolved.opaque_tail.expect("forwarded").argv,
            args(&["-name", "x"])
        );
    }

    /// A separator before the boundary must not turn a write flag behind it
    /// into an invisible operand: `find -- . -delete` deletes.
    #[test]
    fn test_parsed_tail_still_sees_a_write_flag_after_the_separator() {
        let resolved = find_argv(&["--", ".", "-delete"]);
        assert_eq!(resolved.input, json!({"path": ["."], "delete": true}));
        let tail = resolved.opaque_tail.expect("forwarded");
        assert_eq!(tail.argv, args(&["-delete"]));
        assert!(tail.separator, "the caller's separator must survive");
    }

    #[test]
    fn test_parsed_tail_marker_must_be_last_and_variadic() {
        let schema = json!({"type":"object","properties":{
            "expression":{"type":"string","x-apexe-positional":0,
                          "x-apexe-tail-parsed":true}
        }});
        let error = resolve_argv(&args(&["-name", "x"]), &schema).unwrap_err();
        assert!(
            error.message.contains("must sit on the last operand"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_two_variadic_operands_stay_ambiguous_without_the_marker() {
        let mut schema = find_schema();
        schema["properties"]["expression"]
            .as_object_mut()
            .unwrap()
            .remove("x-apexe-tail-parsed");
        assert!(resolve_argv(&args(&["-name", "x"]), &schema).is_err());
    }

    #[test]
    fn test_opaque_and_parsed_tails_on_one_operand_are_refused() {
        let mut schema = find_schema();
        schema["properties"]["expression"]["x-apexe-opaque-after"] = json!(true);
        let error = resolve_argv(&args(&[".", "-name", "x"]), &schema).unwrap_err();
        assert!(
            error
                .message
                .contains("opposite treatments of the same tail"),
            "{}",
            error.message
        );
    }

    /// A declared non-dash token opens the expression: `find . \( -name a \)
    /// is paths then operators, not a path named `(`.
    #[test]
    fn test_tail_token_starts_the_parsed_tail() {
        let resolved = find_argv(&[".", "(", "-name", "a", "-o", "-name", "b", ")"]);
        assert_eq!(
            resolved.input,
            json!({"path": ["."], "name": "b", "o": true})
        );
        let tail = resolved.opaque_tail.expect("forwarded");
        assert_eq!(
            tail.argv,
            args(&["(", "-name", "a", "-o", "-name", "b", ")"])
        );
        assert_eq!(Value::Object(tail.prefix), json!({"path": ["."]}));
    }

    #[test]
    fn test_tail_tokens_are_skipped_inside_the_tail() {
        // `!` opens the expression the same way `(` does, so `.` is the only
        // path; the punctuation after it is skipped while the flags are parsed.
        let resolved = find_argv(&[".", "!", "-name", "a", ",", "-print"]);
        assert_eq!(
            resolved.input,
            json!({"path": ["."], "name": "a", "print": true})
        );
        assert_eq!(
            resolved.opaque_tail.expect("forwarded").argv,
            args(&["!", "-name", "a", ",", "-print"])
        );

        let grouped = find_argv(&[".", "(", "!", "-name", "a", ")", "-print"]);
        assert_eq!(
            grouped.input,
            json!({"path": ["."], "name": "a", "print": true})
        );
    }

    #[test]
    fn test_tail_tokens_do_not_reopen_the_exec_ceiling() {
        let error = resolve_command_argv(
            &args(&[".", "-exec", "rm", "{}", ";"]),
            &find_schema(),
            &[],
            Some("cli.find"),
        )
        .unwrap_err();
        assert!(
            error.message.contains("'{}'") && error.message.contains("cannot be classified"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_tail_tokens_require_a_parsed_tail() {
        let schema = json!({
            "type": "object",
            "x-apexe-tail-literal": ["("],
            "properties": {
                "expression": {
                    "type": "array",
                    "items": {"type": "string"},
                    "x-apexe-positional": 0
                }
            }
        });
        let error = resolve_argv(&args(&[".", "(", "-name", "a", ")"]), &schema).unwrap_err();
        assert!(
            error.message.contains("x-apexe-tail-literal"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_tail_tokens_refuse_exec_terminators_and_flags() {
        for bad in [";", "+", "{}", "-name"] {
            let mut schema = find_schema();
            schema["x-apexe-tail-literal"] = json!([bad]);
            let error = resolve_argv(&args(&[".", "-name", "x"]), &schema).unwrap_err();
            assert!(
                error.message.contains("x-apexe-tail-literal"),
                "{bad}: {}",
                error.message
            );
        }
    }

    /// `find -exec rm -rf {} ; -print`: the list is collected verbatim, its
    /// tokens are not classified as find flags, and the flags after the
    /// terminator are still parsed.
    #[test]
    fn test_exec_list_is_collected_and_not_parsed_as_flags() {
        let mut schema = find_schema();
        schema["properties"]["exec"]["x-apexe-exec-list"] = json!(true);
        let resolved = resolve_command_argv(
            &args(&[".", "-exec", "rm", "-rf", "{}", ";", "-print"]),
            &schema,
            &[],
            Some("cli.find"),
        )
        .unwrap();
        assert_eq!(resolved.input["exec"], json!("rm"));
        assert_eq!(resolved.input["path"], json!(["."]));
        assert_eq!(resolved.input["print"], json!(true));
        assert_eq!(resolved.exec_lists.len(), 1);
        assert_eq!(
            resolved.exec_lists[0].tokens,
            args(&["rm", "-rf", "{}", ";"])
        );
        let tail = resolved.opaque_tail.expect("forwarded");
        assert_eq!(
            tail.argv,
            args(&["-exec", "rm", "-rf", "{}", ";", "-print"])
        );
    }

    #[test]
    fn test_exec_list_requires_a_terminator() {
        let mut schema = find_schema();
        schema["properties"]["exec"]["x-apexe-exec-list"] = json!(true);
        let error = resolve_command_argv(
            &args(&[".", "-exec", "rm", "{}"]),
            &schema,
            &[],
            Some("cli.find"),
        )
        .unwrap_err();
        assert!(
            error.message.contains("must end with ';' or '+'"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_multiple_exec_lists_are_recorded_in_order() {
        let mut schema = find_schema();
        schema["properties"]["exec"]["x-apexe-exec-list"] = json!(true);
        let resolved = resolve_command_argv(
            &args(&[".", "-exec", "rm", "{}", ";", "-exec", "cat", "{}", "+"]),
            &schema,
            &[],
            Some("cli.find"),
        )
        .unwrap();
        assert_eq!(resolved.exec_lists.len(), 2);
        assert_eq!(resolved.exec_lists[0].tokens[0], "rm");
        assert_eq!(resolved.exec_lists[1].tokens[0], "cat");
        assert_eq!(resolved.input["exec"], json!("cat"));
    }

    #[test]
    fn test_exec_list_refuses_an_attached_value() {
        let mut schema = find_schema();
        schema["properties"]["exec"]["x-apexe-exec-list"] = json!(true);
        schema["properties"]["exec"]["x-apexe-flag"] = json!("--exec");
        let error = resolve_argv(&args(&["--exec=rm", ";"]), &schema).unwrap_err();
        assert!(
            error.message.contains("takes no attached value"),
            "{}",
            error.message
        );
    }

    #[test]
    fn test_grouping_survives_the_separator() {
        let resolved = find_argv(&["--", ".", "(", "-name", "a", ")"]);
        assert_eq!(resolved.input, json!({"path": ["."], "name": "a"}));
        let tail = resolved.opaque_tail.expect("forwarded");
        assert_eq!(tail.argv, args(&["(", "-name", "a", ")"]));
        assert!(tail.separator);
    }
}
