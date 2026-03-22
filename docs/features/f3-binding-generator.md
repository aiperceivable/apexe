# F3: Binding Generator

| Field | Value |
|-------|-------|
| **Feature** | F3 |
| **Priority** | P0 (bridges scan to apcore) |
| **Effort** | Medium (~1,000 LOC) |
| **Dependencies** | F2 |

---

## 1. Overview

The binding generator transforms `ScannedCLITool` data structures into `.binding.yaml` files and a shared `execute_cli()` function that are loadable by apcore's `BindingLoader`. It handles module ID generation, JSON Schema creation from CLI flags/args, and command injection prevention in the executor.

---

## 2. Module: `src/binding/module_id.rs`

### Function: `generate_module_id`

```rust
use regex::Regex;

use crate::errors::ApexeError;

/// Generate a canonical apcore module ID from a CLI command path.
///
/// # Errors
///
/// Returns an error if the generated ID exceeds 128 characters.
pub fn generate_module_id(tool_name: &str, command_path: &[String]) -> Result<String, ApexeError> {
    let prefix = "cli";
    let sanitized_tool = sanitize_segment(tool_name);
    let sanitized_path: Vec<String> = command_path.iter().map(|s| sanitize_segment(s)).collect();

    let mut segments = vec![prefix.to_string(), sanitized_tool];
    segments.extend(sanitized_path);

    let module_id = segments.join(".");

    let re = Regex::new(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$").unwrap();
    if !re.is_match(&module_id) {
        return Err(ApexeError::ParseError(format!(
            "Generated module ID '{module_id}' does not match required pattern"
        )));
    }

    if module_id.len() > 128 {
        return Err(ApexeError::ParseError(format!(
            "Module ID '{module_id}' exceeds 128 characters"
        )));
    }

    Ok(module_id)
}

/// Sanitize a string for use as a module ID segment.
fn sanitize_segment(segment: &str) -> String {
    let mut s = segment.to_lowercase();
    s = s.replace('-', "_");
    s.retain(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');

    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s = format!("x{s}");
    }

    if s.is_empty() {
        "unknown".to_string()
    } else {
        s
    }
}
```

**ID Generation Examples:**

| Tool | Command Path | Module ID |
|------|-------------|-----------|
| `git` | `["commit"]` | `cli.git.commit` |
| `git` | `["remote", "add"]` | `cli.git.remote.add` |
| `docker` | `["container", "ls"]` | `cli.docker.container.ls` |
| `docker` | `[]` (top-level) | `cli.docker` |
| `ffmpeg` | `[]` | `cli.ffmpeg` |
| `kubectl` | `["get", "pods"]` | `cli.kubectl.get.pods` |
| `aws` | `["s3", "cp"]` | `cli.aws.s3.cp` |
| `my-tool` | `["sub-cmd"]` | `cli.my_tool.sub_cmd` |
| `3ds-tool` | `[]` | `cli.x3ds_tool` |

---

## 3. Module: `src/binding/schema_gen.rs`

### Struct: `SchemaGenerator`

```rust
use std::collections::HashMap;

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
```

**Flag-to-Schema Mapping (detailed):**

| ScannedFlag State | JSON Schema Output |
|-------------------|--------------------|
| `long_name="--message", value_type=String, required=true` | `{"message": {"type": "string"}}` + in `required` |
| `long_name="--all", value_type=Boolean` | `{"all": {"type": "boolean", "default": false}}` |
| `long_name="--count", value_type=Integer, default="10"` | `{"count": {"type": "integer", "default": 10}}` |
| `long_name="--format", value_type=Enum, enum_values=["json","text"]` | `{"format": {"type": "string", "enum": ["json","text"]}}` |
| `long_name="--include", value_type=String, repeatable=true` | `{"include": {"type": "array", "items": {"type": "string"}}}` |
| `long_name="--config", value_type=Path` | `{"config": {"type": "string"}}` |

---

## 4. Module: `src/binding/binding_gen.rs`

### Struct: `BindingGenerator`

```rust
use std::collections::HashMap;

use serde_json::{json, Value as JsonValue};

use super::module_id::generate_module_id;
use super::schema_gen::SchemaGenerator;
use crate::models::{ScannedCLITool, ScannedCommand};

/// A single generated binding entry for a CLI command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedBinding {
    pub module_id: String,
    pub description: String,
    pub target: String,
    pub input_schema: JsonValue,
    pub output_schema: JsonValue,
    pub tags: Vec<String>,
    pub version: String,
    pub annotations: HashMap<String, JsonValue>,
    pub metadata: HashMap<String, JsonValue>,
}

/// A complete binding file for one CLI tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedBindingFile {
    pub tool_name: String,
    pub file_name: String,
    pub bindings: Vec<GeneratedBinding>,
}

/// Transforms ScannedCLITool into GeneratedBindingFile.
pub struct BindingGenerator {
    schema_gen: SchemaGenerator,
}

impl BindingGenerator {
    pub fn new() -> Self {
        Self {
            schema_gen: SchemaGenerator,
        }
    }

    /// Generate a complete binding file from a scanned tool.
    pub fn generate(&self, tool: &ScannedCLITool) -> anyhow::Result<GeneratedBindingFile> {
        let mut bindings = Vec::new();
        self.generate_command_bindings(
            &tool.name,
            &tool.subcommands,
            &[],
            &mut bindings,
        )?;

        // Deduplicate module IDs
        deduplicate_ids(&mut bindings);

        Ok(GeneratedBindingFile {
            tool_name: tool.name.clone(),
            file_name: format!("{}.binding.yaml", tool.name),
            bindings,
        })
    }

    /// Recursively generate bindings for commands and subcommands.
    fn generate_command_bindings(
        &self,
        tool_name: &str,
        commands: &[ScannedCommand],
        parent_path: &[String],
        bindings: &mut Vec<GeneratedBinding>,
    ) -> anyhow::Result<()> {
        for command in commands {
            let mut command_path: Vec<String> = parent_path.to_vec();
            command_path.push(command.name.clone());

            let module_id = generate_module_id(tool_name, &command_path)?;
            let input_schema = self.schema_gen.generate_input_schema(command);
            let output_schema = self.schema_gen.generate_output_schema(&command.structured_output);

            let mut metadata = HashMap::new();
            metadata.insert("apexe_binary".to_string(), json!(tool_name));

            let full_cmd: Vec<String> = std::iter::once(tool_name.to_string())
                .chain(command_path.iter().cloned())
                .collect();
            metadata.insert("apexe_command".to_string(), json!(full_cmd));
            metadata.insert("apexe_timeout".to_string(), json!(30));

            if command.structured_output.supported {
                if let Some(ref flag) = command.structured_output.flag {
                    metadata.insert("apexe_json_flag".to_string(), json!(flag));
                }
            }

            let description: String = command.description.chars().take(200).collect();

            let binding = GeneratedBinding {
                module_id,
                description,
                target: "apexe::executor::execute_cli".to_string(),
                input_schema,
                output_schema,
                tags: vec!["cli".to_string(), tool_name.to_string()],
                version: "1.0.0".to_string(),
                annotations: HashMap::new(),
                metadata,
            };

            bindings.push(binding);

            if !command.subcommands.is_empty() {
                self.generate_command_bindings(
                    tool_name,
                    &command.subcommands,
                    &command_path,
                    bindings,
                )?;
            }
        }

        Ok(())
    }
}

/// Deduplicate module IDs by appending _2, _3, etc. for collisions.
fn deduplicate_ids(bindings: &mut [GeneratedBinding]) {
    let mut seen: HashMap<String, usize> = HashMap::new();

    for binding in bindings.iter_mut() {
        let count = seen.entry(binding.module_id.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            binding.module_id = format!("{}_{}", binding.module_id, count);
        }
    }
}
```

---

## 5. Module: `src/binding/writer.rs`

### Struct: `BindingYAMLWriter`

```rust
use std::path::{Path, PathBuf};

use tracing::info;

use super::binding_gen::GeneratedBindingFile;

/// Serializes GeneratedBindingFile to YAML files.
pub struct BindingYAMLWriter;

impl BindingYAMLWriter {
    /// Write binding file to disk.
    ///
    /// Returns the path to the written file.
    pub fn write(&self, binding_file: &GeneratedBindingFile, output_dir: &Path) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(output_dir)?;

        let yaml_data = serde_json::json!({
            "bindings": binding_file.bindings,
        });
        let yaml_str = serde_yaml::to_string(&yaml_data)?;
        let content = format!("# Auto-generated by apexe. Edit to customize.\n{yaml_str}");

        let file_path = output_dir.join(&binding_file.file_name);
        std::fs::write(&file_path, content)?;

        info!(path = %file_path.display(), count = binding_file.bindings.len(), "Wrote binding file");

        Ok(file_path)
    }
}
```

---

## 6. Module: `src/executor.rs`

### Function: `execute_cli`

See tech-design.md Section 7.3.5 for full implementation.

**Critical security properties:**
- `std::process::Command` always used (no shell invocation)
- `validate_no_injection()` rejects `;|&$` and similar metacharacters
- Base command from binding metadata, not user input
- Timeout enforced on every subprocess call

### Function: `validate_no_injection`

```rust
use std::collections::HashSet;

use crate::errors::ApexeError;

/// Characters that MUST NOT appear in command arguments to prevent injection.
const SHELL_INJECTION_CHARS: &[char] = &[';', '|', '&', '$', '`', '\\', '\'', '"', '\n', '\r'];

/// Reject values containing shell metacharacters.
pub fn validate_no_injection(param_name: &str, value: &str) -> Result<(), ApexeError> {
    let injection_set: HashSet<char> = SHELL_INJECTION_CHARS.iter().copied().collect();
    let found: Vec<char> = value.chars().filter(|c| injection_set.contains(c)).collect();
    if !found.is_empty() {
        return Err(ApexeError::CommandInjection {
            param_name: param_name.to_string(),
            chars: found,
        });
    }
    Ok(())
}
```

### Argument Building Logic

```
For each (key, value) in schema-validated inputs:
  If value is null:     skip
  If value is bool:     append "--{key}" only if true
  If value is array:    for each item: append "--{key}" and item.to_string()
  If value is str/int:  append "--{key}" and value.to_string()

Key transformation: key.replace("_", "-") to convert schema names back to CLI flags
  e.g., "no_cache" -> "--no-cache"
```

**Positional argument handling:**
- Positional args are listed in metadata as `_apexe_positional_keys: ["file", "path"]`
- These are appended to the command without `--` prefix
- They must be appended AFTER all flags

---

## 7. Test Scenarios

| Test ID | Scenario | Input | Expected |
|---------|----------|-------|----------|
| F3-T01 | Module ID: simple | `("git", &["commit"])` | `Ok("cli.git.commit")` |
| F3-T02 | Module ID: nested | `("docker", &["container", "ls"])` | `Ok("cli.docker.container.ls")` |
| F3-T03 | Module ID: hyphen | `("my-tool", &["sub-cmd"])` | `Ok("cli.my_tool.sub_cmd")` |
| F3-T04 | Module ID: digit prefix | `("3ds", &[])` | `Ok("cli.x3ds")` |
| F3-T05 | Module ID: too long | 130-char path | `Err(ParseError)` |
| F3-T06 | Module ID: validation | all generated IDs | Match `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$` |
| F3-T07 | Schema: string flag | `--message STRING required` | `{"message": {"type": "string"}}` in required |
| F3-T08 | Schema: boolean flag | `--all` (no value) | `{"all": {"type": "boolean", "default": false}}` |
| F3-T09 | Schema: integer flag | `--count NUM default=10` | `{"count": {"type": "integer", "default": 10}}` |
| F3-T10 | Schema: enum flag | `--format {json,text}` | `{"format": {"type": "string", "enum": ["json","text"]}}` |
| F3-T11 | Schema: repeatable flag | `--include PATTERN repeatable` | `{"include": {"type": "array", "items": {"type": "string"}}}` |
| F3-T12 | Schema: positional required | `<file>` required | In required array |
| F3-T13 | Schema: variadic positional | `<files>...` | `{"files": {"type": "array", "items": {"type": "string"}}}` |
| F3-T14 | Output schema: no JSON | No structured output | stdout + stderr + exit_code only |
| F3-T15 | Output schema: with JSON | Structured output detected | Includes `json_output` property |
| F3-T16 | Binding YAML: valid | Generated git.binding.yaml | Parseable by `serde_yaml::from_str()` |
| F3-T17 | Binding YAML: loadable | Generated binding | `BindingLoader::load_bindings()` succeeds |
| F3-T18 | Binding YAML: registry | Loaded bindings | All modules in Registry |
| F3-T19 | Executor: simple command | `execute_cli("echo", &["echo"], ..)` with message="hi" | stdout contains "hi" |
| F3-T20 | Executor: boolean flag | `{"all": true}` | Command includes `--all` |
| F3-T21 | Executor: boolean false | `{"all": false}` | Command does NOT include `--all` |
| F3-T22 | Executor: injection blocked | `{"message": "hi; rm -rf /"}` | `Err(CommandInjection)` |
| F3-T23 | Executor: pipe blocked | `{"file": "a \| b"}` | `Err(CommandInjection)` |
| F3-T24 | Executor: backtick blocked | `` {"cmd": "`whoami`"} `` | `Err(CommandInjection)` |
| F3-T25 | Executor: no shell | Any invocation | `Command::new()` used, no shell |
| F3-T26 | Executor: timeout | Slow command | Timeout error after timeout seconds |
| F3-T27 | Executor: JSON output | `--format json` flag set | `json_output` field in result |
| F3-T28 | Deduplication | Two commands map to same ID | Second gets `_2` suffix |

### Example Test (rstest)

```rust
use rstest::rstest;

#[rstest]
#[case("git", &["commit"], "cli.git.commit")]
#[case("docker", &["container", "ls"], "cli.docker.container.ls")]
#[case("my-tool", &["sub-cmd"], "cli.my_tool.sub_cmd")]
#[case("3ds", &[], "cli.x3ds")]
fn test_module_id_generation(
    #[case] tool: &str,
    #[case] path: &[&str],
    #[case] expected: &str,
) {
    let path_strings: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    let result = generate_module_id(tool, &path_strings).unwrap();
    assert_eq!(result, expected);
}

#[test]
fn test_injection_blocked() {
    let result = validate_no_injection("msg", "hello; rm -rf /");
    assert!(result.is_err());
    match result.unwrap_err() {
        ApexeError::CommandInjection { param_name, chars } => {
            assert_eq!(param_name, "msg");
            assert!(chars.contains(&';'));
        }
        _ => panic!("Expected CommandInjection error"),
    }
}
```
