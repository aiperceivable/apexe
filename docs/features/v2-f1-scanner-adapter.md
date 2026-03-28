# F1: Scanner Adapter -- ScannedCLITool to ScannedModule Conversion

| Field | Value |
|---|---|
| **Feature ID** | F1 |
| **Tech Design Section** | 5.2.1 |
| **Priority** | P0 (Foundation) |
| **Dependencies** | F6 (Error Migration) |
| **Depended On By** | F2, F3, F4 |
| **New Files** | `src/adapter/mod.rs`, `src/adapter/converter.rs`, `src/adapter/schema.rs`, `src/adapter/annotations.rs` |
| **Deleted Files** | None |
| **Estimated LOC** | ~600 |
| **Estimated Tests** | ~30 |

---

## 1. Purpose

Bridge the gap between apexe's scanner output (`ScannedCLITool`) and the apcore-toolkit's standardized module descriptor (`ScannedModule`). This conversion layer is the linchpin of the v0.2.0 integration: every downstream feature (output, serving, governance) consumes `ScannedModule`.

---

## 2. Input and Output Types

### Input

```rust
// src/models/mod.rs (unchanged)
pub struct ScannedCLITool {
    pub name: String,
    pub binary_path: String,
    pub version: Option<String>,
    pub subcommands: Vec<ScannedCommand>,
    pub global_flags: Vec<ScannedFlag>,
    pub structured_output: StructuredOutputInfo,
    pub scan_tier: u32,
    pub warnings: Vec<String>,
}
```

### Output

```rust
// From apcore-toolkit 0.4
pub struct ScannedModule {
    pub module_id: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub tags: Vec<String>,
    pub target: String,
    pub version: String,
    pub annotations: ModuleAnnotations,
    pub documentation: Option<String>,
    pub examples: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub warnings: Vec<String>,
}
```

---

## 3. Module Structure

### 3.1 `src/adapter/mod.rs`

```rust
pub mod annotations;
pub mod converter;
pub mod schema;

pub use converter::CliToolConverter;
```

### 3.2 `src/adapter/converter.rs` -- CliToolConverter

```rust
use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use crate::models::{ScannedCLITool, ScannedCommand, ScannedFlag};

/// Configuration for ScannedCLITool to ScannedModule conversion.
pub struct CliToolConverter {
    /// Namespace prefix for module IDs (default: "cli").
    namespace: String,
    /// Whether to include raw_help in metadata.
    include_raw_help: bool,
}
```

**Public methods**:

```rust
impl CliToolConverter {
    /// Create a converter with default settings.
    pub fn new() -> Self;

    /// Create a converter with a custom namespace prefix.
    pub fn with_namespace(namespace: &str) -> Self;

    /// Convert a single ScannedCLITool into one or more ScannedModules.
    /// Produces one ScannedModule per leaf command (commands with no subcommands).
    pub fn convert(&self, tool: &ScannedCLITool) -> Vec<ScannedModule>;

    /// Convert multiple tools.
    pub fn convert_all(&self, tools: &[ScannedCLITool]) -> Vec<ScannedModule>;
}
```

**Also implement the `From` trait for ergonomic conversion**:

```rust
impl From<&ScannedCLITool> for Vec<ScannedModule> {
    fn from(tool: &ScannedCLITool) -> Vec<ScannedModule> {
        CliToolConverter::new().convert(tool)
    }
}
```

### 3.3 Conversion Logic (Step by Step)

1. **Flatten subcommand tree**: Recursively walk `ScannedCLITool.subcommands`. For each leaf command (one with no child subcommands), produce a `ScannedModule`. If a tool has no subcommands, produce a single module for the root command.

2. **Generate module_id**: Format as `{namespace}.{tool_name}.{subcommand_path}` where subcommand_path joins nested command names with dots.
   - `git commit` becomes `cli.git.commit`
   - `docker container ls` becomes `cli.docker.container.ls`
   - `ffmpeg` (no subcommands) becomes `cli.ffmpeg`

3. **Build input_schema**: Call `schema::build_input_schema(command, global_flags)`.

4. **Build output_schema**: Call `schema::build_output_schema(command)`.

5. **Generate tags**: `["cli", tool_name, help_format_name]`. Add `"structured-output"` if structured output is supported.

6. **Set target**: `exec://{binary_path} {full_command_path}`.

7. **Set version**: Use `tool.version` or `"unknown"`.

8. **Infer annotations**: Call `annotations::infer(command)`.

9. **Build documentation**: Use `command.raw_help` if non-empty.

10. **Copy examples**: Use `command.examples`.

11. **Build metadata**: Include `scan_tier`, `help_format`, `binary_path`. Optionally include `raw_help`.

12. **Copy warnings**: From both the tool-level and any command-level warnings.

### 3.4 `src/adapter/schema.rs` -- Schema Mapping

This module extracts and adapts schema generation logic from the current `src/binding/schema_gen.rs`.

```rust
/// Build a JSON Schema object from a command's flags and positional args.
pub fn build_input_schema(
    command: &ScannedCommand,
    global_flags: &[ScannedFlag],
) -> serde_json::Value;

/// Build an output schema based on structured output capability.
pub fn build_output_schema(command: &ScannedCommand) -> serde_json::Value;
```

**Input schema logic** (preserved from existing SchemaGenerator):

1. Create a JSON Schema `"object"` with `properties` and `required` arrays.
2. For each flag in `command.flags` + `global_flags`:
   - Property key: `flag.canonical_name()`
   - Type: map `ValueType` to JSON Schema type (`String` -> `"string"`, `Integer` -> `"integer"`, `Boolean` -> `"boolean"`, `Float` -> `"number"`, `Path` -> `"string" + format: "path"`, `Url` -> `"string" + format: "uri"`, `Enum` -> `"string" + enum: [values]`)
   - Description: `flag.description`
   - Default: `flag.default` if present
   - If `flag.required`: add to `required` array
   - If `flag.repeatable`: wrap in `"type": "array", "items": {...}`
3. For each positional arg in `command.positional_args`:
   - Property key: `arg.name` (lowercased, spaces to underscores)
   - Type: same mapping as flags
   - If `arg.variadic`: wrap in array
   - If `arg.required`: add to `required`

**Output schema logic**:

1. If `command.structured_output.supported` and format is `"json"`:
   - Return `{ "type": "object", "properties": { "json_output": { "type": "object" }, "exit_code": { "type": "integer" } } }`
2. Otherwise:
   - Return `{ "type": "object", "properties": { "stdout": { "type": "string" }, "stderr": { "type": "string" }, "exit_code": { "type": "integer" } } }`

### 3.5 `src/adapter/annotations.rs` -- Annotation Inference

Extracts and adapts annotation logic from current `src/governance/annotations.rs`.

```rust
use apcore::ModuleAnnotations;
use crate::models::ScannedCommand;

/// Infer ModuleAnnotations from a command's name and characteristics.
pub fn infer(command: &ScannedCommand) -> ModuleAnnotations;
```

**Inference rules**:

| Command name pattern | Annotation |
|---|---|
| `list`, `ls`, `show`, `get`, `status`, `info`, `version`, `help`, `describe`, `view`, `cat`, `log`, `diff`, `search`, `find`, `check`, `inspect` | `readonly: true` |
| `delete`, `rm`, `remove`, `destroy`, `purge`, `drop`, `kill`, `prune`, `clean`, `reset`, `format`, `wipe` | `destructive: true`, `requires_approval: true` |
| `create`, `add`, `new`, `init`, `clone`, `install`, `build`, `run`, `exec`, `start`, `stop`, `restart`, `push`, `pull`, `commit`, `merge`, `apply`, `update`, `set`, `config`, `edit`, `rename`, `move`, `mv`, `cp`, `copy` | `readonly: false` (write operation, default) |

Additional inference:
- `idempotent: true` for `get`, `list`, `show`, `status`, `info`, `describe`, `version`, `help`, `check`
- `streaming: false` (CLI commands are batch, not streaming)
- `cacheable: true` for readonly + idempotent commands

---

## 4. Test Scenarios

### 4.1 Converter Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_converter_single_command_tool` | Tool with no subcommands (e.g., `ffmpeg`) | Single ScannedModule with module_id `cli.ffmpeg` |
| `test_converter_tool_with_subcommands` | Tool with 3 leaf subcommands | 3 ScannedModules |
| `test_converter_nested_subcommands` | `docker container ls` (2 levels) | module_id `cli.docker.container.ls` |
| `test_converter_deeply_nested` | 3 levels of nesting | Correct dot-separated module_id |
| `test_converter_module_id_format` | Various tool/command names | `cli.{tool}.{path}` format |
| `test_converter_custom_namespace` | Namespace = "myns" | module_id starts with `myns.` |
| `test_converter_description_copied` | Command with description | description field matches |
| `test_converter_version_present` | Tool with version "2.43.0" | version = "2.43.0" |
| `test_converter_version_missing` | Tool with version = None | version = "unknown" |
| `test_converter_tags_include_tool_name` | Tool named "git" | tags contains "git" |
| `test_converter_tags_include_help_format` | Command with HelpFormat::Gnu | tags contains "gnu" |
| `test_converter_tags_structured_output` | Tool with structured output | tags contains "structured-output" |
| `test_converter_target_format` | Tool at /usr/bin/git | target = "exec:///usr/bin/git commit" |
| `test_converter_warnings_propagated` | Tool with warnings | warnings field populated |
| `test_converter_from_trait` | Use From trait directly | Same result as converter.convert() |
| `test_converter_convert_all` | Two tools | Combined ScannedModule list |
| `test_converter_empty_tool` | Tool with no subcommands, no flags | Valid single ScannedModule |

### 4.2 Schema Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_schema_string_flag` | Flag with ValueType::String | `"type": "string"` in schema |
| `test_schema_boolean_flag` | Flag with ValueType::Boolean | `"type": "boolean"` in schema |
| `test_schema_enum_flag` | Flag with enum_values | `"enum": [...]` in schema |
| `test_schema_required_flag` | Flag with required=true | Name in `required` array |
| `test_schema_repeatable_flag` | Flag with repeatable=true | Array wrapper in schema |
| `test_schema_positional_arg` | Positional arg | Included in properties |
| `test_schema_variadic_arg` | Arg with variadic=true | Array wrapper |
| `test_schema_global_flags_included` | Global flags on tool | Merged into command schema |
| `test_schema_output_json` | Structured output supported | json_output in output schema |
| `test_schema_output_raw` | No structured output | stdout/stderr in output schema |

### 4.3 Annotation Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_annotations_list_is_readonly` | Command named "list" | readonly = true |
| `test_annotations_delete_is_destructive` | Command named "delete" | destructive = true, requires_approval = true |
| `test_annotations_create_is_write` | Command named "create" | readonly = false |
| `test_annotations_get_is_idempotent` | Command named "get" | idempotent = true |
| `test_annotations_readonly_is_cacheable` | Readonly + idempotent | cacheable = true |
| `test_annotations_unknown_defaults` | Command named "xyzzy" | readonly = false, destructive = false |

---

## 5. Edge Cases

- **Tool with only a root command** (no subcommands): Produce a single ScannedModule for the root.
- **Duplicate module IDs** after flattening: Use `apcore_toolkit::deduplicate_ids()` to append numeric suffixes.
- **Empty description**: Set description to `"Execute {full_command}"`.
- **Flags with no long or short name**: Use `canonical_name()` which returns `"unknown"` -- deduplicate will disambiguate.
- **Unicode in tool names**: Preserve as-is in module_id (apcore module IDs support UTF-8).

---

## 6. Migration Notes

- Schema generation logic is **extracted** from `src/binding/schema_gen.rs`, not rewritten. The mapping rules are identical.
- Annotation inference logic is **extracted** from `src/governance/annotations.rs`. The patterns are identical but output type changes from `HashMap<String, JsonValue>` to `ModuleAnnotations`.
- The `From<&ScannedCLITool> for Vec<ScannedModule>` trait impl means existing code can convert with `let modules: Vec<ScannedModule> = (&tool).into();`.
