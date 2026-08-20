use std::path::{Path, PathBuf};

use apcore::module::ModuleAnnotations;
use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::{annotations_to_dict, ScannedModule};
use serde_json::{Map, Value};

/// Writes Claude Skill files (`SKILL.md`) for scanned modules.
///
/// Each module becomes `.claude/skills/<module_id>/SKILL.md` — directly
/// usable by Claude Code as a skill without any additional conversion step,
/// closing the gap between "apexe scans a CLI tool" and "an agent can use it
/// as a skill".
///
/// The document is rendered here rather than by apcore-toolkit's
/// `ModuleStyle::Skill` formatter because that formatter is schema-generic and
/// cannot see apexe's `x-apexe-*` keywords: it drops `x-apexe-conflicts-with`
/// entirely, collapses the `["string", "boolean"]` type of an
/// `x-apexe-value-optional` flag to `any`, and prints man-page invocations as
/// JSON objects whose only populated field is `title`. The section structure
/// and frontmatter shape are kept identical to the toolkit's so existing
/// consumers see no change beyond the added facts.
pub struct SkillOutput;

impl SkillOutput {
    /// Create a new `SkillOutput`.
    pub fn new() -> Self {
        Self
    }

    /// Write one `SKILL.md` per module under `<output_dir>/.claude/skills/`.
    ///
    /// Returns the paths written.
    // ModuleError is the crate-wide domain error; boxing it would diverge
    // from the rest of the apexe/apcore API surface.
    #[allow(clippy::result_large_err)]
    /// Reserve `dir_name` for `module_id`, refusing a clash between two modules.
    ///
    /// `module_id` is not itself a safe path component (see
    /// [`sanitize_skill_dir_name`]), so distinct ids can sanitize to the same
    /// directory — `a/b` and `a_b` both yield `a_b`. Without this the second
    /// module's SKILL.md would silently overwrite the first's instead of
    /// surfacing the naming clash.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    fn claim_skill_dir<'a>(
        seen_dirs: &mut std::collections::HashMap<String, &'a str>,
        dir_name: &str,
        module_id: &'a str,
    ) -> Result<(), ModuleError> {
        match seen_dirs.get(dir_name) {
            Some(&existing) if existing != module_id => Err(ModuleError::new(
                ErrorCode::GeneralInvalidInput,
                format!(
                    "Skill directory name collision: modules '{existing}' \
                     and '{module_id}' both sanitize to '.claude/skills/{dir_name}'"
                ),
            )),
            Some(_) => Ok(()),
            None => {
                seen_dirs.insert(dir_name.to_string(), module_id);
                Ok(())
            }
        }
    }

    /// Write one module's `SKILL.md`, returning its path.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    fn write_one(
        module: &ScannedModule,
        output_dir: &Path,
        dir_name: &str,
    ) -> Result<PathBuf, ModuleError> {
        let skill_dir = output_dir.join(".claude").join("skills").join(dir_name);
        std::fs::create_dir_all(&skill_dir).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!(
                    "Failed to create skill directory {}: {e}",
                    skill_dir.display()
                ),
            )
        })?;

        let path = skill_dir.join("SKILL.md");
        std::fs::write(&path, render_skill(module)).map_err(|e| {
            ModuleError::new(
                ErrorCode::GeneralInternalError,
                format!("Failed to write {}: {e}", path.display()),
            )
        })?;
        Ok(path)
    }

    /// Write a `SKILL.md` per module under `output_dir/.claude/skills/`.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    pub fn write(
        &self,
        modules: &[ScannedModule],
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>, ModuleError> {
        let mut written = Vec::with_capacity(modules.len());
        let mut seen_dirs: std::collections::HashMap<String, &str> =
            std::collections::HashMap::new();
        for module in modules {
            let dir_name = sanitize_skill_dir_name(&module.module_id);
            Self::claim_skill_dir(&mut seen_dirs, &dir_name, &module.module_id)?;
            written.push(Self::write_one(module, output_dir, &dir_name)?);
        }
        Ok(written)
    }
}

impl Default for SkillOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// Sanitize a `module_id` into a single safe path component.
///
/// `module_id` is normally apcore-Registry-validated (dot-separated, no
/// slashes) by the time it reaches here, but `SkillOutput` is also usable
/// directly as a library against hand-loaded `ScannedModule`s, so this
/// defends against a malformed/adversarial `module_id` escaping
/// `.claude/skills/` via a path separator or a bare `..` component.
fn sanitize_skill_dir_name(module_id: &str) -> String {
    let sanitized: String = module_id
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "_".to_string()
    } else {
        sanitized
    }
}

/// Nesting depth at which a nested object property collapses to a JSON block
/// instead of continuing as an indented bullet list. Matches apcore-toolkit's
/// prose renderer so nested schemas keep the shape consumers already expect.
const MAX_SCHEMA_DEPTH: usize = 3;

/// The sentence that follows the man-page example list.
///
/// Examples reach a module as raw invocation strings (see
/// `adapter::converter`), deliberately un-parsed: `find / \! -name "*.c"` does
/// not survive a round trip through schema fields. Saying so stops an agent
/// reading the list as a set of call templates.
const EXAMPLES_NOTE: &str =
    "These are invocations from the tool's man page, shown as command lines. \
     They are not call templates: translate the flags to the properties above.";

/// Render a module's complete `SKILL.md`: YAML frontmatter plus body.
fn render_skill(module: &ScannedModule) -> String {
    let display = resolve_display(module);
    let frontmatter = format!(
        "---\nname: {}\ndescription: {}\n---\n\n",
        yaml_scalar(flatten_line_breaks(&display.title).trim()),
        yaml_scalar(flatten_line_breaks(&display.description).trim())
    );
    frontmatter + &render_body(module, &display)
}

/// Title, description, guidance and tags after the display overlay is applied.
struct SkillDisplay {
    title: String,
    description: String,
    guidance: Option<String>,
    tags: Vec<String>,
}

/// Apply `module.display` over the module's own title/description/tags.
///
/// Mirrors apcore-toolkit's `display: true` resolution: an overlay field wins
/// only when it is present and non-empty, so a partial overlay leaves the rest
/// of the module untouched.
fn resolve_display(module: &ScannedModule) -> SkillDisplay {
    let overlay: Option<&Map<String, Value>> = module.display.as_ref().and_then(|v| v.as_object());
    let text_field = |key: &str| -> Option<String> {
        overlay
            .and_then(|o| o.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let tags = overlay
        .and_then(|o| o.get("tags"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<String>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| module.tags.clone());
    SkillDisplay {
        title: text_field("alias").unwrap_or_else(|| module.module_id.clone()),
        description: text_field("description").unwrap_or_else(|| module.description.clone()),
        guidance: text_field("guidance"),
        tags,
    }
}

/// Render the Markdown body below the frontmatter.
fn render_body(module: &ScannedModule, display: &SkillDisplay) -> String {
    let mut sections: Vec<String> = vec![format!("# {}", display.title)];
    if !display.description.is_empty() {
        sections.push(display.description.clone());
    }
    if let Some(guidance) = &display.guidance {
        sections.push(format!("_{guidance}_"));
    }

    sections.push("## Parameters".to_string());
    let params = render_schema_prose(&module.input_schema, 0);
    sections.push(if params.is_empty() {
        "_(no parameters)_".to_string()
    } else {
        params
    });

    sections.push("## Returns".to_string());
    let returns = render_schema_prose(&module.output_schema, 0);
    sections.push(if returns.is_empty() {
        "_(no return schema)_".to_string()
    } else {
        returns
    });

    if let Some(table) = render_annotations_table(module.annotations.as_ref()) {
        sections.push("## Behavior".to_string());
        sections.push(table);
    }

    if !module.examples.is_empty() {
        sections.push("## Examples".to_string());
        sections.extend(render_examples(&module.examples));
    }

    if !display.tags.is_empty() {
        sections.push("## Tags".to_string());
        let tags = display.tags.iter().map(|t| format!("`{t}`"));
        sections.push(tags.collect::<Vec<String>>().join(", "));
    }

    let mut body = sections.join("\n\n");
    body.push('\n');
    body
}

/// Render a JSON Schema object as a Markdown bullet list, one line per
/// top-level property.
fn render_schema_prose(schema: &Value, depth: usize) -> String {
    let Some(obj) = schema.as_object() else {
        return String::new();
    };
    let declared_type = obj.get("type").and_then(|v| v.as_str());
    let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) else {
        return match declared_type {
            Some(t) if t != "object" => format!("_schema accepts {t}_"),
            _ => String::new(),
        };
    };
    if declared_type != Some("object") {
        return match declared_type {
            Some(t) => format!("_schema accepts {t}_"),
            None => String::new(),
        };
    }
    render_properties_prose(properties, &required_names(obj), depth)
}

/// Render each property of an object schema as one bullet, recursing into
/// nested objects until [`MAX_SCHEMA_DEPTH`].
fn render_properties_prose(
    properties: &Map<String, Value>,
    required: &[String],
    depth: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (name, prop) in properties.iter() {
        lines.push(render_property_line(
            name,
            prop,
            required.iter().any(|r| r == name),
        ));
        let Some(prop_obj) = prop.as_object() else {
            continue;
        };
        if prop_obj.get("type").and_then(|v| v.as_str()) != Some("object") {
            continue;
        }
        let Some(nested) = prop_obj.get("properties").and_then(|v| v.as_object()) else {
            continue;
        };
        let nested_lines = if depth + 1 >= MAX_SCHEMA_DEPTH {
            render_nested_json_block(prop)
        } else {
            render_properties_prose(nested, &required_names(prop_obj), depth + 1)
        };
        lines.extend(nested_lines.lines().map(|line| format!("  {line}")));
    }
    lines.join("\n")
}

/// Render a property that is nested too deeply for prose as a fenced JSON
/// block, so the information is still present.
fn render_nested_json_block(prop: &Value) -> String {
    let pretty = serde_json::to_string_pretty(prop).unwrap_or_else(|_| "{}".to_string());
    format!("```json\n{pretty}\n```")
}

/// Render one property bullet: name, type, requiredness, description, and the
/// `x-apexe-*` facts a caller cannot infer from the type alone.
fn render_property_line(name: &str, prop: &Value, required: bool) -> String {
    let prop_obj = prop.as_object();
    let requiredness = if required { "required" } else { "optional" };
    let description = prop_obj
        .and_then(|o| o.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let mut line = format!("- `{name}` ({}, {requiredness})", type_label(prop_obj));
    if !description.is_empty() {
        line.push_str(" — ");
        line.push_str(description);
    }

    let notes: Vec<String> = prop_obj
        .map(|o| {
            [value_optional_note(o), conflicts_note(o)]
                .into_iter()
                .flatten()
                .collect()
        })
        .unwrap_or_default();
    if notes.is_empty() {
        return line;
    }
    if !description.is_empty() && !line.ends_with(['.', '!', '?']) {
        line.push('.');
    }
    line.push(' ');
    line.push_str(&notes.join(" "));
    line
}

/// Render a property's `type` keyword for display.
///
/// A JSON Schema `type` may be a list of alternatives; an
/// `x-apexe-value-optional` flag is exactly that (`["string", "boolean"]`).
/// Rendering the union rather than `any` matters because `any` is both less
/// informative and wider than the truth — an object or array is not accepted.
fn type_label(prop: Option<&Map<String, Value>>) -> String {
    match prop.and_then(|o| o.get("type")) {
        Some(Value::String(name)) => name.clone(),
        Some(Value::Array(names)) => {
            let rendered: Vec<&str> = names.iter().filter_map(|v| v.as_str()).collect();
            if rendered.is_empty() {
                "any".to_string()
            } else {
                rendered.join(" | ")
            }
        }
        _ => "any".to_string(),
    }
}

/// Explain the two legal spellings of an `x-apexe-value-optional` flag.
///
/// The union type says a string or a boolean is accepted; it does not say that
/// the boolean selects the bare flag and the string supplies a value, which is
/// the part a caller has to get right.
fn value_optional_note(prop: &Map<String, Value>) -> Option<String> {
    if prop.get("x-apexe-value-optional").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    match prop.get("x-apexe-flag").and_then(Value::as_str) {
        Some(flag) => Some(format!(
            "Send `true` for the bare `{flag}`, or a string for `{flag}=<value>`."
        )),
        None => Some("Send `true` for the bare flag, or a string for `--flag=value`.".to_string()),
    }
}

/// State the properties this one must not be sent with.
///
/// The executor rejects a conflicting pair at call time; without this line the
/// only way to learn the constraint is to violate it, which costs every caller
/// one failed round trip.
fn conflicts_note(prop: &Map<String, Value>) -> Option<String> {
    let names: Vec<String> = prop
        .get("x-apexe-conflicts-with")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str())
        .map(|name| format!("`{name}`"))
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!("**Not with:** {}.", names.join(", ")))
}

/// Collect an object schema's `required` list.
fn required_names(schema: &Map<String, Value>) -> Vec<String> {
    schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Render the `## Examples` body as a list of sections.
///
/// apexe's examples are man-page invocation strings carried in
/// [`apcore::module::ModuleExample::title`] with no inputs or output, so a JSON
/// dump of the struct buries the command line among two always-null fields.
/// Those render as a command-line bullet list; an example that does carry
/// structured inputs or output keeps its JSON block so nothing is lost.
fn render_examples(examples: &[apcore::module::ModuleExample]) -> Vec<String> {
    let mut command_lines: Vec<String> = Vec::new();
    let mut structured: Vec<String> = Vec::new();
    for (index, example) in examples.iter().enumerate() {
        if example.inputs.is_null() && example.output.is_null() && !example.title.is_empty() {
            command_lines.push(format!("- {}", code_span(&example.title)));
        } else {
            structured.push(render_structured_example(index, example));
        }
    }

    let mut sections: Vec<String> = Vec::new();
    if !command_lines.is_empty() {
        sections.push(command_lines.join("\n"));
        sections.push(EXAMPLES_NOTE.to_string());
    }
    sections.extend(structured);
    sections
}

/// Render one example that carries inputs or output as a heading plus a JSON
/// block holding only the fields that are actually populated.
fn render_structured_example(index: usize, example: &apcore::module::ModuleExample) -> String {
    let heading = if example.title.is_empty() {
        format!("### Example {}", index + 1)
    } else {
        format!("### {}", example.title)
    };
    let mut body = Map::new();
    if !example.inputs.is_null() {
        body.insert("inputs".to_string(), example.inputs.clone());
    }
    if !example.output.is_null() {
        body.insert("output".to_string(), example.output.clone());
    }
    let pretty =
        serde_json::to_string_pretty(&Value::Object(body)).unwrap_or_else(|_| "{}".to_string());
    let mut section = heading;
    if let Some(description) = example.description.as_deref().filter(|d| !d.is_empty()) {
        section.push_str("\n\n");
        section.push_str(description);
    }
    section.push_str("\n\n```json\n");
    section.push_str(&pretty);
    section.push_str("\n```");
    section
}

/// Render non-default `ModuleAnnotations` as a Markdown fact table.
///
/// Only fields that differ from the protocol default are emitted, rows are
/// sorted by key, and the free-form `extra` bag is skipped — the same contract
/// apcore-toolkit's renderer follows, so the `## Behavior` section is
/// unchanged. Returns `None` when nothing is non-default, which drops the
/// section entirely.
fn render_annotations_table(annotations: Option<&ModuleAnnotations>) -> Option<String> {
    let value = annotations_to_dict(annotations);
    let defaults = annotations_to_dict(Some(&ModuleAnnotations::default()));
    let (obj, defaults) = (value.as_object()?, defaults.as_object()?);
    let mut entries: Vec<(&String, &Value)> = obj
        .iter()
        .filter(|(key, value)| key.as_str() != "extra" && defaults.get(*key) != Some(*value))
        .collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut rows = vec!["| Flag | Value |".to_string(), "|---|---|".to_string()];
    for (key, value) in entries {
        let rendered = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        rows.push(format!("| `{key}` | {rendered} |"));
    }
    Some(rows.join("\n"))
}

/// Every character YAML treats as a line break.
///
/// `\n` is the obvious one and was the only one handled. The other four end a
/// line just as definitively for a conforming parser, and one of them arriving
/// in a scanned `--help` description is enough to terminate the frontmatter
/// block early and push the remainder into the Markdown body — which is the
/// part an agent reads as instructions. This is the same set
/// [`crate::module::executor`]'s `CONTROL_CHARS` rejects on the way *in*, for
/// the same reason: whoever controls a line boundary controls how a consumer
/// frames the document.
const YAML_LINE_BREAKS: [char; 5] = ['\n', '\r', '\u{0085}', '\u{2028}', '\u{2029}'];

/// Collapse every line break in `text` to a single space.
///
/// Frontmatter values are single-line by construction, so a break in a scanned
/// description is never meaningful content — it is the tool's own `--help`
/// wrapping. Replacing rather than deleting keeps the words either side apart.
fn flatten_line_breaks(text: &str) -> String {
    text.replace(YAML_LINE_BREAKS, " ")
}

/// Whether a plain (unquoted) YAML scalar would parse as something other than
/// this exact string.
///
/// Two families: characters that carry structural meaning anywhere in the
/// scalar, and whitespace a plain scalar cannot represent. `\t` earns its place
/// on the second count — GNU `--help` routinely separates a flag from its
/// description with one, no scanner stage strips it, and YAML forbids a tab
/// from a plain scalar's indentation-sensitive positions.
fn needs_yaml_quoting(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            ':' | '#'
                | '{'
                | '}'
                | '['
                | ']'
                | '\''
                | '"'
                | '&'
                | '*'
                | '!'
                | '|'
                | '>'
                | ','
                | '%'
        ) || c == '\t'
            || YAML_LINE_BREAKS.contains(&c)
            || c.is_control()
    }) || text.starts_with(['-', '?', '%', '@', '`', ' '])
        || text.ends_with(' ')
}

/// Quote a YAML scalar when its content would otherwise change the parse.
///
/// The quoted form is YAML's double-quoted style, which is the only one with
/// escape sequences — a raw `\r` or U+2028 inside double quotes is still a line
/// break to the parser, so quoting alone would not save a description that
/// carries one.
fn yaml_scalar(text: &str) -> String {
    if text.is_empty() {
        return "\"\"".to_string();
    }
    if !needs_yaml_quoting(text) {
        return text.to_string();
    }
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('"');
    for c in text.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{0085}' => escaped.push_str("\\N"),
            '\u{2028}' => escaped.push_str("\\L"),
            '\u{2029}' => escaped.push_str("\\P"),
            // Any remaining C0/C1 control character. `\xHH` covers the whole
            // range a scanned help text can plausibly carry, and a parser that
            // reads the escape gets the byte back unchanged.
            c if c.is_control() => escaped.push_str(&format!("\\x{:02x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

/// Wrap `text` in a Markdown code span that cannot be closed early by its own
/// content.
///
/// Man-page EXAMPLES sections routinely contain backticks — shell command
/// substitution is the common case, and `shar` and `mandoc` both ship lines
/// that use it — and the scanner keeps them verbatim. A single-backtick span
/// around such a line closes at the first inner backtick, so the rendered
/// document shows the agent a *different command* than the man page gives.
/// CommonMark's rule is the fix: a span opened with N backticks ends at the
/// next run of exactly N, so N is one longer than the longest run inside. The
/// space padding is what lets the content start or end with a backtick of its
/// own; CommonMark strips one leading and one trailing space from the span.
fn code_span(text: &str) -> String {
    let longest_run = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    if longest_run == 0 {
        return format!("`{text}`");
    }
    let fence = "`".repeat(longest_run + 1);
    format!("{fence} {text} {fence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use apcore::module::ModuleExample;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_test_module(id: &str) -> ScannedModule {
        ScannedModule::new(
            id.to_string(),
            format!("Test module {id}"),
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            json!({"type": "object"}),
            vec!["cli".to_string(), "test".to_string()],
            format!("exec:///usr/bin/test {id}"),
        )
    }

    #[test]
    fn test_skill_output_writes_file() {
        let dir = TempDir::new().unwrap();
        let modules = vec![make_test_module("cli.git.commit")];

        let paths = SkillOutput::new().write(&modules, dir.path()).unwrap();

        assert_eq!(paths.len(), 1);
        let expected = dir.path().join(".claude/skills/cli.git.commit/SKILL.md");
        assert_eq!(paths[0], expected);
        assert!(expected.exists());
    }

    /// Extract and parse the YAML block between the leading `---` fences.
    ///
    /// Substring assertions over the raw text hold just as well on frontmatter
    /// that no loader can read, which is how the escaping gap survived: a skill
    /// whose frontmatter fails to parse silently does not load, which is the
    /// exact outcome `SkillOutput` exists to produce correctly. Parsing is the
    /// only assertion that distinguishes the two.
    fn parse_frontmatter(content: &str) -> Map<String, Value> {
        let body = content
            .strip_prefix("---\n")
            .expect("a skill document opens with a frontmatter fence");
        let (frontmatter, _rest) = body
            .split_once("\n---\n")
            .expect("the frontmatter fence must close");
        let parsed: Value = serde_yaml::from_str(frontmatter).unwrap_or_else(|e| {
            panic!("frontmatter must be valid YAML ({e}); got:\n{frontmatter}")
        });
        parsed
            .as_object()
            .cloned()
            .unwrap_or_else(|| panic!("frontmatter must be a mapping; got:\n{frontmatter}"))
    }

    #[test]
    fn test_skill_output_content_has_frontmatter_and_description() {
        let dir = TempDir::new().unwrap();
        let modules = vec![make_test_module("cli.git.commit")];

        let paths = SkillOutput::new().write(&modules, dir.path()).unwrap();
        let content = std::fs::read_to_string(&paths[0]).unwrap();

        let frontmatter = parse_frontmatter(&content);
        assert_eq!(frontmatter["name"], "cli.git.commit");
        assert_eq!(frontmatter["description"], "Test module cli.git.commit");
        // Loaders key on this shape; the parse above would accept a reordering.
        assert!(content.starts_with("---\nname: "));
    }

    #[test]
    fn test_skill_frontmatter_survives_help_text_control_characters() {
        // A GNU `--help` line separates a flag from its description with a tab,
        // and no scanner stage strips it. Combined with the other four YAML
        // line breaks and a colon, this is every way a scanned description
        // could previously end the frontmatter block early or fail the parse.
        for hostile in [
            "Print a table\tcolumn-separated",
            "Line one\rline two",
            "Line one\u{0085}line two",
            "Line one\u{2028}line two",
            "Line one\u{2029}line two",
            "Usage: prints a report",
            "Ends the block\r---\nname: injected",
            "Trailing backslash \\",
            "Quoted \"value\" inside",
        ] {
            let mut module = make_test_module("cli.hostile");
            module.description = hostile.to_string();

            let content = render_skill(&module);
            let frontmatter = parse_frontmatter(&content);
            assert_eq!(
                frontmatter.len(),
                2,
                "a description must not add or remove frontmatter keys: {hostile:?}"
            );
            assert!(
                frontmatter["description"].is_string(),
                "description must stay a scalar: {hostile:?}"
            );
        }
    }

    #[test]
    fn test_skill_frontmatter_survives_a_title_needing_quoting() {
        // `alias` comes from a repo-committed overlay rather than the scanned
        // binary, so this is defence in depth rather than a live exposure --
        // but an unquoted colon in it breaks the document just as completely.
        let mut module = make_test_module("cli.plain");
        module.display = Some(json!({"alias": "git: the stupid content tracker"}));

        let frontmatter = parse_frontmatter(&render_skill(&module));
        assert_eq!(frontmatter["name"], "git: the stupid content tracker");
    }

    #[test]
    fn test_yaml_scalar_leaves_a_plain_scalar_unquoted() {
        assert_eq!(
            yaml_scalar("List directory contents"),
            "List directory contents"
        );
        assert_eq!(yaml_scalar("cli.git.commit"), "cli.git.commit");
    }

    #[test]
    fn test_yaml_scalar_quotes_the_empty_string() {
        assert_eq!(yaml_scalar(""), "\"\"");
    }

    #[test]
    fn test_yaml_scalar_escapes_every_yaml_line_break() {
        // The four beyond `\n`. A raw one inside double quotes is still a line
        // break to a conforming parser, so quoting alone is not enough --
        // each has to become an escape sequence.
        for (raw, escape) in [
            ('\n', "\\n"),
            ('\r', "\\r"),
            ('\u{0085}', "\\N"),
            ('\u{2028}', "\\L"),
            ('\u{2029}', "\\P"),
        ] {
            let quoted = yaml_scalar(&format!("before{raw}after"));
            assert_eq!(quoted, format!("\"before{escape}after\""));
            assert!(
                !quoted.contains(raw),
                "the raw break must not survive: {raw:?}"
            );
            let parsed: String = serde_yaml::from_str(&quoted).unwrap();
            assert_eq!(parsed, format!("before{raw}after"));
        }
    }

    #[test]
    fn test_yaml_scalar_escapes_tabs_and_other_control_characters() {
        assert_eq!(yaml_scalar("flag\tdescription"), "\"flag\\tdescription\"");
        assert_eq!(yaml_scalar("bell\u{0007}here"), "\"bell\\x07here\"");
        for text in ["flag\tdescription", "bell\u{0007}here"] {
            let parsed: String = serde_yaml::from_str(&yaml_scalar(text)).unwrap();
            assert_eq!(parsed, text);
        }
    }

    #[test]
    fn test_yaml_scalar_quotes_structural_and_leading_characters() {
        for text in [
            "Usage: thing",
            "#not a comment",
            "- not a list item",
            "? not a key",
            "value, with comma",
            "@reserved",
            "`backtick",
            "trailing space ",
        ] {
            let quoted = yaml_scalar(text);
            assert!(quoted.starts_with('"'), "{text:?} must be quoted");
            let parsed: String = serde_yaml::from_str(&quoted).unwrap();
            assert_eq!(parsed, text);
        }
    }

    #[test]
    fn test_yaml_scalar_round_trips_backslashes_and_quotes() {
        for text in ["path\\to\\thing", "say \"hello\"", "both \\ and \""] {
            let parsed: String = serde_yaml::from_str(&yaml_scalar(text)).unwrap();
            assert_eq!(parsed, text);
        }
    }

    #[test]
    fn test_skill_output_multiple_modules() {
        let dir = TempDir::new().unwrap();
        let modules = vec![
            make_test_module("cli.git.commit"),
            make_test_module("cli.git.push"),
        ];

        let paths = SkillOutput::new().write(&modules, dir.path()).unwrap();

        assert_eq!(paths.len(), 2);
        for path in &paths {
            assert!(path.exists());
        }
    }

    #[test]
    fn test_skill_output_empty_modules() {
        let dir = TempDir::new().unwrap();
        let paths = SkillOutput::new().write(&[], dir.path()).unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn test_skill_output_write_rejects_colliding_module_ids() {
        // Regression for the WARNING finding: "a/b" and "a_b" both sanitize
        // to the directory name "a_b". write() must reject the batch
        // instead of letting the second module's SKILL.md silently
        // overwrite the first's.
        let dir = TempDir::new().unwrap();
        let modules = vec![make_test_module("a/b"), make_test_module("a_b")];

        let err = SkillOutput::new()
            .write(&modules, dir.path())
            .expect_err("colliding module_ids must be rejected");
        assert!(err.message.contains("collision"));
        assert!(err.message.contains("a/b"));
        assert!(err.message.contains("a_b"));
    }

    /// A module whose schema carries the `x-apexe-*` keywords from issue #26:
    /// a mutually exclusive pair, an `x-apexe-value-optional` union, and a
    /// plain property that must be untouched by either addition.
    fn make_annotated_module() -> ScannedModule {
        ScannedModule::new(
            "cli.demo".to_string(),
            "Demo tool".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "l": {
                        "type": "boolean",
                        "description": "Long format",
                        "x-apexe-flag": "-l",
                        "x-apexe-conflicts-with": ["one", "C"],
                    },
                    "decorate": {
                        "type": ["string", "boolean"],
                        "description": "Print out the ref names",
                        "x-apexe-flag": "--decorate",
                        "x-apexe-value-optional": true,
                    },
                    "path": {"type": "string", "description": "Target path"},
                },
            }),
            json!({"type": "object"}),
            vec!["cli".to_string()],
            "exec:///bin/demo".to_string(),
        )
    }

    fn command_line_example(title: &str) -> ModuleExample {
        // ModuleExample is #[non_exhaustive]; build via Default + field set.
        let mut example = ModuleExample::default();
        example.title = title.to_string();
        example
    }

    #[test]
    fn test_render_skill_states_mutual_exclusion_per_property() {
        // Defect 1: x-apexe-conflicts-with never reached the skill, so the
        // only way to learn the constraint was to have a call rejected.
        let content = render_skill(&make_annotated_module());

        assert!(
            content.contains("**Not with:** `one`, `C`."),
            "conflicts must be stated on the property line; got:\n{content}"
        );
        // The unconstrained properties must not gain the note.
        assert_eq!(content.matches("**Not with:**").count(), 1);
    }

    #[test]
    fn test_render_skill_renders_union_type_and_value_optional_spellings() {
        // Defect 2: ["string", "boolean"] rendered as `any`, which is both
        // uninformative and wider than the truth.
        let content = render_skill(&make_annotated_module());

        assert!(
            content.contains("`decorate` (string | boolean, optional)"),
            "union type must render as a union; got:\n{content}"
        );
        assert!(!content.contains("`decorate` (any"));
        assert!(
            content.contains(
                "Send `true` for the bare `--decorate`, or a string for `--decorate=<value>`."
            ),
            "value-optional spellings must be spelled out; got:\n{content}"
        );
    }

    #[test]
    fn test_render_skill_renders_man_page_examples_as_command_lines() {
        // Defect 3: man-page invocations were dumped as JSON objects whose
        // only populated field was `title`, alongside two null fields.
        let mut module = make_annotated_module();
        module.examples = vec![
            command_line_example("ls -l"),
            command_line_example("ls -lt /var"),
        ];

        let content = render_skill(&module);

        assert!(content.contains("## Examples"));
        assert!(
            content.contains("- `ls -l`\n- `ls -lt /var`"),
            "got:\n{content}"
        );
        assert!(!content.contains("\"inputs\": null"));
        assert!(!content.contains("### Example 1"));
        assert!(content.contains("not call templates"));
    }

    #[test]
    fn test_render_skill_does_not_truncate_an_example_containing_backticks() {
        // Regression: the man-page bullet wrapped the raw line in a SINGLE
        // backtick span, so the first inner backtick closed it and the command
        // substitution rendered as prose -- showing the agent a different
        // command than the man page gives. Both lines below ship verbatim in
        // man pages on a stock host (`shar`, `mandoc`).
        let mut module = make_annotated_module();
        module.examples = vec![
            command_line_example("shar `find . -print` | mail -s \"ls source\" rick"),
            command_line_example("mandoc -T lint `find /usr/src -name \\*\\.[1-9]`"),
        ];

        let content = render_skill(&module);

        assert!(
            content.contains("`` shar `find . -print` | mail -s \"ls source\" rick ``"),
            "the span must be longer than the longest inner backtick run; got:\n{content}"
        );
        assert!(
            content.contains("`` mandoc -T lint `find /usr/src -name \\*\\.[1-9]` ``"),
            "got:\n{content}"
        );
    }

    #[test]
    fn test_code_span_outruns_the_longest_inner_backtick_run() {
        // CommonMark closes a span at the next run of exactly the opening
        // length, so the fence has to be one longer than anything inside.
        assert_eq!(code_span("ls -l"), "`ls -l`");
        assert_eq!(code_span("echo `date`"), "`` echo `date` ``");
        assert_eq!(code_span("a ``b`` c"), "``` a ``b`` c ```");
        // Content that starts or ends with a backtick is why the pad exists.
        assert_eq!(code_span("`quoted`"), "`` `quoted` ``");
    }

    #[test]
    fn test_render_skill_keeps_structured_examples_as_json() {
        // An example that genuinely carries inputs is not a command line, so
        // it keeps a JSON block — but without the always-null companion field.
        let mut module = make_annotated_module();
        let mut structured = ModuleExample::default();
        structured.title = "long listing".to_string();
        structured.inputs = json!({"l": true});
        module.examples = vec![structured, command_line_example("ls -l")];

        let content = render_skill(&module);

        assert!(content.contains("### long listing"), "got:\n{content}");
        assert!(content.contains("\"l\": true"));
        assert!(!content.contains("\"output\": null"));
        // The command-line example still renders as a bullet.
        assert!(content.contains("- `ls -l`"));
    }

    #[test]
    fn test_render_skill_without_apexe_annotations_keeps_section_shape() {
        // A module carrying none of the annotations must render exactly the
        // structure existing consumers already parse.
        let content = render_skill(&make_test_module("cli.plain"));

        assert!(content.starts_with("---\nname: cli.plain\ndescription: "));
        assert!(content.contains("\n---\n\n# cli.plain\n"));
        assert!(content.contains("## Parameters"));
        assert!(content.contains("- `path` (string, optional)"));
        assert!(content.contains("## Returns"));
        assert!(content.contains("## Tags"));
        assert!(content.contains("`cli`, `test`"));
        for absent in ["**Not with:**", " | ", "Send `true`", "\"inputs\": null"] {
            assert!(
                !content.contains(absent),
                "unexpected {absent} in:\n{content}"
            );
        }
    }

    #[test]
    fn test_render_skill_omits_behavior_when_annotations_are_default() {
        let mut module = make_test_module("cli.plain");
        assert!(!render_skill(&module).contains("## Behavior"));

        module.annotations = Some(ModuleAnnotations {
            readonly: true,
            ..Default::default()
        });
        let content = render_skill(&module);
        assert!(content.contains("## Behavior"));
        assert!(content.contains("| `readonly` | true |"));
        assert!(!content.contains("`destructive`"));
    }

    #[test]
    fn test_type_label_falls_back_to_any_for_untyped_property() {
        assert_eq!(type_label(None), "any");
        let untyped = json!({"description": "no type"});
        assert_eq!(type_label(untyped.as_object()), "any");
        let empty_union = json!({"type": []});
        assert_eq!(type_label(empty_union.as_object()), "any");
    }

    #[test]
    fn test_render_skill_applies_display_overlay() {
        let mut module = make_test_module("cli.plain");
        module.display = Some(json!({
            "alias": "plain-tool",
            "description": "Overlaid description",
            "guidance": "Prefer the long form.",
            "tags": ["curated"],
        }));

        let content = render_skill(&module);

        assert!(content.contains("name: plain-tool"));
        assert!(content.contains("# plain-tool"));
        assert!(content.contains("Overlaid description"));
        assert!(content.contains("_Prefer the long form._"));
        assert!(content.contains("`curated`"));
    }

    #[test]
    fn test_sanitize_skill_dir_name_blocks_traversal() {
        assert_eq!(
            sanitize_skill_dir_name("../../etc/passwd"),
            ".._.._etc_passwd"
        );
        assert_eq!(sanitize_skill_dir_name(".."), "_");
        assert_eq!(sanitize_skill_dir_name(""), "_");
        assert_eq!(sanitize_skill_dir_name("cli.git.commit"), "cli.git.commit");
    }
}
