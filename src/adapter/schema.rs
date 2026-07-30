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

/// Option-name words that identify the value as a credential.
///
/// Matched as whole `-`/`_`-separated words rather than as substrings, because
/// every one of them is also a syllable of an unrelated word: `key` is inside
/// `keyword`, `auth` inside `author` (`ls --author`), `pass` inside `bypass`.
const CREDENTIAL_NAME_WORDS: &[&str] = &[
    "auth",
    "cert",
    "certificate",
    "cred",
    "creds",
    "header",
    "identity",
    "jwt",
    "key",
    "keyfile",
    "login",
    "pass",
];

/// Option-name fragments that identify a credential even when run together
/// with other words inside a single token, as `--tlspassword`, `--cookiejar`
/// and `--oauth2-bearer` are.
///
/// Every entry has to stay unambiguous *as a substring* of a flag name; that
/// is exactly why `key`, `auth` and `pass` live in [`CREDENTIAL_NAME_WORDS`]
/// instead of here. `user` is here rather than there because curl spells the
/// TLS-SRP credential `--tlsuser`, and an option name containing those four
/// letters is about a user in every case worth naming.
const CREDENTIAL_NAME_FRAGMENTS: &[&str] = &[
    "apikey",
    "bearer",
    "cookie",
    "credential",
    "netrc",
    "oauth",
    "passphrase",
    "passwd",
    "password",
    "secret",
    "sigv4",
    "token",
    "user",
];

/// Help-text phrases that identify the value as a credential.
///
/// Each is a noun phrase naming the secret itself, never a bare word that
/// could turn up in an unrelated sentence — `bearer token` rather than
/// `token`, which appears in prose like "the expression is composed of
/// tokens".
const CREDENTIAL_HELP_PHRASES: &[&str] = &[
    "access key",
    "access token",
    "api key",
    "api token",
    "auth token",
    "authentication token",
    "bearer token",
    "credential",
    "identity file",
    "netrc",
    "pass phrase",
    "passphrase",
    "passwd",
    "password",
    "private key",
    "secret",
    "session token",
    "ssh key",
    "user name",
    "username",
];

/// Whether an option name matches one of the two name rules.
fn name_identifies_credential(name: &str) -> bool {
    let name = name.trim_start_matches('-').to_lowercase();
    name.split(['-', '_', '.'])
        .any(|word| CREDENTIAL_NAME_WORDS.contains(&word))
        || CREDENTIAL_NAME_FRAGMENTS
            .iter()
            .any(|fragment| name.contains(fragment))
}

/// Whether a help text matches the phrase rule.
fn help_identifies_credential(description: &str) -> bool {
    let description = description.to_lowercase();
    CREDENTIAL_HELP_PHRASES
        .iter()
        .any(|phrase| description.contains(phrase))
}

/// Whether this option's value is a credential that must not reach a log.
///
/// apcore's `redact_sensitive` masks a property only when its schema carries
/// `x-sensitive: true`, and nothing else in the pipeline decides what a secret
/// is — so emitting that keyword is the whole of the protection (issue #30).
/// The judgement has to be made from what a scan actually holds: the option's
/// spelling and its help text. Three rules, in descending order of certainty:
///
/// 1. A `-`/`_`-separated word of the name is in [`CREDENTIAL_NAME_WORDS`] —
///    `--user`, `--proxy-user`, `--key`, `--cert`, `--header`.
/// 2. The name contains a [`CREDENTIAL_NAME_FRAGMENTS`] entry anywhere, which
///    reaches the run-together spellings — `--oauth2-bearer`, `--tlspassword`,
///    `--netrc-file`, `--aws-sigv4`.
/// 3. The help text contains a [`CREDENTIAL_HELP_PHRASES`] entry. This covers
///    the short-only options no name rule can reach: ssh's `-i` has no long
///    form, and is described as the file holding "the identity (private key)".
///
/// The rules are deliberately loose, because the two errors are not
/// symmetric. A masked `--user-agent` costs a log line some detail; an
/// unmasked `--user` writes `admin:hunter2` to the process log at INFO.
/// `--header` is marked for the same reason even though it carries ordinary
/// headers at least as often as `Authorization:` — the value cannot be split,
/// so masking all of it is the safe reading.
///
/// What it deliberately does *not* claim: that an unmarked option is safe.
/// A request body (`--data`) or a URL with the key in its query string still
/// carries secrets no name or help text announces, and neither is reachable
/// from a property-level marker.
fn is_credential_bearing(names: &[&str], description: &str) -> bool {
    names.iter().any(|name| name_identifies_credential(name))
        || help_identifies_credential(description)
}

/// Mark a flag's property sensitive so the logging middleware redacts it.
///
/// Boolean flags are skipped: they carry no value, so the marker could only
/// ever mask the literal `true` while adding noise to the contract for every
/// authentication *selector* a tool offers (curl's `--basic`, `--digest`,
/// `--negotiate`, `--ntlm`, `--netrc` are all bare switches).
fn apply_sensitive_flag(schema: &mut JsonValue, flag: &ScannedFlag) {
    if flag.value_type == ValueType::Boolean {
        return;
    }
    let names: Vec<&str> = [flag.long_name.as_deref(), flag.short_name.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if is_credential_bearing(&names, &flag.description) {
        // On the property itself, not on `items`: apcore checks the property
        // first and replaces the whole array, which is what "mask the whole
        // value" means for a repeatable `--header`.
        schema["x-sensitive"] = json!(true);
    }
}

/// Mark an operand's property sensitive, by the same rules as a flag's.
///
/// Operands are covered too because the credential is not always behind a
/// flag: `htpasswd user password` and `mysql -p` style tools pass one bare.
fn apply_sensitive_arg(schema: &mut JsonValue, arg: &ScannedArg) {
    if is_credential_bearing(&[&arg.name], &arg.description) {
        schema["x-sensitive"] = json!(true);
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

/// The JSON Schema `format` a value type implies, if any.
///
/// Exists so the hint is attached from one place. It used to be an inline
/// `match` on the scalar branch of each builder, which meant the array branches
/// silently dropped it — and arrays are the common case for exactly the types
/// that need it: 36 of the shipped overlays' variadic operands and 7 of their
/// repeatable flags are paths or URLs, so the hint reached almost none of the
/// values it was written for.
fn format_hint(value_type: ValueType) -> Option<&'static str> {
    match value_type {
        // Path and Url both serialize as `string`, so without this an agent
        // cannot tell either from free text.
        ValueType::Path => Some("path"),
        ValueType::Url => Some("uri"),
        _ => None,
    }
}

/// Attach the format hint to an array schema's `items`, where it describes each
/// element rather than the array.
fn apply_items_format(schema: &mut JsonValue, value_type: ValueType) {
    if let Some(format) = format_hint(value_type) {
        schema["items"]["format"] = json!(format);
    }
}

/// Attach the format hint to a scalar schema.
fn apply_format(schema: &mut JsonValue, value_type: ValueType) {
    if let Some(format) = format_hint(value_type) {
        schema["format"] = json!(format);
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
        // On `items`, not on the array: it is each element that is a path.
        apply_items_format(&mut schema, flag.value_type);
        if !flag.description.is_empty() {
            schema["description"] = json!(flag.description);
        }
        apply_long_running(&mut schema, flag);
        apply_flag_literal(&mut schema, flag);
        // Repeatable is the common arity for the credential options that
        // matter most: curl's `--header` and `--cookie` both repeat, so
        // skipping this branch would leave `Authorization: Bearer …` in the
        // log.
        apply_sensitive_flag(&mut schema, flag);
        // Placement is orthogonal to arity — `find -f path` is pre-operand and
        // could legitimately repeat — so it must be emitted on this branch too,
        // not only on the scalar one below.
        apply_flag_placement(&mut schema, flag);
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
    apply_format(&mut schema, flag.value_type);

    if !flag.description.is_empty() {
        schema["description"] = json!(flag.description);
    }

    apply_default(&mut schema, flag);

    if let Some(ref enum_values) = flag.enum_values {
        schema["enum"] = json!(enum_values);
    }

    apply_long_running(&mut schema, flag);
    apply_flag_literal(&mut schema, flag);
    apply_flag_placement(&mut schema, flag);
    apply_sensitive_flag(&mut schema, flag);

    schema
}

/// Record that this flag belongs ahead of the command's operands.
///
/// Emitted only for the minority of flags that need it, and deliberately as a
/// separate keyword from `x-apexe-operand-position` rather than a shared
/// "ordering" field: `find` puts its options before the paths and its primaries
/// after them, so the two sides are set independently and a single knob cannot
/// express the grammar. See [`ScannedFlag::before_operands`].
fn apply_flag_placement(schema: &mut JsonValue, flag: &ScannedFlag) {
    if flag.before_operands {
        schema["x-apexe-flag-position"] = json!("before-operands");
    }
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
        let mut schema = json!({
            "type": "array",
            "items": { "type": base_type },
        });
        // On `items`, not on the array: it is each element that is a path.
        apply_items_format(&mut schema, arg.value_type);
        schema
    } else {
        let mut schema = json!({ "type": base_type });
        apply_format(&mut schema, arg.value_type);
        schema
    };

    if !arg.description.is_empty() {
        schema["description"] = json!(arg.description);
    }
    schema["x-apexe-positional"] = json!(index);
    if arg.before_flags {
        schema["x-apexe-operand-position"] = json!("before-flags");
    }
    apply_sensitive_arg(&mut schema, arg);

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

/// Pick a free property key for a flag displaced by a same-named operand.
///
/// `_option` is the natural suffix but it is not automatically free: git
/// already ships `--strategy` *and* `--strategy-option`, so a tool could hold
/// both `foo` and `foo_option` before any rescue happens. The numbered forms
/// are the fallback; `None` means every candidate was taken, and the caller
/// then keeps the pre-existing behaviour of letting the operand win outright
/// rather than clobbering an unrelated property.
fn free_rescue_key(
    prop_name: &str,
    properties: &serde_json::Map<String, JsonValue>,
) -> Option<String> {
    let preferred = format!("{prop_name}_option");
    if !properties.contains_key(&preferred) {
        return Some(preferred);
    }
    (2..=9)
        .map(|n| format!("{prop_name}_option_{n}"))
        .find(|candidate| !properties.contains_key(candidate))
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
    //
    // The displaced flag is *re-keyed* rather than dropped. "Operand wins" is
    // right for `curl`, where `--url` and `<url>` are the same thing, but the
    // premise fails whenever the two merely share a name: `find -path PATTERN`
    // is a glob predicate and has nothing to do with the `path` operand naming
    // the roots of the walk, and `grep --file=FILE` reads patterns from a file
    // while the `file` operand names the files to search. Overwriting removed
    // those options from the contract entirely — no property rendered `-path`
    // or `-f`, so an agent could not invoke them at all. The executor spells a
    // flag from `x-apexe-flag`, never from the property key, so a suffixed key
    // renders exactly the same command line.
    for (index, arg) in command.positional_args.iter().enumerate() {
        let prop_name = arg.name.to_lowercase().replace('-', "_");
        let mut prop_schema = arg_to_schema(arg, index);

        if let Some(displaced) = properties.get(&prop_name).cloned() {
            if prop_schema.get("description").is_none() {
                if let Some(description) = displaced.get("description") {
                    prop_schema["description"] = description.clone();
                }
            }
            // A same-named flag and operand are usually two spellings of one
            // option (`curl --url` and `<url>`), so a credential judgement made
            // on the flag has to survive the operand taking its key — the
            // operand's own name and (often empty) description may not carry
            // the evidence the flag's help text did.
            if displaced.get("x-sensitive").is_some() {
                prop_schema["x-sensitive"] = json!(true);
            }
            // Only a real flag needs rescuing; a duplicate operand name has
            // nothing to preserve.
            if displaced.get("x-apexe-flag").is_some() {
                if let Some(rescued) = free_rescue_key(&prop_name, &properties) {
                    properties.insert(rescued.clone(), displaced);
                    // `required` named the old key, which now belongs to the
                    // operand; point it at the flag's new home.
                    if let Some(slot) = required.iter_mut().find(|name| **name == prop_name) {
                        slot.clone_from(&rescued);
                    }
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

    // A schema-root marker rather than a per-property one: `--` separates
    // options from operands for the whole invocation, so it says nothing about
    // any individual property. The executor reads it to decide both whether a
    // `-`-leading value may be passed through and where the separator goes.
    if command.end_of_options {
        schema["x-apexe-end-of-options"] = json!(true);
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
            end_of_options: false,
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
    fn test_is_credential_bearing_matches_name_words() {
        // Rule 1: a whole `-`/`_`-separated word of the option name.
        for name in [
            "--key",
            "--key-type",
            "--cert",
            "--header",
            "--proxy-header",
            "--login-path",
            "--client-cert",
            "--identity-file",
            "--pass",
        ] {
            assert!(
                is_credential_bearing(&[name], ""),
                "{name} names a credential"
            );
        }
    }

    #[test]
    fn test_is_credential_bearing_matches_name_fragments() {
        // Rule 2: the run-together spellings a whole-word match cannot reach.
        for name in [
            "--user",
            "--proxy-user",
            "--oauth2-bearer",
            "--tlspassword",
            "--tlsuser",
            "--netrc-file",
            "--aws-sigv4",
            "--cookiejar",
            "--apikey",
            "--service-account-token",
        ] {
            assert!(
                is_credential_bearing(&[name], ""),
                "{name} names a credential"
            );
        }
    }

    #[test]
    fn test_is_credential_bearing_matches_help_phrases() {
        // Rule 3: the only rule that can reach a short-only option. ssh's `-i`
        // has no long form, so its help text is the entire evidence.
        assert!(is_credential_bearing(
            &["-i"],
            "Selects a file from which the identity (private key) for public key authentication is read."
        ));
        assert!(is_credential_bearing(
            &["-p"],
            "The password to use when connecting to the server."
        ));
        assert!(is_credential_bearing(
            &["-t"],
            "Send this bearer token with every request."
        ));
    }

    #[test]
    fn test_is_credential_bearing_rejects_ordinary_options() {
        // The negative half. `--author` and `--keyword` are the reason the name
        // rules split into whole-word and substring lists, and the help texts
        // here are the "a word turned up in an unrelated sentence" case the
        // phrase list is written to avoid.
        let ordinary: &[(&str, &str)] = &[
            ("--max-time", "Maximum time allowed for the transfer."),
            ("--output", "Write to file instead of stdout."),
            ("--silent", "Silent mode."),
            ("--author", "With -l, print the author of each file."),
            ("--keyword", "Search the manual page names for the keyword."),
            ("--bypass", "Skip the intermediate stage."),
            ("--verbose", "Make the operation more talkative."),
            ("--retry", "Retry request if transient problems occur."),
            (
                "-exec",
                "The expression is composed of tokens separated by whitespace.",
            ),
        ];
        for (name, description) in ordinary {
            assert!(
                !is_credential_bearing(&[name], description),
                "{name} is not a credential"
            );
        }
    }

    #[test]
    fn test_schema_credential_flag_is_marked_sensitive() {
        // The reported leak: apcore redacts a property only when it carries
        // `x-sensitive: true`, and no binding had ever emitted one (issue #30).
        let flag = make_flag(
            Some("--user"),
            "<user:password> Server user and password",
            ValueType::String,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["user"]["x-sensitive"], true);
    }

    #[test]
    fn test_schema_repeatable_header_flag_is_marked_sensitive() {
        // `--header` carries ordinary headers as often as `Authorization:`, and
        // the value cannot be split, so the whole of it is masked. It is also
        // repeatable, which returns early from `flag_to_schema` — the branch
        // where the marker is easiest to drop.
        let flag = make_flag(
            Some("--header"),
            "<header/@file> Pass custom header(s) to server",
            ValueType::String,
            false,
            None,
            None,
            true,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);
        let property = &schema["properties"]["header"];

        assert_eq!(property["type"], "array");
        // On the property, not on `items`: apcore checks the property first and
        // masks the array whole.
        assert_eq!(property["x-sensitive"], true);
    }

    #[test]
    fn test_schema_short_only_flag_is_marked_from_its_help_text() {
        // ssh `-i` has no long form; the help text is the only evidence.
        let flag = ScannedFlag {
            short_name: Some("-i".to_string()),
            description: "Selects a file from which the identity (private key) is read."
                .to_string(),
            value_type: ValueType::Path,
            ..Default::default()
        };
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["i"]["x-sensitive"], true);
    }

    #[test]
    fn test_schema_omits_sensitive_for_ordinary_flags() {
        // Absent, not `false` — an ordinary option makes no claim either way,
        // matching how the other `x-` keywords are emitted.
        for (name, description) in [
            ("--max-time", "Maximum time allowed for the transfer."),
            ("--output", "Write to file instead of stdout."),
            ("--silent", "Silent mode."),
        ] {
            let flag = make_flag(
                Some(name),
                description,
                ValueType::String,
                false,
                None,
                None,
                false,
            );
            let key = flag.canonical_name();
            let cmd = make_command(vec![flag], vec![]);
            let schema = build_input_schema(&cmd, &[]);

            assert!(
                schema["properties"][&key]["x-sensitive"].is_null(),
                "{name} must not be marked sensitive"
            );
        }
    }

    #[test]
    fn test_schema_omits_sensitive_for_boolean_auth_selectors() {
        // A bare switch carries no value, so the marker could only mask the
        // literal `true`. curl's `--basic`, `--digest` and `--netrc` are all of
        // this shape, and marking them would be pure contract noise.
        let flag = make_flag(
            Some("--netrc"),
            "Must read .netrc for user name and password",
            ValueType::Boolean,
            false,
            None,
            None,
            false,
        );
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["netrc"]["type"], "boolean");
        assert!(schema["properties"]["netrc"]["x-sensitive"].is_null());
    }

    #[test]
    fn test_schema_credential_operand_is_marked_sensitive() {
        // The credential is not always behind a flag.
        let arg = ScannedArg {
            name: "password".to_string(),
            description: String::new(),
            value_type: ValueType::String,
            required: true,
            variadic: false,
            before_flags: false,
        };
        let cmd = make_command(vec![], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["password"]["x-sensitive"], true);
    }

    #[test]
    fn test_schema_sensitive_survives_an_operand_displacing_the_flag() {
        // The operand takes the flag's key, and its own name carries none of
        // the evidence the flag's help text did.
        let flag = make_flag(
            Some("--secret"),
            "The shared secret to authenticate with.",
            ValueType::String,
            false,
            None,
            None,
            false,
        );
        let arg = ScannedArg {
            name: "secret".to_string(),
            description: String::new(),
            value_type: ValueType::String,
            required: true,
            variadic: false,
            before_flags: false,
        };
        let cmd = make_command(vec![flag], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["secret"]["x-apexe-positional"], 0);
        assert_eq!(schema["properties"]["secret"]["x-sensitive"], true);
        assert_eq!(schema["properties"]["secret_option"]["x-sensitive"], true);
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
    fn test_schema_variadic_operand_carries_the_format_hint_on_items() {
        // Regression: the format hint was applied only on the scalar branch, so
        // the array branches dropped it — and arrays are the common case for
        // exactly the types that need it. 36 of the shipped overlays' variadic
        // operands are paths or URLs, so the hint reached almost none of them
        // and an agent could not tell a path from free text.
        let arg = ScannedArg {
            name: "file".to_string(),
            description: "Files to read.".to_string(),
            value_type: ValueType::Path,
            required: false,
            variadic: true,
            before_flags: false,
        };
        let cmd = make_command(vec![], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);
        let property = &schema["properties"]["file"];

        assert_eq!(property["type"], "array");
        // On `items`, because it is each element that is a path — not the array.
        assert_eq!(property["items"]["format"], "path");
        assert!(
            property["format"].is_null(),
            "the array itself has no format: {property}"
        );
    }

    #[test]
    fn test_schema_repeatable_path_flag_carries_the_format_hint_on_items() {
        let flag = ScannedFlag {
            long_name: Some("--include".to_string()),
            description: "Include a path.".to_string(),
            value_type: ValueType::Path,
            repeatable: true,
            ..Default::default()
        };
        let cmd = make_command(vec![flag], vec![]);
        let schema = build_input_schema(&cmd, &[]);
        let property = &schema["properties"]["include"];

        assert_eq!(property["type"], "array");
        assert_eq!(property["items"]["format"], "path");
    }

    #[test]
    fn test_schema_url_operand_uses_the_uri_format() {
        // The other half of the hint, on both arities.
        let scalar = ScannedArg {
            name: "url".to_string(),
            description: String::new(),
            value_type: ValueType::Url,
            required: true,
            variadic: false,
            before_flags: false,
        };
        let variadic = ScannedArg {
            name: "mirror".to_string(),
            description: String::new(),
            value_type: ValueType::Url,
            required: false,
            variadic: true,
            before_flags: false,
        };
        let cmd = make_command(vec![], vec![scalar, variadic]);
        let schema = build_input_schema(&cmd, &[]);

        assert_eq!(schema["properties"]["url"]["format"], "uri");
        assert_eq!(schema["properties"]["mirror"]["items"]["format"], "uri");
    }

    #[test]
    fn test_schema_non_path_types_carry_no_format_hint() {
        // The hint is only meaningful for the types that serialize as `string`
        // but are not free text; emitting it elsewhere would be noise.
        let arg = ScannedArg {
            name: "pattern".to_string(),
            description: String::new(),
            value_type: ValueType::String,
            required: true,
            variadic: true,
            before_flags: false,
        };
        let cmd = make_command(vec![], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);
        let property = &schema["properties"]["pattern"];

        assert!(property["items"]["format"].is_null(), "{property}");
        assert!(property["format"].is_null(), "{property}");
    }

    #[test]
    fn test_schema_rescues_a_flag_displaced_by_a_same_named_operand() {
        // Regression: "operand wins" is right for `curl`, where `--url` and
        // `<url>` are the same thing, but `find -path PATTERN` is a glob
        // predicate unrelated to the `path` operand, and `grep --file=FILE`
        // reads patterns from a file while the `file` operand names what to
        // search. Overwriting removed those options from the contract entirely.
        let flag = ScannedFlag {
            short_name: Some("-path".to_string()),
            description: "True if the pathname matches pattern.".to_string(),
            value_type: ValueType::String,
            required: true,
            ..Default::default()
        };
        let arg = ScannedArg {
            name: "path".to_string(),
            description: "Roots of the walk.".to_string(),
            value_type: ValueType::Path,
            required: true,
            variadic: true,
            before_flags: true,
        };
        let cmd = make_command(vec![flag], vec![arg]);
        let schema = build_input_schema(&cmd, &[]);
        let properties = &schema["properties"];

        // The operand keeps the plain key...
        assert_eq!(properties["path"]["x-apexe-positional"], 0);
        // ...and the flag survives under a suffixed one, still spelled `-path`.
        assert_eq!(properties["path_option"]["x-apexe-flag"], "-path");
        assert_eq!(
            properties["path_option"]["description"],
            "True if the pathname matches pattern."
        );

        // `required` must follow the flag to its new key, not dangle on the
        // operand's.
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"path_option"), "{required:?}");
        assert_eq!(
            required.iter().filter(|n| **n == "path").count(),
            1,
            "the operand's own requirement is recorded once: {required:?}"
        );
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
