use apcore::{ErrorCode, ModuleError};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Characters that MUST NOT appear in values taken from a binding file.
///
/// `json_flag` and `command_parts` are build-time artifacts produced by a scan,
/// not runtime data: nothing legitimate puts a shell metacharacter in them, so
/// the conservative set is kept as a tamper signal for binding files edited by
/// hand or in transit. Runtime parameter values are a different trust class and
/// go through [`validate_argument_value`] instead.
const BINDING_INJECTION_CHARS: &[char] = &[
    ';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r', '\0', '(', ')', '<', '>',
];

/// Characters rejected in *any* argument value, whatever its source.
///
/// This is deliberately tiny. Every argument reaches the child through
/// [`execute_subprocess`], which builds a `Command` and passes argv straight to
/// `execve` — no shell is spawned anywhere on the execution path (the one
/// `sh`/`zsh` invocation in the crate, in `scanner::completion`, runs a
/// hardcoded constant script with no interpolation). Shell metacharacters are
/// therefore ordinary bytes in argv: `;`, `|`, `&`, `$`, backtick and quotes
/// cannot start a command, open a pipe, or expand a variable. Rejecting them
/// bought no safety and cost the tool's core use cases — `curl` could not send
/// a JSON body (`{"a":1}`) or a form (`a=1&b=2`), and no `jq` filter containing
/// `|`, `(`, `)` or `$` could be passed at all.
///
/// What remains:
/// - `\0` — `Command` rejects it anyway; catching it here gives a better error.
/// - `\n` / `\r` — not an execve risk, but they corrupt the audit log's line
///   framing and any line-oriented protocol the wrapped tool speaks.
/// - U+2028 LINE SEPARATOR, U+2029 PARAGRAPH SEPARATOR and U+0085 NEL — the
///   same framing argument, for the consumers that split on them (issue #39).
///   The default-on logging middleware writes an input value into the log
///   verbatim, so a caller that can plant one of these decides where a log
///   reader sees a line boundary: the same 62-physical-line audit trail is 71
///   lines under the Unicode-aware line splitting that JavaScript, Java and
///   many log processors apply. A rejection is worth more than the value, for
///   the same reason `\n` is rejected — nothing legitimate puts a line
///   terminator in a command-line argument.
///
/// What is deliberately *not* here: `\t`, VT (U+000B), FF (U+000C) and the ANSI
/// escape introducer (U+001B). They are the other half of the "control
/// character in a log" family, and they are already neutralised on the way out:
/// `tracing` renders a field through `Debug`, which escapes them as `\t` and
/// `\u{1b}` rather than emitting them raw, so none can start an escape sequence
/// or move a cursor in a consumer's terminal. Nor do they mean anything to a
/// line splitter. The three added above are in the set precisely because the
/// reported log line shows them arriving raw. Rejecting the rest would cost
/// real values — a tab-separated `--data` body — for no gain.
const CONTROL_CHARS: &[char] = &['\0', '\n', '\r', '\u{2028}', '\u{2029}', '\u{0085}'];

/// Default cap on captured stdout/stderr per stream, matching apcore-cli's
/// `Sandbox::with_max_output_bytes` default. Prevents a runaway CLI tool
/// (e.g. one that dumps a multi-GB file to stdout) from exhausting memory.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Validate a value that came from a binding file (`json_flag`,
/// `command_parts`), rejecting the conservative [`BINDING_INJECTION_CHARS`] set.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn validate_no_injection(param_name: &str, value: &str) -> Result<(), ModuleError> {
    let found: Vec<char> = value
        .chars()
        .filter(|c| BINDING_INJECTION_CHARS.contains(c))
        .collect();
    if !found.is_empty() {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Parameter '{}' contains prohibited characters: {:?}",
                param_name, found
            ),
        ));
    }
    Ok(())
}

/// Where one caller-supplied value sits in the input object.
///
/// Carried into the error message so a rejection names the token that caused
/// it. An array property scans every element but used to report as though the
/// first one were at fault: `{"expression": ["(", "-name", "*.txt", ")"]}` was
/// refused with "Parameter 'expression' starts with '-'", and `"("` does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueLocation<'a> {
    /// Property key the value arrived under.
    pub param_name: &'a str,
    /// Position within the property's array, or `None` for a scalar property.
    pub index: Option<usize>,
}

impl ValueLocation<'_> {
    /// Render this location as the subject of a sentence.
    fn describe(&self) -> String {
        match self.index {
            Some(index) => format!("Element {index} of parameter '{}'", self.param_name),
            None => format!("Parameter '{}'", self.param_name),
        }
    }
}

/// Validate a caller-supplied parameter value before it becomes one argv entry.
///
/// Two checks, both about what a value can do *as an argument* rather than what
/// it would mean to a shell that never runs:
///
/// 1. [`CONTROL_CHARS`] are rejected outright.
/// 2. A value that starts with `-` is rejected, because the wrapped tool's own
///    parser reads argv positionally: a value of `--output=/etc/passwd` or `-K
///    /etc/shadow` is indistinguishable from an option the caller was never
///    granted. This is the injection that actually exists on an execve path,
///    and the previous shell-metacharacter blacklist did not cover it — `-` was
///    not in that set. It matters most for values that reach argv bare (a
///    positional argument) and for options whose argument is optional, where
///    the tool will happily treat the next `-`-prefixed token as a new flag.
///
/// `numeric` exempts check 2 for values that arrived as a JSON number, so a
/// legitimate negative number still works. A *string* `"-1"` is still rejected:
/// the caller asked for text, and honoring a leading `-` in text is exactly the
/// ambiguity being closed.
///
/// `separator_available` exempts check 2 as well, and is set by
/// [`build_arguments`] only
/// for a tool whose overlay states that it honours the `--` end-of-options
/// separator. Refusing was one way to keep a value from being read as an
/// option; `--` is the other, and it is the stronger of the two — after the
/// separator the wrapped parser *cannot* read the value as an option, whereas
/// refusing only establishes that the value did not look like one. It also
/// unblocks the parameters that exist for this exact case: every one of
/// `find`'s expression primaries begins with `-`, so the operand documented as
/// carrying them could not carry any legal value at all.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn validate_argument_value(
    location: ValueLocation<'_>,
    value: &str,
    numeric: bool,
    separator_available: bool,
) -> Result<(), ModuleError> {
    let found: Vec<char> = value
        .chars()
        .filter(|c| CONTROL_CHARS.contains(c))
        .collect();
    if !found.is_empty() {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "{} contains prohibited control characters: {:?}",
                location.describe(),
                found
            ),
        ));
    }

    if !numeric && !separator_available && value.starts_with('-') {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "{} is '{value}', which starts with '-'; the wrapped command \
                 would parse it as an option rather than a value",
                location.describe()
            ),
        ));
    }

    Ok(())
}

fn json_value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Where an operand sits relative to the command's flags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OperandPlacement {
    /// After every flag — the `tool [options] operands` grammar, and the default.
    AfterFlags,
    /// Before every flag — the `find path ... [expression]` grammar, where the
    /// options are per-file predicates that must follow the paths.
    BeforeFlags,
}

/// Where a flag sits relative to the command's operands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FlagPlacement {
    /// After the leading operands — the ordinary `tool [options] operands`
    /// position, and the default.
    Default,
    /// Ahead of every operand. `find`'s true options (`-L`, `-s`, `-f path`)
    /// must precede the paths even though its primaries must follow them.
    BeforeOperands,
    /// Ahead of the subcommand token itself, which is a step further out than
    /// [`FlagPlacement::BeforeOperands`]: a subcommand is not an operand, and
    /// the tokens naming it do not come from the input object at all — they
    /// come from the module's `exec://` target. The renderer therefore cannot
    /// place these itself; it reports them separately in
    /// [`RenderedArgv::before_subcommand`] and the module assembles the three
    /// pieces. `git -C <dir> cat-file` is the shape; `git cat-file -C <dir>`
    /// is what the scanner used to emit, where `-C` belongs to a parser that
    /// never sees it.
    BeforeSubcommand,
}

/// How a single input property is spelled on the command line.
enum ArgForm {
    /// Passed behind a flag token, e.g. `-l` or `--output FILE`, on the stated
    /// side of the operands.
    Flag(String, FlagPlacement),
    /// Passed bare, at the given index among the command's operands, on the
    /// stated side of the flags.
    Positional(u64, OperandPlacement),
}

/// Decide how `key` is spelled, from the `x-apexe-*` annotations the scanner
/// wrote onto its schema property.
///
/// Falls back to `--{key-with-hyphens}` when the schema says nothing. Bindings
/// generated before these annotations existed carry no such markers, and
/// silently rendering nothing for them would turn a wrong command into a
/// missing argument; the fallback keeps their prior behaviour exactly.
fn arg_form(schema: Option<&Value>, key: &str) -> ArgForm {
    let property = schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(key));

    if let Some(index) = property
        .and_then(|p| p.get("x-apexe-positional"))
        .and_then(Value::as_u64)
    {
        let placement = match property
            .and_then(|p| p.get("x-apexe-operand-position"))
            .and_then(Value::as_str)
        {
            Some("before-flags") => OperandPlacement::BeforeFlags,
            // Absent or unrecognized keeps the prior behaviour, which is right
            // for every tool whose grammar is `tool [options] operands`.
            _ => OperandPlacement::AfterFlags,
        };
        return ArgForm::Positional(index, placement);
    }

    let placement = match property
        .and_then(|p| p.get("x-apexe-flag-position"))
        .and_then(Value::as_str)
    {
        Some("before-operands") => FlagPlacement::BeforeOperands,
        Some("before-subcommand") => FlagPlacement::BeforeSubcommand,
        // Absent or unrecognized keeps the prior behaviour.
        _ => FlagPlacement::Default,
    };

    if let Some(literal) = property
        .and_then(|p| p.get("x-apexe-flag"))
        .and_then(Value::as_str)
    {
        return ArgForm::Flag(literal.to_string(), placement);
    }

    ArgForm::Flag(format!("--{}", key.replace('_', "-")), placement)
}

/// The literal `--` end-of-options separator.
const END_OF_OPTIONS: &str = "--";

/// Whether the wrapped tool honours `--` as an end-of-options separator.
///
/// Read from the schema root, where a curated overlay's `end_of_options` lands.
/// Like `x-apexe-operand-position` and `x-apexe-flag-position` this is a
/// per-tool fact about the command's own argv parser that no help text or man
/// page states machine-readably, so it is asserted by a human or not at all.
/// Absent, [`build_arguments`] keeps refusing a value that begins with `-`,
/// which is the right answer for a tool nobody has checked: `find . -- -name
/// '*.txt'` is "unknown primary or operator" on BSD and "unknown predicate
/// `--'" on GNU, so an unverified guess at where the separator goes would
/// produce an invocation that fails instead of one that is merely refused.
fn honours_end_of_options(input_schema: Option<&Value>) -> bool {
    input_schema
        .and_then(|s| s.get("x-apexe-end-of-options"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether this grammar puts its operands ahead of the flags.
///
/// This is what decides where `--` goes, because option parsing stops at the
/// first operand: the separator has to sit at the point the tool stops reading
/// options, and that point is *before* the operands. For
/// `find [-f path] path ... [expression]` that is after the pre-operand options
/// and ahead of the paths — `find -- . -name '*.txt'` works on BSD and GNU
/// alike, while `find . -- -name '*.txt'` is rejected by both. For the ordinary
/// `tool [options] operands` grammar it is after every flag instead.
///
/// Answered from the schema rather than from the values in hand, because a
/// `find` call that supplies its paths through `-f` alone renders no leading
/// operand at all and the separator still belongs at the same place:
/// `find -f dir -- -name x` works where `find -f dir -name x` is
/// "illegal option -- n".
fn operands_precede_flags(input_schema: Option<&Value>) -> bool {
    input_schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties.values().any(|property| {
                property
                    .get("x-apexe-operand-position")
                    .and_then(Value::as_str)
                    == Some("before-flags")
            })
        })
}

/// Whether this property's value must be attached to its flag token with `=`.
///
/// True for exactly the options whose value is *optional*, which the scanner
/// marks with `x-apexe-value-optional`. Those accept no other spelling: GNU
/// `ls --classify never` reads `never` as a file to list and fails, while
/// `--classify=never` works.
fn value_must_be_attached(input_schema: Option<&Value>, key: &str) -> bool {
    input_schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(key))
        .and_then(|p| p.get("x-apexe-value-optional"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether a value will actually reach the command line.
///
/// `false` and null render nothing, so naming such a key is not "sending" the
/// flag and must not trip the conflict check — `{"f": true, "i": false}` is a
/// perfectly good way to say "force, not interactive".
fn is_effective(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

/// Reject inputs that name two flags the tool cannot take together.
///
/// Without this the pair is not an error but something worse: it renders, and
/// which of the two wins is decided by the order the caller happened to write
/// the JSON keys in. An object has no ordering anyone intended as an interface,
/// so `{"f": true, "i": true}` on `rm` is not "interactive wins" — it is
/// whichever key was typed last. Failing closed turns an unpredictable
/// invocation into a message naming both flags.
///
/// Only curated overlays state mutual exclusion, so this fires only where a
/// human recorded it; a schema without `x-apexe-conflicts-with` is unaffected.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
fn reject_conflicting_flags(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
) -> Result<(), ModuleError> {
    let Some(properties) = input_schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };

    for (key, value) in kwargs {
        if !is_effective(value) {
            continue;
        }
        let Some(conflicts) = properties
            .get(key)
            .and_then(|p| p.get("x-apexe-conflicts-with"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for other in conflicts.iter().filter_map(Value::as_str) {
            // Report each pair once: the relation is declared on both flags.
            if other <= key.as_str() {
                continue;
            }
            if kwargs.get(other).is_some_and(is_effective) {
                return Err(ModuleError::new(
                    ErrorCode::GeneralInvalidInput,
                    format!(
                        "Parameters '{key}' and '{other}' cannot be used together; \
                         send one or the other"
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// Refuse to emit a bare flag token for an option the schema says takes a value.
///
/// A bare `--proxy` is not a no-op: the tool reads whatever argv token comes
/// next as its value, so `{"proxy": true, "connect_timeout": true}` rendered
/// `--proxy --connect-timeout` and curl dialled a proxy named
/// `--connect-timeout`. That is the failure mode `x-apexe-conflicts-with`
/// exists to prevent — an invocation that runs, and does something the caller
/// never asked for — reached here from the renderer's own output rather than
/// from the caller's key order.
///
/// Schema validation upstream rejects a boolean for a `string` property, so in
/// the served path this never fires. It is the backstop for the paths that do
/// not validate first — `preview`, and bindings whose schema was generated
/// before an option's value placeholder was recognized.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
fn reject_bare_flag_for_value_option(
    input_schema: Option<&Value>,
    key: &str,
    literal: &str,
) -> Result<(), ModuleError> {
    let declared_type = input_schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(key))
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str);

    // Only a stated non-boolean scalar type is treated as "takes a value".
    // An absent type, or `array`, says nothing either way and is left alone.
    if matches!(
        declared_type,
        Some("string" | "number" | "integer") // array/object/absent are not a claim about arity
    ) {
        return Err(ModuleError::new(
            ErrorCode::GeneralInvalidInput,
            format!(
                "Parameter '{key}' takes a value, so it cannot be sent as `true`: \
                 '{literal}' would be rendered bare and the wrapped command would \
                 read the next argument as its value"
            ),
        ));
    }
    Ok(())
}

/// One invocation's caller-supplied argv, split at the subcommand tokens.
///
/// The two halves cannot be a single list because the tokens that separate them
/// are not the renderer's to produce: `cli.git.cat_file` names its subcommand in
/// the module target (`exec:///usr/bin/git cat-file`), and only
/// [`crate::module::cli_module::CliModule`] holds those. The renderer therefore
/// reports where each group belongs and the module interleaves.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenderedArgv {
    /// Tokens that must precede the subcommand path, because the tool's *own*
    /// parser reads them before it dispatches — `git -C <dir>` in
    /// `git -C <dir> cat-file`.
    pub before_subcommand: Vec<String>,
    /// Everything else, already in the order the command's grammar wants.
    pub after_subcommand: Vec<String>,
}

impl RenderedArgv {
    /// Concatenate both halves into one list.
    ///
    /// Correct only where there is no subcommand token to sit between them —
    /// a root module, or any call whose schema marks no flag
    /// `before-subcommand`. Anything holding `command_parts` must interleave
    /// instead.
    pub fn into_flat(self) -> Vec<String> {
        let mut args = self.before_subcommand;
        args.extend(self.after_subcommand);
        args
    }
}

/// The five token groups a call's arguments sort into, before ordering.
///
/// Split out from [`build_argv`] so that function is the ordering rule and
/// nothing else: which group a token lands in is a per-argument decision, and
/// stating it under 90 lines of value rendering hid the one invariant that
/// actually matters — what comes after what.
#[derive(Debug, Default)]
struct ArgvGroups {
    /// Tokens for [`RenderedArgv::before_subcommand`]; see [`FlagPlacement`].
    before_subcommand: Vec<String>,
    /// Flags marked [`FlagPlacement::BeforeOperands`].
    leading_flags: Vec<String>,
    /// Flags in the ordinary post-operand position.
    trailing_flags: Vec<String>,
    /// `(operand index, tokens)` for operands preceding the flags. Sorted
    /// before use, so operands land in the order the usage line declared
    /// regardless of JSON key order.
    leading_operands: Vec<(u64, Vec<String>)>,
    /// `(operand index, tokens)` for operands following the flags.
    trailing_operands: Vec<(u64, Vec<String>)>,
    /// Whether any rendered value begins with `-`, and so needs the `--`
    /// separator's protection. Emitting `--` unconditionally would be legal for
    /// a tool that declares the marker, but it would rewrite every invocation
    /// of that tool for the sake of the few that need it.
    saw_dash_leading_value: bool,
}

impl ArgvGroups {
    /// Append `tokens` to the group `form` names.
    fn extend(&mut self, form: &ArgForm, tokens: Vec<String>) {
        match *form {
            ArgForm::Flag(_, FlagPlacement::BeforeSubcommand) => {
                self.before_subcommand.extend(tokens);
            }
            ArgForm::Flag(_, FlagPlacement::BeforeOperands) => self.leading_flags.extend(tokens),
            ArgForm::Flag(_, FlagPlacement::Default) => self.trailing_flags.extend(tokens),
            ArgForm::Positional(index, OperandPlacement::BeforeFlags) => {
                self.leading_operands.push((index, tokens));
            }
            ArgForm::Positional(index, OperandPlacement::AfterFlags) => {
                self.trailing_operands.push((index, tokens));
            }
        }
    }
}

/// Render one property's value or values into argv tokens.
///
/// `attached` selects the `--flag=value` spelling. An option whose value is
/// *optional* accepts only that form, so it is the one case `=` is required
/// for, and `x-apexe-value-optional` marks exactly those options.
///
/// Everything else takes two argv entries. `--flag=value` is a GNU convention
/// rather than a universal one, so the previous premise — that an option with a
/// required value accepts both spellings — is simply false: `curl` implements
/// its own parser and accepts NO long option in `=` form, answering
/// `--max-time=1` with "option --max-time=1: is unknown". That made 134 of
/// cli.curl's 258 properties unusable. The separated form is the one the help
/// text shows (`-m, --max-time <seconds>`) and GNU accepts it as well as `=`.
///
/// Short options were never attachable — `-I=PATTERN` would pass a value
/// beginning with `=` — so they take the separated form here too.
///
/// Every value is validated before it is emitted; `saw_dash` is set when one
/// begins with `-` under a schema that declares [`END_OF_OPTIONS`].
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
fn render_property_tokens(
    key: &str,
    value: &Value,
    form: &ArgForm,
    attached: bool,
    honours_separator: bool,
    saw_dash: &mut bool,
) -> Result<Vec<String>, ModuleError> {
    let (items, indexed): (Vec<&Value>, bool) = match value {
        Value::Array(items) => (items.iter().collect(), true),
        other => (vec![other], false),
    };

    let mut tokens: Vec<String> = Vec::new();
    for (position, item) in items.into_iter().enumerate() {
        let text = json_value_to_string(item);
        let location = ValueLocation {
            param_name: key,
            index: indexed.then_some(position),
        };
        validate_argument_value(location, &text, item.is_number(), honours_separator)?;
        if honours_separator && text.starts_with('-') {
            *saw_dash = true;
        }
        match *form {
            ArgForm::Flag(ref literal, _) if attached && literal.starts_with("--") => {
                tokens.push(format!("{literal}={text}"));
            }
            ArgForm::Flag(ref literal, _) => {
                tokens.push(literal.clone());
                tokens.push(text);
            }
            ArgForm::Positional(..) => tokens.push(text),
        }
    }
    Ok(tokens)
}

/// Sort each argument into its token group, rendering values along the way.
///
/// A bare operand cannot carry a boolean: there is no token to emit for `true`
/// and no way to express `false`. Skipping is the only coherent reading, and
/// matches how `false` is treated for flags.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
fn collect_argv_groups(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
    honours_separator: bool,
) -> Result<ArgvGroups, ModuleError> {
    let mut groups = ArgvGroups::default();

    for (key, value) in kwargs {
        let form = arg_form(input_schema, key);

        if let Value::Bool(enabled) = value {
            if let ArgForm::Flag(ref literal, _) = form {
                if *enabled {
                    reject_bare_flag_for_value_option(input_schema, key, literal)?;
                    groups.extend(&form, vec![literal.clone()]);
                }
            }
            continue;
        }
        if value.is_null() {
            continue;
        }

        let tokens = render_property_tokens(
            key,
            value,
            &form,
            value_must_be_attached(input_schema, key),
            honours_separator,
            &mut groups.saw_dash_leading_value,
        )?;
        groups.extend(&form, tokens);
    }

    groups.leading_operands.sort_by_key(|(index, _)| *index);
    groups.trailing_operands.sort_by_key(|(index, _)| *index);
    Ok(groups)
}

/// Whether this call needs a `--` separator emitted at all.
///
/// In an operands-first grammar it is the first *operand* that stops the tool
/// reading options, so a call that renders none leaves the trailing tokens
/// exposed to the option parser even though nothing in it begins with a dash:
/// `find -f dir -name '*.txt'` is "illegal option -- n", while
/// `find -f dir -- -name '*.txt'` works. This is reachable precisely because
/// `-f` supplies the paths in place of the operand, which is the case the
/// operand's own description points at.
fn needs_end_of_options(
    groups: &ArgvGroups,
    honours_separator: bool,
    separator_precedes_operands: bool,
) -> bool {
    let nothing_stops_option_parsing = separator_precedes_operands
        && groups.leading_operands.is_empty()
        && !(groups.trailing_flags.is_empty() && groups.trailing_operands.is_empty());
    honours_separator && (groups.saw_dash_leading_value || nothing_stops_option_parsing)
}

/// Build CLI args from JSON kwargs, spelling each one the way `input_schema`
/// says the wrapped tool accepts it, and flatten the result.
///
/// See [`build_argv`], which this wraps: use that wherever the module's
/// subcommand tokens have to be interleaved. Flattening is what every
/// subcommand-less tool wants, and it is what the argv-shape tests assert.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn build_arguments(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
) -> Result<Vec<String>, ModuleError> {
    Ok(build_argv(kwargs, input_schema)?.into_flat())
}

/// Build CLI args from JSON kwargs, spelling each one the way `input_schema`
/// says the wrapped tool accepts it.
///
/// Bool `true` emits the bare flag, `false` and null are skipped, and an array
/// repeats the flag once per element. Values marked `x-apexe-positional` are
/// emitted bare and ordered by their recorded index — an input object has no
/// ordering of its own, so without that index `cp source target` would depend
/// on the order the caller happened to write the JSON keys in.
///
/// Placement is per-token, not one global rule, because a single grammar can
/// put dash-tokens on both sides of the same operand. `find` is
/// `find [-H|-L|-P] [-EXdsx] [-f path] path ... [expression]`: its true
/// options must precede the paths (`find dir -L` is "unknown primary or
/// operator") while its primaries must follow them (`find -name '*.txt' dir`
/// is "illegal option -- n"). Four ordered groups are therefore emitted —
/// leading flags, leading operands, trailing flags, trailing operands — each
/// sorted by its own recorded index. A schema carrying neither marker yields
/// the ordinary `tool [options] operands` shape, unchanged.
/// See `x-apexe-flag-position` and `x-apexe-operand-position`.
///
/// A fifth group sits outside all four: a flag marked
/// `x-apexe-flag-position: "before-subcommand"` is reported in
/// [`RenderedArgv::before_subcommand`] rather than emitted here, because the
/// subcommand tokens it has to precede are not part of this function's output.
///
/// A tool whose schema carries `x-apexe-end-of-options` additionally gets a
/// `--` separator inserted at the point its parser stops reading options, as
/// soon as any value begins with `-`. That is what makes such a value legal
/// rather than refused; see [`validate_argument_value`] and
/// [`operands_precede_flags`].
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub fn build_argv(
    kwargs: &serde_json::Map<String, Value>,
    input_schema: Option<&Value>,
) -> Result<RenderedArgv, ModuleError> {
    reject_conflicting_flags(kwargs, input_schema)?;

    let honours_separator = honours_end_of_options(input_schema);
    let groups = collect_argv_groups(kwargs, input_schema, honours_separator)?;

    // The separator marks where the tool stops reading options, which is ahead
    // of whichever operand group its grammar puts first.
    let separator_precedes_operands = operands_precede_flags(input_schema);
    let needs_separator =
        needs_end_of_options(&groups, honours_separator, separator_precedes_operands);

    // This ordering is the contract; everything above only decides which group
    // a token belongs to.
    let mut args: Vec<String> = groups.leading_flags;
    if needs_separator && separator_precedes_operands {
        args.push(END_OF_OPTIONS.to_string());
    }
    args.extend(
        groups
            .leading_operands
            .into_iter()
            .flat_map(|(_, values)| values),
    );
    args.extend(groups.trailing_flags);
    if needs_separator && !separator_precedes_operands {
        args.push(END_OF_OPTIONS.to_string());
    }
    args.extend(
        groups
            .trailing_operands
            .into_iter()
            .flat_map(|(_, values)| values),
    );
    Ok(RenderedArgv {
        before_subcommand: groups.before_subcommand,
        after_subcommand: args,
    })
}

/// Output from a subprocess execution.
#[derive(Debug)]
pub struct SubprocessOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// `true` if `stdout` was cut short at `max_output_bytes`.
    pub stdout_truncated: bool,
    /// `true` if `stderr` was cut short at `max_output_bytes`.
    pub stderr_truncated: bool,
}

/// Truncate `bytes` to at most `max_len` bytes on a UTF-8 char boundary,
/// returning the decoded string and whether truncation occurred.
fn truncate_output(bytes: &[u8], max_len: usize) -> (String, bool) {
    if bytes.len() <= max_len {
        return (String::from_utf8_lossy(bytes).to_string(), false);
    }
    // Back up off a UTF-8 continuation byte (top two bits `10`) so the cut
    // lands on a char boundary; `bytes` has no `str::is_char_boundary`.
    let mut cut = max_len;
    while cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
        cut -= 1;
    }
    (String::from_utf8_lossy(&bytes[..cut]).to_string(), true)
}

/// Size of the scratch buffer used to pull each chunk out of the pipe in
/// [`read_up_to`]. Small and fixed, independent of `max_len`.
const READ_CHUNK_SIZE: usize = 8 * 1024;

/// Read up to `max_len` bytes from `reader`, stopping early at EOF.
///
/// Deliberately does *not* drain the reader to EOF: a runaway process (e.g.
/// one that never stops writing) would otherwise buffer unboundedly. Once
/// the cap is hit, the pipe is left unread; the writer sees backpressure and
/// blocks, which is resolved by the outer timeout killing the child.
///
/// Grows the output buffer incrementally as bytes actually arrive instead of
/// pre-allocating `max_len` upfront — `max_len` is a 64 MiB+ cap per stream,
/// and eagerly allocating it for every subprocess call (stdout *and*
/// stderr, times however many calls run concurrently) costs real memory
/// even when the actual output is a few bytes.
async fn read_up_to<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; READ_CHUNK_SIZE];
    while buf.len() < max_len {
        let to_read = READ_CHUNK_SIZE.min(max_len - buf.len());
        let n = reader.read(&mut chunk[..to_read]).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

/// Base environment variables passed through to a wrapped CLI subprocess.
///
/// A wrapped tool is invoked by an external agent over MCP/A2A, so it must not
/// inherit apexe's *full* process environment — that could hold secrets (API
/// tokens, cloud credentials) the agent should never see. The subprocess env is
/// scrubbed to this allowlist plus any `LC_*` locale variable; file-based
/// credentials under `$HOME` still work. (Passing tool-specific credential env
/// through is intended as a future opt-in config knob.)
const ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "TERM", "TZ", "TMPDIR",
];

/// Whether an environment variable is allowed through to a wrapped subprocess.
fn is_allowed_env(key: &str) -> bool {
    ENV_ALLOWLIST.contains(&key) || key.starts_with("LC_")
}

/// Report a failure to run or communicate with a wrapped binary.
fn execute_error(binary_path: &str, doing: &str, cause: impl std::fmt::Display) -> ModuleError {
    ModuleError::new(
        ErrorCode::ModuleExecuteError,
        format!("Failed to {doing} '{binary_path}': {cause}"),
    )
}

/// Spawn `binary_path` with a scrubbed environment and piped streams.
///
/// The environment is cleared and rebuilt from [`ENV_ALLOWLIST`], so secrets in
/// apexe's own environment do not leak to — or get surfaced by — the wrapped
/// tool. `stdin` is `/dev/null` so a tool that waits for input fails fast
/// instead of hanging, and `kill_on_drop` is what makes the caller's timeout
/// actually kill the process rather than leave an orphan.
///
/// The `kill_on_drop` guarantee is covered by
/// `test_execute_subprocess_kills_the_child_when_the_timeout_elapses`. The
/// `stdin` one deliberately is not: `cargo test` already hands the harness a
/// non-terminal stdin, so a child inheriting it behaves exactly like one given
/// `/dev/null` and no assertion can tell the two apart. A test that passes with
/// the line removed would report coverage it does not have.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
fn spawn_isolated(
    binary_path: &str,
    args: &[String],
) -> Result<tokio::process::Child, ModuleError> {
    let mut command = Command::new(binary_path);
    command
        .args(args)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in std::env::vars() {
        if is_allowed_env(&key) {
            command.env(key, value);
        }
    }
    command
        .spawn()
        .map_err(|e| execute_error(binary_path, "execute", e))
}

/// Drain both pipes and the exit status, capping each stream.
///
/// One extra byte past the cap is read so truncation can be detected. The three
/// are driven concurrently because reading one stream to completion while the
/// other stays unread deadlocks the child once its pipe buffer fills.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
async fn collect_output(
    child: &mut tokio::process::Child,
    binary_path: &str,
    max_output_bytes: usize,
) -> Result<SubprocessOutput, ModuleError> {
    let read_cap = max_output_bytes.saturating_add(1);
    // INVARIANT: stdout/stderr were configured Stdio::piped() in
    // `spawn_isolated`, so take() is Some.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let (stdout_bytes, stderr_bytes, status) = tokio::join!(
        read_up_to(&mut stdout_pipe, read_cap),
        read_up_to(&mut stderr_pipe, read_cap),
        child.wait(),
    );

    let stdout_bytes = stdout_bytes.map_err(|e| execute_error(binary_path, "read stdout of", e))?;
    let stderr_bytes = stderr_bytes.map_err(|e| execute_error(binary_path, "read stderr of", e))?;
    let status = status.map_err(|e| execute_error(binary_path, "wait on", e))?;

    let (stdout, stdout_truncated) = truncate_output(&stdout_bytes, max_output_bytes);
    let (stderr, stderr_truncated) = truncate_output(&stderr_bytes, max_output_bytes);
    Ok(SubprocessOutput {
        stdout,
        stderr,
        exit_code: status.code().unwrap_or(-1),
        stdout_truncated,
        stderr_truncated,
    })
}

/// Execute a subprocess with a timeout, capturing stdout/stderr.
///
/// Runs the given binary with args directly (no shell), optionally appending
/// json_flag parts. See [`spawn_isolated`] for the environment scrubbing and
/// [`collect_output`] for the output caps and the concurrent drain that avoids
/// pipe deadlock.
#[allow(clippy::result_large_err)] // ModuleError is 184 bytes; acceptable at crate boundary
pub async fn execute_subprocess(
    binary_path: &str,
    args: &[String],
    json_flag: Option<&str>,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<SubprocessOutput, ModuleError> {
    let mut full_args: Vec<String> = args.to_vec();
    if let Some(flag) = json_flag {
        full_args.extend(shell_words::split(flag).unwrap_or_default());
    }

    let run = async {
        let mut child = spawn_isolated(binary_path, &full_args)?;
        collect_output(&mut child, binary_path, max_output_bytes).await
    };

    let timeout_duration = std::time::Duration::from_millis(timeout_ms);
    match tokio::time::timeout(timeout_duration, run).await {
        Ok(result) => result,
        // `retryable` is deliberately left unset here: a killed process may
        // have partially completed a non-idempotent side effect (e.g. `rm
        // -rf` deleted some files before timing out), so whether a retry is
        // *safe* depends on the module's `idempotent` annotation, which this
        // function has no visibility into. `CliModule::execute` sets it.
        Err(_elapsed) => Err(ModuleError::new(
            ErrorCode::ModuleTimeout,
            format!("Command '{}' timed out after {}ms", binary_path, timeout_ms),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_arguments_string_value() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!("test.txt"));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--file", "test.txt"]);
    }

    #[test]
    fn test_build_arguments_boolean_true() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("all".to_string(), json!(true));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--all"]);
    }

    #[test]
    fn test_build_arguments_boolean_false() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("all".to_string(), json!(false));
        let args = build_arguments(&kwargs, None).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_arguments_places_marked_operands_before_flags() {
        // Regression (#20): `find` renders as `find path ... [expression]`, so
        // its predicates must follow the paths. Rendering `-name '*.txt' dir`
        // is rejected by the binary with "illegal option -- n", which made all
        // 98 of cli.find's predicate properties unusable.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "array",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "expression": { "type": "array", "x-apexe-positional": 1 },
                "name": { "type": "string", "x-apexe-flag": "-name" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("name".to_string(), json!("*.txt"));
        kwargs.insert("path".to_string(), json!(["sandbox"]));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["sandbox", "-name", "*.txt"]
        );
    }

    #[test]
    fn test_build_arguments_places_marked_flags_before_operands() {
        // Regression: `before_flags` alone was half a fix. `find`'s grammar is
        // `find [-L] path ... [expression]` — its true options precede the
        // paths while its primaries follow them, so marking only the operand
        // dragged the options behind the path too, where BSD find answers
        // "unknown primary or operator" and GNU "unknown predicate".
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "array",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "L": {
                    "type": "boolean",
                    "x-apexe-flag": "-L",
                    "x-apexe-flag-position": "before-operands"
                },
                "name": { "type": "string", "x-apexe-flag": "-name" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("name".to_string(), json!("*.txt"));
        kwargs.insert("path".to_string(), json!(["sandbox"]));
        kwargs.insert("L".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-L", "sandbox", "-name", "*.txt"],
            "options precede the path, primaries follow it"
        );
    }

    #[test]
    fn test_build_arguments_places_a_marked_value_flag_before_operands() {
        // `find -f path` carries a value and is still pre-operand.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "array",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "f": {
                    "type": "string",
                    "x-apexe-flag": "-f",
                    "x-apexe-flag-position": "before-operands"
                },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("path".to_string(), json!(["sandbox"]));
        kwargs.insert("f".to_string(), json!("extra"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-f", "extra", "sandbox"]
        );
    }

    #[test]
    fn test_build_arguments_keeps_unmarked_flags_after_leading_operands() {
        // The default must not move: a tool that marks its operand but not its
        // flags keeps every flag in the trailing group.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "array",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "name": { "type": "string", "x-apexe-flag": "-name" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("name".to_string(), json!("*.txt"));
        kwargs.insert("path".to_string(), json!(["dir"]));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["dir", "-name", "*.txt"]
        );
    }

    #[test]
    fn test_build_arguments_keeps_unmarked_operands_after_flags() {
        // The default must not move: `tool [options] operands` is the grammar
        // of nearly everything else, and `ls -l dir` would break if it did.
        let schema = json!({
            "type": "object",
            "properties": {
                "file": { "type": "array", "x-apexe-positional": 0 },
                "l": { "type": "boolean", "x-apexe-flag": "-l" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!(["dir"]));
        kwargs.insert("l".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-l", "dir"]
        );
    }

    #[test]
    fn test_build_arguments_orders_operands_on_both_sides_of_the_flags() {
        // `find` has one operand on each side, and each side keeps its own
        // declared order — which is why placement is a separate keyword rather
        // than a sign on the index.
        let schema = json!({
            "type": "object",
            "properties": {
                "root": {
                    "type": "string",
                    "x-apexe-positional": 1,
                    "x-apexe-operand-position": "before-flags"
                },
                "start": {
                    "type": "string",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "tail": { "type": "string", "x-apexe-positional": 0 },
                "v": { "type": "boolean", "x-apexe-flag": "-v" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("tail".to_string(), json!("last"));
        kwargs.insert("root".to_string(), json!("second"));
        kwargs.insert("v".to_string(), json!(true));
        kwargs.insert("start".to_string(), json!("first"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["first", "second", "-v", "last"]
        );
    }

    #[test]
    fn test_build_arguments_rejects_true_for_a_value_taking_option() {
        // Regression (#21): `--proxy` typed boolean rendered a bare `--proxy`,
        // and curl read the *next* flag as the proxy address. A flag that takes
        // a value has no bare spelling, so `true` has to fail rather than
        // render something the caller did not ask for.
        let schema = json!({
            "type": "object",
            "properties": {
                "proxy": { "type": "string", "x-apexe-flag": "--proxy" },
                "connect_timeout": { "type": "number", "x-apexe-flag": "--connect-timeout" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("proxy".to_string(), json!(true));
        kwargs.insert("connect_timeout".to_string(), json!(true));

        let err = build_arguments(&kwargs, Some(&schema))
            .expect_err("a value-taking option must not render bare");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(
            err.message.contains("takes a value"),
            "unhelpful message: {}",
            err.message
        );
    }

    #[test]
    fn test_build_arguments_still_renders_declared_boolean_flags() {
        let schema = json!({
            "type": "object",
            "properties": { "verbose": { "type": "boolean", "x-apexe-flag": "-v" } }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("verbose".to_string(), json!(true));
        assert_eq!(build_arguments(&kwargs, Some(&schema)).unwrap(), vec!["-v"]);
    }

    #[test]
    fn test_build_arguments_null_skipped() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("x".to_string(), json!(null));
        let args = build_arguments(&kwargs, None).unwrap();
        assert!(args.is_empty());
    }

    #[test]
    fn test_build_arguments_array_values() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("include".to_string(), json!(["a", "b"]));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--include", "a", "--include", "b"]);
    }

    #[test]
    fn test_build_arguments_underscore_to_hyphen() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("no_cache".to_string(), json!(true));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--no-cache"]);
    }

    #[test]
    fn test_build_arguments_integer_value() {
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("count".to_string(), json!(5));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--count", "5"]);
    }

    #[test]
    fn test_build_arguments_option_like_value_blocked() {
        // The injection that exists on an execve path: a value the wrapped
        // tool's own parser would read as an option. `--output=` would make
        // curl write to an arbitrary file.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!("--output=/etc/passwd"));
        let result = build_arguments(&kwargs, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_build_arguments_option_like_value_blocked_in_array() {
        // F2 §5.5: array elements go through the same validation as scalars.
        // The array branch re-emits `--flag item` per element, so a `-`-leading
        // element is exactly where a variadic option would absorb it as a flag.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("tags".to_string(), json!(["ok", "-K/etc/shadow"]));
        let result = build_arguments(&kwargs, None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_build_arguments_allows_shell_metacharacters() {
        // No shell runs on the execution path, so these are ordinary argv
        // bytes. Rejecting them used to make curl unable to POST a JSON body
        // and jq unable to receive any filter at all.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!(r#"{"name":"apexe"}"#));
        let args = build_arguments(&kwargs, None).expect("a JSON body must be passable");
        assert_eq!(args, vec!["--data", r#"{"name":"apexe"}"#]);

        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!("name=apexe&lang=rust"));
        let args = build_arguments(&kwargs, None).expect("a form body must be passable");
        assert_eq!(args, vec!["--data", "name=apexe&lang=rust"]);

        let mut kwargs = serde_json::Map::new();
        kwargs.insert("filter".to_string(), json!(r#".items[] | select(.n > $x)"#));
        let args = build_arguments(&kwargs, None).expect("a jq filter must be passable");
        assert_eq!(args, vec!["--filter", r#".items[] | select(.n > $x)"#]);
    }

    #[test]
    fn test_build_arguments_rejects_control_characters() {
        for bad in ["a\nb", "a\rb", "a\0b"] {
            let mut kwargs = serde_json::Map::new();
            kwargs.insert("msg".to_string(), json!(bad));
            let result = build_arguments(&kwargs, None);
            assert!(
                result.is_err(),
                "control character in {bad:?} must be rejected"
            );
            assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
        }
    }

    #[test]
    fn test_build_arguments_rejects_unicode_line_terminators() {
        // Regression (#39): U+2028, U+2029 and U+0085 passed the guard and
        // reached the default-on logging middleware verbatim, so a caller could
        // decide where a log consumer sees a line boundary — the same audit
        // trail read as 62 lines by a `\n` splitter and 71 by a Unicode-aware
        // one. Same framing argument that already justifies rejecting `\n`.
        for bad in [
            "/tmp\u{2028}INJECTED",
            "/tmp\u{2029}INJECTED",
            "/tmp\u{0085}INJECTED",
        ] {
            let mut kwargs = serde_json::Map::new();
            kwargs.insert("msg".to_string(), json!(bad));
            let err = build_arguments(&kwargs, None)
                .expect_err("a Unicode line terminator must be rejected");
            assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
            assert!(
                err.message.contains("prohibited control characters"),
                "the message must name the rule: {}",
                err.message
            );
        }
    }

    #[test]
    fn test_build_arguments_allows_the_escaped_control_characters() {
        // The negative half, and the reason the set stops where it does: `\t`,
        // VT, FF and the ANSI escape introducer are rendered escaped by the
        // logging path, so rejecting them would cost real values (a
        // tab-separated `--data` body) for no framing gain.
        for allowed in ["a\tb", "a\u{000b}b", "a\u{000c}b", "a\u{001b}[31mb"] {
            let mut kwargs = serde_json::Map::new();
            kwargs.insert("data".to_string(), json!(allowed));
            assert!(
                build_arguments(&kwargs, None).is_ok(),
                "{allowed:?} must still be passable"
            );
        }
    }

    #[test]
    fn test_build_argv_reports_global_flags_before_the_subcommand() {
        // Regression (#39): `cli.git.cat_file {"C": "/repo"}` rendered
        // `git cat-file -C /repo`, where `-C` reaches git-cat-file's parser
        // rather than git's own. The subcommand tokens are not this function's
        // to emit, so the group is reported separately.
        let schema = json!({
            "type": "object",
            "properties": {
                "C": {
                    "type": "string",
                    "x-apexe-flag": "-C",
                    "x-apexe-flag-position": "before-subcommand"
                },
                "paginate": {
                    "type": "boolean",
                    "x-apexe-flag": "--paginate",
                    "x-apexe-flag-position": "before-subcommand"
                },
                "oneline": { "type": "boolean", "x-apexe-flag": "--oneline" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("C".to_string(), json!("/repo"));
        kwargs.insert("paginate".to_string(), json!(true));
        kwargs.insert("oneline".to_string(), json!(true));

        let argv = build_argv(&kwargs, Some(&schema)).unwrap();
        assert_eq!(argv.before_subcommand, vec!["-C", "/repo", "--paginate"]);
        assert_eq!(argv.after_subcommand, vec!["--oneline"]);
    }

    #[test]
    fn test_build_argv_keeps_unmarked_flags_after_the_subcommand() {
        // The default must not move: a schema with no `before-subcommand`
        // marker renders exactly as it did, with an empty leading group.
        let schema = json!({
            "type": "object",
            "properties": { "oneline": { "type": "boolean", "x-apexe-flag": "--oneline" } }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("oneline".to_string(), json!(true));

        let argv = build_argv(&kwargs, Some(&schema)).unwrap();
        assert!(argv.before_subcommand.is_empty());
        assert_eq!(argv.after_subcommand, vec!["--oneline"]);
    }

    #[test]
    fn test_build_argv_keeps_the_three_flag_groups_apart() {
        // `before-subcommand` is a third position, not a rename of
        // `before-operands`: a subcommand token is not an operand, so a grammar
        // can need both at once and the two groups must not merge.
        let schema = json!({
            "type": "object",
            "properties": {
                "C": {
                    "type": "string",
                    "x-apexe-flag": "-C",
                    "x-apexe-flag-position": "before-subcommand"
                },
                "f": {
                    "type": "string",
                    "x-apexe-flag": "-f",
                    "x-apexe-flag-position": "before-operands"
                },
                "path": { "type": "array", "x-apexe-positional": 0,
                          "x-apexe-operand-position": "before-flags" },
                "name": { "type": "string", "x-apexe-flag": "-name" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("C".to_string(), json!("/repo"));
        kwargs.insert("f".to_string(), json!("dir"));
        kwargs.insert("path".to_string(), json!(["root"]));
        kwargs.insert("name".to_string(), json!("*.txt"));

        let argv = build_argv(&kwargs, Some(&schema)).unwrap();
        assert_eq!(argv.before_subcommand, vec!["-C", "/repo"]);
        assert_eq!(
            argv.after_subcommand,
            vec!["-f", "dir", "root", "-name", "*.txt"]
        );
    }

    #[test]
    fn test_build_arguments_flattens_the_leading_group_first() {
        // The flat form is what a subcommand-less tool wants, and the leading
        // group still has to come first — flattening must not silently reorder.
        let schema = json!({
            "type": "object",
            "properties": {
                "C": {
                    "type": "string",
                    "x-apexe-flag": "-C",
                    "x-apexe-flag-position": "before-subcommand"
                },
                "oneline": { "type": "boolean", "x-apexe-flag": "--oneline" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("oneline".to_string(), json!(true));
        kwargs.insert("C".to_string(), json!("/repo"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-C", "/repo", "--oneline"]
        );
    }

    #[test]
    fn test_build_arguments_negative_number_allowed() {
        // A JSON number is type-evidence that the caller meant a value, so a
        // legitimate negative number survives the `-` prefix rule.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("offset".to_string(), json!(-5));
        let args = build_arguments(&kwargs, None).unwrap();
        assert_eq!(args, vec!["--offset", "-5"]);
    }

    #[test]
    fn test_build_arguments_negative_number_as_string_blocked() {
        // ...but the same thing typed as a string is not: the caller asked for
        // text, and a leading `-` in text is the ambiguity being closed.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("offset".to_string(), json!("-5"));
        assert!(build_arguments(&kwargs, None).is_err());
    }

    /// A schema shaped like the scanner emits, for the given properties.
    fn schema_with(properties: Value) -> Value {
        json!({ "type": "object", "properties": properties })
    }

    #[test]
    fn test_build_arguments_short_flag_uses_single_dash() {
        // 45 of `ls`'s 47 properties are single-character short options. The
        // key `l` used to render as `--l`, which BSD ls rejects outright, so
        // the module could not pass any of them.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l" },
            "a": { "type": "boolean", "x-apexe-flag": "-a" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("a".to_string(), json!(true));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-l", "-a"]);
    }

    #[test]
    fn test_build_arguments_rejects_conflicting_flags() {
        // Both would render, and which one the tool honours would come down to
        // the order the caller wrote the keys in.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["1"] },
            "1": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("1".to_string(), json!(true));

        let err = build_arguments(&kwargs, Some(&schema)).unwrap_err();
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
        assert!(
            err.message.contains('l') && err.message.contains('1'),
            "the error should name both flags: {}",
            err.message
        );
    }

    #[test]
    fn test_build_arguments_conflict_is_order_independent() {
        // The rejection must not depend on which key came first -- that is the
        // whole defect being closed.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["1"] },
            "1": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
        }));

        for keys in [["l", "1"], ["1", "l"]] {
            let mut kwargs = serde_json::Map::new();
            for key in keys {
                kwargs.insert(key.to_string(), json!(true));
            }
            assert!(
                build_arguments(&kwargs, Some(&schema)).is_err(),
                "order {keys:?} must be rejected too"
            );
        }
    }

    #[test]
    fn test_build_arguments_disabled_flag_is_not_a_conflict() {
        // `false` renders nothing, so naming it is not sending it -- this is a
        // legitimate way to say "long listing, not single-column".
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l", "x-apexe-conflicts-with": ["1"] },
            "1": { "type": "boolean", "x-apexe-flag": "-1", "x-apexe-conflicts-with": ["l"] },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("1".to_string(), json!(false));

        assert_eq!(build_arguments(&kwargs, Some(&schema)).unwrap(), vec!["-l"]);
    }

    #[test]
    fn test_build_arguments_without_conflict_annotations_is_unaffected() {
        // Only curated overlays state mutual exclusion; a schema that says
        // nothing must keep rendering exactly as before.
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l" },
            "1": { "type": "boolean", "x-apexe-flag": "-1" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("l".to_string(), json!(true));
        kwargs.insert("1".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-l", "-1"]
        );
    }

    #[test]
    fn test_build_arguments_optional_value_long_flag_uses_equals() {
        // GNU `ls --classify never` lists a file called `never` and fails;
        // only `--classify=never` works, because the option's value is
        // optional. `x-apexe-value-optional` marks exactly those options.
        let schema = schema_with(json!({
            "classify": {
                "type": ["string", "boolean"],
                "x-apexe-flag": "--classify",
                "x-apexe-value-optional": true,
            },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("classify".to_string(), json!("never"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["--classify=never"]);
    }

    #[test]
    fn test_build_arguments_required_value_long_flag_stays_separate() {
        // Regression (#24): every long option used to render `--flag=value`,
        // on the premise that an option with a *required* value accepts both
        // spellings. That is a GNU convention, not a universal one. `curl`
        // implements its own parser and accepts no long option in `=` form --
        // `curl --max-time=1` is "option --max-time=1: is unknown" -- which
        // made 134 of cli.curl's 258 properties unusable.
        let schema = schema_with(json!({
            "max_time": { "type": "number", "x-apexe-flag": "--max-time" },
            "user_agent": { "type": "string", "x-apexe-flag": "--user-agent" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("max_time".to_string(), json!(1));
        kwargs.insert("user_agent".to_string(), json!("apexe"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["--max-time", "1", "--user-agent", "apexe"]
        );
    }

    #[test]
    fn test_build_arguments_optional_value_short_flag_stays_separate() {
        // The `=` spelling is a long-option form only: `-I=PATTERN` would pass
        // a value that begins with `=`, so a short option keeps two entries
        // even when its value is optional.
        let schema = schema_with(json!({
            "I": {
                "type": ["string", "boolean"],
                "x-apexe-flag": "-I",
                "x-apexe-value-optional": true,
            },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("I".to_string(), json!("*.tmp"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-I", "*.tmp"]
        );
    }

    #[test]
    fn test_build_arguments_optional_value_flag_still_renders_bare_for_true() {
        // The other half of an optional value: `true` selects the bare form,
        // and must not be caught by the "takes a value" backstop.
        let schema = schema_with(json!({
            "exec_path": {
                "type": ["string", "boolean"],
                "x-apexe-flag": "--exec-path",
                "x-apexe-value-optional": true,
            },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("exec_path".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["--exec-path"]
        );
    }

    /// A schema shaped like `find`'s: paths before the flags, true options
    /// before the paths, primaries and the expression trailing, and `--`
    /// declared as an accepted separator.
    fn find_schema() -> Value {
        json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "path": {
                    "type": "array",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "expression": { "type": "array", "x-apexe-positional": 1 },
                "f": {
                    "type": "array",
                    "x-apexe-flag": "-f",
                    "x-apexe-flag-position": "before-operands"
                },
                "L": {
                    "type": "boolean",
                    "x-apexe-flag": "-L",
                    "x-apexe-flag-position": "before-operands"
                },
                "name": { "type": "string", "x-apexe-flag": "-name" },
            }
        })
    }

    #[test]
    fn test_build_arguments_emits_end_of_options_before_the_operands() {
        // Regression (#25): every `find` expression primary begins with `-`,
        // so the operand documented as carrying them could not carry any legal
        // value. `--` is where it belongs for this grammar: verified against
        // /usr/bin/find (BSD) and findutils 4.10.0, `find -- . -name '*.txt'`
        // works on both while `find . -- -name '*.txt'` is "unknown primary or
        // operator" / "unknown predicate `--'".
        let schema = find_schema();
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("path".to_string(), json!(["sandbox"]));
        kwargs.insert("expression".to_string(), json!(["-name", "*.txt"]));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["--", "sandbox", "-name", "*.txt"]
        );
    }

    #[test]
    fn test_build_arguments_end_of_options_follows_the_pre_operand_flags() {
        // `-f` supplies the paths itself, and option parsing continues past its
        // value: `find -f dir -name x` is "illegal option -- n", while
        // `find -f dir -- -name x` works. The separator therefore sits after
        // the pre-operand flags even when no operand renders at all.
        let schema = find_schema();
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("f".to_string(), json!(["-weird-dir"]));
        kwargs.insert("name".to_string(), json!("*.txt"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-f", "-weird-dir", "--", "-name", "*.txt"]
        );
    }

    #[test]
    fn test_build_arguments_end_of_options_follows_every_flag_by_default() {
        // For the ordinary `tool [options] operands` grammar the separator goes
        // after the flags instead, because that is where the tool stops reading
        // options.
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "file": { "type": "array", "x-apexe-positional": 0 },
                "i": { "type": "boolean", "x-apexe-flag": "-i" },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!(["-weird-name"]));
        kwargs.insert("i".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["-i", "--", "-weird-name"]
        );
    }

    #[test]
    fn test_build_arguments_emits_end_of_options_when_no_operand_stops_parsing() {
        // `find -f dir -name '*.txt'` is "illegal option -- n": with the paths
        // supplied by `-f` instead of by the operand, nothing stops the option
        // parser before the predicates. No value here begins with `-`, so the
        // dash-leading rule alone would not fire.
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "f": {"type": "array", "x-apexe-flag": "-f", "x-apexe-flag-position": "before-operands"},
                "path": {"type": "array", "x-apexe-positional": 0, "x-apexe-operand-position": "before-flags"},
                "name": {"type": "string", "x-apexe-flag": "-name"},
            }
        });
        let kwargs = json!({"f": ["sandbox"], "name": "*.txt"})
            .as_object()
            .expect("object literal")
            .clone();

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-f", "sandbox", "--", "-name", "*.txt"]);
    }

    #[test]
    fn test_build_arguments_omits_end_of_options_when_an_operand_stops_parsing() {
        // A bare path operand already terminates option parsing, so the
        // separator would be noise.
        let schema = json!({
            "type": "object",
            "x-apexe-end-of-options": true,
            "properties": {
                "path": {"type": "array", "x-apexe-positional": 0, "x-apexe-operand-position": "before-flags"},
                "name": {"type": "string", "x-apexe-flag": "-name"},
            }
        });
        let kwargs = json!({"path": ["sandbox"], "name": "*.txt"})
            .as_object()
            .expect("object literal")
            .clone();

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["sandbox", "-name", "*.txt"]);
    }

    #[test]
    fn test_build_arguments_omits_end_of_options_when_nothing_needs_it() {
        // Emitting `--` unconditionally would be legal but would rewrite every
        // invocation of a marked tool for the sake of the few that need it.
        let schema = find_schema();
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("path".to_string(), json!(["sandbox"]));
        kwargs.insert("name".to_string(), json!("*.txt"));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["sandbox", "-name", "*.txt"]
        );
    }

    #[test]
    fn test_build_arguments_still_rejects_option_like_values_without_the_marker() {
        // The default is unchanged: a tool nobody has checked for `--` support
        // keeps refusing, because guessing where the separator goes would turn
        // a refusal into a failing invocation.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "array",
                    "x-apexe-positional": 0,
                    "x-apexe-operand-position": "before-flags"
                },
                "expression": { "type": "array", "x-apexe-positional": 1 },
            }
        });
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("path".to_string(), json!(["sandbox"]));
        kwargs.insert("expression".to_string(), json!(["-name", "*.txt"]));

        let err = build_arguments(&kwargs, Some(&schema))
            .expect_err("without the marker the guard must still refuse");
        assert_eq!(err.code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_validate_argument_value_names_the_offending_array_element() {
        // Regression (#25b): the check scans every element but reported as
        // though the first one were at fault -- `{"expression": ["(", "-name",
        // "*.txt", ")"]}` was refused with "Parameter 'expression' starts with
        // '-'", and `"("` does not.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert(
            "expression".to_string(),
            json!(["(", "-name", "*.txt", ")"]),
        );

        let err = build_arguments(&kwargs, None).expect_err("the element must be refused");
        assert!(
            err.message.contains("Element 1 of parameter 'expression'"),
            "the message must name the element and its index: {}",
            err.message
        );
        assert!(
            err.message.contains("'-name'"),
            "the message must quote the offending value: {}",
            err.message
        );
    }

    #[test]
    fn test_validate_argument_value_names_a_scalar_without_an_index() {
        // A scalar property has no index to report, and inventing `0` for one
        // would read as an array.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("data".to_string(), json!("--output=/etc/passwd"));

        let err = build_arguments(&kwargs, None).expect_err("the value must be refused");
        assert!(
            err.message.starts_with("Parameter 'data' is"),
            "a scalar names no index: {}",
            err.message
        );
    }

    #[test]
    fn test_build_arguments_short_flag_value_stays_separate() {
        // The mirror image: `-I=PATTERN` would pass a value beginning with `=`,
        // so short options keep the value as its own argv entry.
        let schema = schema_with(json!({
            "I": { "type": "string", "x-apexe-flag": "-I" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("I".to_string(), json!("*.tmp"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-I", "*.tmp"]);
    }

    #[test]
    fn test_build_arguments_multi_character_single_dash_flag() {
        // `find -daystart` is spelled with one dash despite being many
        // characters, so no "one character means one dash" rule recovers it.
        let schema = schema_with(json!({
            "daystart": { "type": "boolean", "x-apexe-flag": "-daystart" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("daystart".to_string(), json!(true));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-daystart"]);
    }

    #[test]
    fn test_build_arguments_positional_is_passed_bare() {
        let schema = schema_with(json!({
            "file": {
                "type": "array",
                "items": { "type": "string" },
                "x-apexe-positional": 0,
            },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!(["/tmp", "/var"]));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["/tmp", "/var"]);
    }

    #[test]
    fn test_build_arguments_positional_order_ignores_key_order() {
        // `cp source target` inverts its meaning if the two are swapped, and a
        // JSON object carries no ordering the renderer can rely on -- serde's
        // preserve_order means the caller's key order would otherwise decide.
        let schema = schema_with(json!({
            "source": { "type": "string", "x-apexe-positional": 0 },
            "target": { "type": "string", "x-apexe-positional": 1 },
        }));

        let mut kwargs = serde_json::Map::new();
        kwargs.insert("target".to_string(), json!("/dst"));
        kwargs.insert("source".to_string(), json!("/src"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["/src", "/dst"]);
    }

    #[test]
    fn test_build_arguments_flags_precede_positionals() {
        let schema = schema_with(json!({
            "l": { "type": "boolean", "x-apexe-flag": "-l" },
            "file": { "type": "string", "x-apexe-positional": 0 },
        }));

        // Deliberately insert the operand first.
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!("/tmp"));
        kwargs.insert("l".to_string(), json!(true));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["-l", "/tmp"]);
    }

    #[test]
    fn test_build_arguments_boolean_positional_is_skipped() {
        // There is no token to emit for a bare operand set to `true`, and no
        // way at all to express `false`.
        let schema = schema_with(json!({
            "file": { "type": "string", "x-apexe-positional": 0 },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("file".to_string(), json!(true));

        assert!(build_arguments(&kwargs, Some(&schema)).unwrap().is_empty());
    }

    #[test]
    fn test_build_arguments_falls_back_without_schema_annotations() {
        // Bindings generated before the annotations existed must keep working
        // exactly as they did, rather than silently rendering nothing.
        let schema = schema_with(json!({ "no_cache": { "type": "boolean" } }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("no_cache".to_string(), json!(true));

        assert_eq!(
            build_arguments(&kwargs, Some(&schema)).unwrap(),
            vec!["--no-cache"]
        );
        assert_eq!(
            build_arguments(&kwargs, None).unwrap(),
            vec!["--no-cache"],
            "no schema at all must behave the same as an unannotated one"
        );
    }

    #[test]
    fn test_build_arguments_long_flag_literal_survives_underscore() {
        // `_` -> `-` is lossy: a long flag that genuinely contains an
        // underscore cannot be recovered from the property key.
        let schema = schema_with(json!({
            "foo_bar": { "type": "string", "x-apexe-flag": "--foo_bar" },
        }));
        let mut kwargs = serde_json::Map::new();
        kwargs.insert("foo_bar".to_string(), json!("v"));

        let args = build_arguments(&kwargs, Some(&schema)).unwrap();
        assert_eq!(args, vec!["--foo_bar", "v"]);
    }

    #[test]
    fn test_validate_no_injection_clean() {
        let result = validate_no_injection("file", "hello world");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_no_injection_semicolon() {
        // Binding-sourced values keep the conservative set: nothing legitimate
        // puts a shell metacharacter in `json_flag` or `command_parts`, so it
        // stays useful as a tamper signal even though no shell would see it.
        let result = validate_no_injection("arg", "a;b");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::GeneralInvalidInput);
    }

    #[test]
    fn test_validate_argument_value_trust_classes_differ() {
        // The same string is fine as a runtime parameter and rejected as a
        // binding value -- that asymmetry is the point.
        let location = ValueLocation {
            param_name: "data",
            index: None,
        };
        assert!(validate_argument_value(location, "a;b|c", false, false).is_ok());
        assert!(validate_no_injection("json_flag", "a;b|c").is_err());
    }

    #[test]
    fn test_is_allowed_env() {
        // Base runtime vars pass; secrets do not.
        assert!(is_allowed_env("PATH"));
        assert!(is_allowed_env("HOME"));
        assert!(is_allowed_env("LC_CTYPE")); // LC_ prefix
        assert!(!is_allowed_env("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_allowed_env("GH_TOKEN"));
        assert!(!is_allowed_env("OPENAI_API_KEY"));
        assert!(!is_allowed_env("CARGO_PKG_NAME"));
    }

    #[tokio::test]
    async fn test_execute_subprocess_kills_the_child_when_the_timeout_elapses() {
        // §9.3 promises the process is "actually terminated rather than left
        // running as an orphan" — the guarantee that makes a timed-out
        // `rm -rf` stop deleting, and the premise `CliModule::execute` reasons
        // about when it decides whether a timeout is safe to retry. It rests
        // entirely on `kill_on_drop(true)`, and flipping that to false broke no
        // test: the timeout error is produced by the outer `tokio::time::
        // timeout` either way, so nothing observed the child again.
        //
        // A surviving orphan is observable through its side effect: the child
        // touches a file after the timeout has already fired.
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("orphan-ran");
        let script = format!("sleep 1; : > {}", marker.display());

        let error = execute_subprocess("sh", &["-c".to_string(), script], None, 150, 4096)
            .await
            .expect_err("the command must time out");
        assert_eq!(error.code, ErrorCode::ModuleTimeout);

        // Past the child's own sleep, so an unkilled one would have finished.
        tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
        assert!(
            !marker.exists(),
            "the timed-out child kept running and completed its side effect"
        );
    }

    #[tokio::test]
    async fn test_execute_subprocess_scrubs_environment() {
        // A wrapped subprocess must NOT inherit apexe's full environment (which
        // may hold secrets). Only allowlisted vars (plus a few sh injects) reach
        // it; PATH must survive so tools can still run.
        let out = execute_subprocess(
            "sh",
            &["-c".to_string(), "env".to_string()],
            None,
            5_000,
            DEFAULT_MAX_OUTPUT_BYTES,
        )
        .await
        .unwrap();

        assert!(
            out.stdout.lines().any(|l| l.starts_with("PATH=")),
            "PATH should pass through: {}",
            out.stdout
        );
        // The test process itself has many vars (cargo sets numerous CARGO_*);
        // a scrubbed child stays tiny, proving it did not inherit them.
        let count = out.stdout.lines().count();
        assert!(
            count <= 20,
            "environment was not scrubbed, child sees {count} vars: {}",
            out.stdout
        );
        assert!(
            !out.stdout.contains("CARGO_"),
            "cargo/apexe env leaked to the wrapped subprocess: {}",
            out.stdout
        );
    }

    #[tokio::test]
    async fn test_execute_subprocess_echo() {
        let result = execute_subprocess(
            "echo",
            &["hello".to_string()],
            None,
            5000,
            DEFAULT_MAX_OUTPUT_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(result.stdout, "hello\n");
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout_truncated);
        assert!(!result.stderr_truncated);
    }

    #[tokio::test]
    async fn test_execute_subprocess_false() {
        let result = execute_subprocess("false", &[], None, 5000, DEFAULT_MAX_OUTPUT_BYTES)
            .await
            .unwrap();
        assert_ne!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_execute_subprocess_nonexistent() {
        let result = execute_subprocess(
            "/nonexistent_binary_that_does_not_exist",
            &[],
            None,
            5000,
            DEFAULT_MAX_OUTPUT_BYTES,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::ModuleExecuteError);
    }

    #[tokio::test]
    async fn test_execute_subprocess_timeout_leaves_retryable_unset() {
        // Retryability depends on the module's `idempotent` annotation,
        // which this layer doesn't have; `CliModule::execute` decides it.
        let result = execute_subprocess("sleep", &["1".to_string()], None, 10, 1024).await;
        let err = result.unwrap_err();
        assert_eq!(err.code, ErrorCode::ModuleTimeout);
        assert_eq!(err.retryable, None);
    }

    #[tokio::test]
    async fn test_execute_subprocess_truncates_large_output() {
        // `seq 1 500` terminates on its own (unlike `yes`, which never stops)
        // and its ~2KB of output comfortably fits in the OS pipe buffer, so
        // the process can finish writing (and exit) even though we stop
        // reading well before EOF -- avoiding the write-blocks-forever
        // deadlock a larger range would hit against the 64-byte cap.
        let result = execute_subprocess(
            "seq",
            &["1".to_string(), "500".to_string()],
            None,
            5000,
            /* max_output_bytes */ 64,
        )
        .await
        .unwrap();
        assert!(result.stdout.len() <= 64);
        assert!(result.stdout_truncated);
    }

    #[tokio::test]
    async fn test_execute_subprocess_kills_hung_process_on_timeout() {
        // `sleep 10` outlives the 20ms timeout; kill_on_drop must actually
        // terminate it (verified indirectly: the call returns promptly
        // instead of blocking for the full 10s).
        let start = std::time::Instant::now();
        let result = execute_subprocess("sleep", &["10".to_string()], None, 20, 1024).await;
        assert!(result.is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "execute_subprocess should return promptly once the timeout elapses"
        );
    }

    #[tokio::test]
    async fn test_read_up_to_does_not_eagerly_allocate_full_cap() {
        // Regression for the CRITICAL finding: read_up_to used to
        // `vec![0u8; max_len]` upfront, so a tiny actual output still cost a
        // full max_len allocation (x2 for stdout+stderr, x concurrency).
        // A capped-but-small actual output must not grow the buffer anywhere
        // near the cap.
        let data = b"tiny output";
        let mut cursor = std::io::Cursor::new(data.to_vec());
        let huge_cap = 64 * 1024 * 1024;
        let result = read_up_to(&mut cursor, huge_cap).await.unwrap();
        assert_eq!(result, data);
        assert!(
            result.capacity() < 1024 * 1024,
            "capacity {} should stay proportional to the {}-byte output, not the {}-byte cap",
            result.capacity(),
            data.len(),
            huge_cap
        );
    }

    #[test]
    fn test_truncate_output_under_limit() {
        let (s, truncated) = truncate_output(b"hello", 100);
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_output_over_limit() {
        let (s, truncated) = truncate_output(b"hello world", 5);
        assert_eq!(s, "hello");
        assert!(truncated);
    }

    #[test]
    fn test_truncate_output_respects_utf8_boundary() {
        // "héllo" - 'é' is 2 bytes (0xC3 0xA9); cutting at byte 2 would split it.
        let bytes = "héllo".as_bytes();
        let (s, truncated) = truncate_output(bytes, 2);
        assert!(truncated);
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(s, "h");
    }
}
