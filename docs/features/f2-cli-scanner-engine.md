# F2: CLI Scanner Engine

| Field | Value |
|-------|-------|
| **Feature** | F2 |
| **Priority** | P0 (core value) |
| **Effort** | Large (~2,500 LOC) |
| **Dependencies** | F1 |

---

## 1. Overview

The scanner engine is the core value-add of apexe. It deterministically parses arbitrary CLI tools' `--help` output, man pages, and shell completion scripts into structured metadata (`ScannedCLITool`). It uses a three-layer priority system: user schema override > plugin parsers > built-in parsers.

---

## 2. Module: `src/models/mod.rs`

All data models for scan results. See tech-design.md Section 7.2.2 for full struct definitions.

### Key Types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Integer,
    Float,
    Boolean,
    Path,
    Enum,
    Url,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelpFormat {
    Gnu,
    Click,
    Argparse,
    Cobra,
    Clap,
    Unknown,
}
```

### Field Mappings: ScannedFlag -> JSON Schema Property

| ScannedFlag Field | JSON Schema Field | Transformation |
|-------------------|-------------------|----------------|
| `long_name` / `short_name` | property key | `canonical_name()`: strip `--`, replace `-` with `_` |
| `value_type` | `type` | `ValueType::String -> "string"`, `Boolean -> "boolean"`, etc. |
| `required` | in `required` array | If true, add to `required` |
| `default` | `default` | String value, type-coerced |
| `enum_values` | `enum` | List of allowed string values |
| `repeatable` | `type: "array"` | Wrap in `{"type": "array", "items": {...}}` |
| `description` | `description` | Direct copy |

---

## 3. Module: `src/scanner/resolver.rs`

### Struct: `ToolResolver`

```rust
use std::process::Command;

use regex::Regex;
use tracing::warn;

use crate::errors::ApexeError;

/// Resolved tool binary information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTool {
    pub name: String,
    pub binary_path: String,
    pub version: Option<String>,
}

/// Resolves CLI tool names to binary paths and version info.
pub struct ToolResolver;

impl ToolResolver {
    /// Resolve a tool name to its binary path and version.
    ///
    /// Returns `Err(ToolNotFound)` if tool is not on PATH.
    pub fn resolve(&self, tool_name: &str) -> Result<ResolvedTool, ApexeError> {
        let binary_path = which::which(tool_name)
            .map_err(|_| ApexeError::ToolNotFound {
                tool_name: tool_name.to_string(),
            })?
            .to_string_lossy()
            .to_string();

        let version = self.get_version(&binary_path, tool_name);

        Ok(ResolvedTool {
            name: tool_name.to_string(),
            binary_path,
            version,
        })
    }

    /// Extract version from --version output.
    ///
    /// Runs the binary with `--version`, parses first line for version pattern.
    fn get_version(&self, binary_path: &str, _tool_name: &str) -> Option<String> {
        let output = Command::new(binary_path)
            .arg("--version")
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_line = stdout.lines().next()?;

        let re = Regex::new(r"(\d+\.\d+[\.\d]*)").ok()?;
        re.captures(first_line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
    }
}
```

**Error handling:**
- `which::which()` fails -> return `Err(ToolNotFound)`
- `--version` times out or fails -> return `version: None`, log warning
- `--version` output has no parseable version -> return `version: None`

---

## 4. Module: `src/scanner/protocol.rs`

### Trait: `CliParser`

```rust
use crate::models::{ScannedArg, ScannedFlag, StructuredOutputInfo};

/// Result of parsing a single help text block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedHelp {
    pub description: String,
    pub flags: Vec<ScannedFlag>,
    pub positional_args: Vec<ScannedArg>,
    pub subcommand_names: Vec<String>,
    pub examples: Vec<String>,
    pub structured_output: StructuredOutputInfo,
}

/// Trait for CLI help format parser plugins.
///
/// Implementations are discovered via shared library loading
/// from `~/.apexe/plugins/` or registered programmatically.
pub trait CliParser: Send + Sync {
    /// Human-readable parser name.
    fn name(&self) -> &str;

    /// Parser priority (lower = higher priority).
    /// Built-in: 100-199. Plugin: 200-299. User override: 0-99.
    fn priority(&self) -> u32;

    /// Return true if this parser can handle the given help text.
    fn can_parse(&self, help_text: &str, tool_name: &str) -> bool;

    /// Parse help text into structured metadata.
    fn parse(&self, help_text: &str, tool_name: &str) -> anyhow::Result<ParsedHelp>;
}
```

**Plugin registration via shared libraries:**

```
~/.apexe/plugins/my_parser.so
  exports: extern "C" fn create_parser() -> Box<dyn CliParser>
```

---

## 5. Module: `src/scanner/pipeline.rs`

### Struct: `ParserPipeline`

```rust
use std::path::Path;

use tracing::{info, warn};

use super::protocol::{CliParser, ParsedHelp};

/// Selects the best parser for a given help text using priority-based routing.
pub struct ParserPipeline {
    parsers: Vec<Box<dyn CliParser>>,
}

impl ParserPipeline {
    /// Initialize with built-in parsers and optional plugins.
    ///
    /// If `plugins` is None, discovers plugins from `~/.apexe/plugins/`.
    pub fn new(plugins: Option<Vec<Box<dyn CliParser>>>) -> Self {
        let mut parsers: Vec<Box<dyn CliParser>> = Vec::new();

        // Add built-in parsers
        parsers.push(Box::new(super::parsers::gnu::GnuHelpParser));
        parsers.push(Box::new(super::parsers::click_parser::ClickHelpParser));
        parsers.push(Box::new(super::parsers::cobra::CobraHelpParser));
        parsers.push(Box::new(super::parsers::clap_parser::ClapHelpParser));

        // Add plugin parsers
        if let Some(ext_parsers) = plugins {
            parsers.extend(ext_parsers);
        }

        parsers.sort_by_key(|p| p.priority());

        Self { parsers }
    }

    /// Parse help text using the highest-priority matching parser.
    ///
    /// Priority resolution:
    /// 1. If `user_override` path exists, load YAML and convert to ParsedHelp.
    /// 2. Try each parser in priority order (ascending).
    /// 3. Fallback: return ParsedHelp with raw text as description.
    pub fn parse(
        &self,
        help_text: &str,
        tool_name: &str,
        user_override: Option<&Path>,
    ) -> ParsedHelp {
        // Check for user override
        if let Some(path) = user_override {
            if path.exists() {
                if let Ok(contents) = std::fs::read_to_string(path) {
                    if let Ok(parsed) = serde_yaml::from_str::<ParsedHelp>(&contents) {
                        info!(path = %path.display(), "Using user override");
                        return parsed;
                    }
                }
            }
        }

        // Try parsers in priority order
        for parser in &self.parsers {
            if parser.can_parse(help_text, tool_name) {
                match parser.parse(help_text, tool_name) {
                    Ok(result) => {
                        info!(parser = parser.name(), "Parsed help text");
                        return result;
                    }
                    Err(e) => {
                        warn!(
                            parser = parser.name(),
                            "Parser failed, trying next: {e}"
                        );
                    }
                }
            }
        }

        // Fallback
        warn!(tool = tool_name, "No parser matched, using raw help text");
        ParsedHelp {
            description: help_text.chars().take(500).collect(),
            ..Default::default()
        }
    }
}
```

**Error handling:**
- Plugin instantiation fails: log warning, skip plugin, continue
- `parse()` returns `Err`: log error, try next parser
- All parsers fail: return fallback ParsedHelp with raw help text

---

## 6. Module: `src/scanner/parsers/gnu.rs`

### Struct: `GnuHelpParser`

```rust
use nom::{
    bytes::complete::{tag, take_while1},
    character::complete::{char, space1},
    combinator::opt,
    sequence::preceded,
    IResult,
};
use regex::Regex;

use crate::models::{ScannedArg, ScannedFlag, StructuredOutputInfo, ValueType};
use crate::scanner::protocol::{CliParser, ParsedHelp};

/// Parser for GNU-style --help output.
///
/// Handles tools like git, grep, curl, wget that follow GNU conventions:
/// - 'Usage: tool [OPTION]...' header
/// - Options formatted as '  -f, --flag=VALUE  Description'
/// - Sections separated by blank lines
pub struct GnuHelpParser;

impl CliParser for GnuHelpParser {
    fn name(&self) -> &str { "gnu" }
    fn priority(&self) -> u32 { 100 }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        let has_usage = help_text.contains("Usage:");
        let has_gnu_opts = Regex::new(r"(?m)^\s+-\w,\s+--\w")
            .map(|re| re.is_match(help_text))
            .unwrap_or(false);
        let not_cobra = !help_text.contains("Available Commands:");
        let not_clap = !help_text.contains("SUBCOMMANDS:");
        has_usage && (has_gnu_opts || !help_text.contains("Commands:")) && not_cobra && not_clap
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        let description = extract_description(help_text);
        let flags = extract_flags(help_text);
        let positional_args = extract_positional_args(help_text);
        let subcommand_names = extract_subcommands(help_text);
        let examples = extract_examples(help_text);
        let structured_output = detect_structured_output(&flags, help_text);

        Ok(ParsedHelp {
            description,
            flags,
            positional_args,
            subcommand_names,
            examples,
            structured_output,
        })
    }
}

/// Extract description from first paragraph before Usage/Options.
fn extract_description(help_text: &str) -> String {
    let mut desc_lines = Vec::new();
    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") || trimmed.starts_with("Options:") {
            break;
        }
        if !trimmed.is_empty() {
            desc_lines.push(trimmed);
        }
    }
    let desc = desc_lines.join(" ");
    desc.chars().take(200).collect()
}

/// Extract flags from OPTIONS section using regex patterns.
fn extract_flags(help_text: &str) -> Vec<ScannedFlag> {
    let flag_re = Regex::new(
        r"(?m)^\s{2,}(-([a-zA-Z0-9]),?\s+)?(--([a-z][\w-]*))((?:=|\s)([A-Z_]+|<[^>]+>))?\s{2,}(.+)"
    ).unwrap();

    let default_re = Regex::new(r"\[default:\s*([^\]]+)\]").unwrap();
    let enum_re = Regex::new(r"\{([^}]+)\}").unwrap();

    let mut flags = Vec::new();

    for cap in flag_re.captures_iter(help_text) {
        let short_name = cap.get(2).map(|m| format!("-{}", m.as_str()));
        let long_name = cap.get(4).map(|m| format!("--{}", m.as_str()));
        let value_name = cap.get(6).map(|m| m.as_str().to_string());
        let description = cap.get(7).map(|m| m.as_str().trim().to_string()).unwrap_or_default();

        let value_type = match value_name.as_deref() {
            None => ValueType::Boolean,
            Some(v) if matches!(v, "FILE" | "PATH" | "DIR" | "DIRECTORY") => ValueType::Path,
            Some(v) if matches!(v, "NUM" | "NUMBER" | "COUNT" | "N" | "PORT") => ValueType::Integer,
            Some(v) if matches!(v, "URL" | "URI") => ValueType::Url,
            _ => ValueType::String,
        };

        let default = default_re
            .captures(&description)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string());

        let enum_values = enum_re
            .captures(&description)
            .and_then(|c| c.get(1))
            .map(|m| {
                m.as_str()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            });

        let required = description.to_lowercase().contains("required");
        let repeatable = description.contains("can be repeated") || description.contains("...");

        let actual_type = if enum_values.is_some() { ValueType::Enum } else { value_type };

        flags.push(ScannedFlag {
            long_name,
            short_name,
            description,
            value_type: actual_type,
            required,
            default,
            enum_values,
            repeatable,
            value_name,
        });
    }

    flags
}

/// Extract positional arguments from Usage line.
fn extract_positional_args(help_text: &str) -> Vec<ScannedArg> {
    let arg_re = Regex::new(r"<([a-zA-Z_][\w-]*)>(\.\.\.)?").unwrap();
    let mut args = Vec::new();

    for line in help_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Usage:") || trimmed.starts_with("usage:") {
            for cap in arg_re.captures_iter(trimmed) {
                let name = cap[1].to_string();
                let variadic = cap.get(2).is_some();
                args.push(ScannedArg {
                    name,
                    description: String::new(),
                    value_type: ValueType::String,
                    required: true,
                    variadic,
                });
            }
        }
    }

    args
}

/// Extract subcommand names from commands section.
fn extract_subcommands(help_text: &str) -> Vec<String> {
    let section_re = Regex::new(r"(?mi)^(commands|subcommands|available commands):").unwrap();
    let cmd_re = Regex::new(r"(?m)^\s{2,}([a-z][\w-]*)\s+\S").unwrap();

    let mut names = Vec::new();

    if let Some(section_match) = section_re.find(help_text) {
        let after_section = &help_text[section_match.end()..];
        for line in after_section.lines() {
            if line.trim().is_empty() || (!line.starts_with(' ') && !line.is_empty()) {
                // End of section: blank line or non-indented line
                if !names.is_empty() {
                    break;
                }
                continue;
            }
            if let Some(cap) = cmd_re.captures(line) {
                names.push(cap[1].to_string());
            }
        }
    }

    names
}

/// Extract example invocations from help text.
fn extract_examples(help_text: &str) -> Vec<String> {
    let example_re = Regex::new(r"(?mi)^(examples?|usage examples?):").unwrap();
    let mut examples = Vec::new();

    if let Some(m) = example_re.find(help_text) {
        for line in help_text[m.end()..].lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if !examples.is_empty() {
                    break;
                }
                continue;
            }
            if trimmed.starts_with('$') || trimmed.starts_with('#') {
                examples.push(trimmed.to_string());
            }
        }
    }

    examples
}

/// Detect structured output flags from parsed flags and help text.
fn detect_structured_output(flags: &[ScannedFlag], _help_text: &str) -> StructuredOutputInfo {
    for flag in flags {
        let long = flag.long_name.as_deref().unwrap_or("");
        if matches!(long, "--format" | "--output-format" | "--output") {
            if let Some(ref enums) = flag.enum_values {
                if enums.iter().any(|v| v == "json") {
                    return StructuredOutputInfo {
                        supported: true,
                        flag: Some(format!("{long} json")),
                        format: Some("json".to_string()),
                    };
                }
            }
        }
        if long == "--json" {
            return StructuredOutputInfo {
                supported: true,
                flag: Some("--json".to_string()),
                format: Some("json".to_string()),
            };
        }
    }

    StructuredOutputInfo::default()
}
```

---

## 7. Module: `src/scanner/parsers/click_parser.rs`

### Struct: `ClickHelpParser`

```rust
/// Parser for Click/argparse-style help output.
///
/// Handles tools using Python Click or argparse:
/// - 'Usage: tool [OPTIONS] COMMAND [ARGS]...' header
/// - Options section with '  --flag TEXT  Description'
/// - Commands section with '  command  Description'
pub struct ClickHelpParser;

impl CliParser for ClickHelpParser {
    fn name(&self) -> &str { "click" }
    fn priority(&self) -> u32 { 110 }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        help_text.contains("[OPTIONS]")
            && help_text.contains("Options:")
            && !help_text.contains("Available Commands:")
            && !help_text.contains("SUBCOMMANDS:")
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        // Same structure as GnuHelpParser but with Click-specific patterns:
        // - Value types: TEXT, INTEGER, FLOAT, PATH, FILENAME
        // - Boolean flags: --flag / --no-flag
        // - Required: [required]
        // - Default: [default: X]
        // - Enum: [opt1|opt2|opt3]
        todo!("Click parser implementation")
    }
}
```

---

## 8. Module: `src/scanner/parsers/cobra.rs`

### Struct: `CobraHelpParser`

```rust
/// Parser for Go Cobra-style help output.
///
/// Handles Go tools like kubectl, docker, gh:
/// - Description paragraph first
/// - 'Usage:\n  tool [command]' format
/// - 'Available Commands:' section
/// - 'Flags:' section with '  -f, --flag type   Description'
pub struct CobraHelpParser;

impl CliParser for CobraHelpParser {
    fn name(&self) -> &str { "cobra" }
    fn priority(&self) -> u32 { 120 }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        help_text.contains("Available Commands:")
            || (help_text.contains("Flags:") && !help_text.contains("Options:"))
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        // Key differences from GNU:
        // - Subcommands in 'Available Commands:' section
        // - Flags in 'Flags:' section (not 'Options:')
        // - Global flags in 'Global Flags:' section
        // - Type shown after flag name: '--flag string  Description'
        todo!("Cobra parser implementation")
    }
}
```

---

## 9. Module: `src/scanner/parsers/clap_parser.rs`

### Struct: `ClapHelpParser`

```rust
/// Parser for Rust Clap-style help output.
///
/// Handles Rust tools like ripgrep, fd, bat:
/// - 'Usage: tool [OPTIONS] [ARGS]' header
/// - 'Options:' section with '  -f, --flag <VALUE>  Description'
/// - 'SUBCOMMANDS:' section (uppercase)
pub struct ClapHelpParser;

impl CliParser for ClapHelpParser {
    fn name(&self) -> &str { "clap" }
    fn priority(&self) -> u32 { 130 }

    fn can_parse(&self, help_text: &str, _tool_name: &str) -> bool {
        help_text.contains("SUBCOMMANDS:")
            || (help_text.contains("<") && help_text.contains(">") && help_text.contains("Options:"))
    }

    fn parse(&self, help_text: &str, _tool_name: &str) -> anyhow::Result<ParsedHelp> {
        todo!("Clap parser implementation")
    }
}
```

---

## 10. Module: `src/scanner/parsers/man.rs`

### Struct: `ManPageParser`

```rust
use std::process::Command;

/// Tier 2 parser: extracts metadata from man pages.
///
/// Used to enrich Tier 1 results with additional descriptions and options.
pub struct ManPageParser;

impl ManPageParser {
    /// Parse man page for additional metadata.
    ///
    /// Runs `man -P cat <tool>`, extracts SYNOPSIS, DESCRIPTION, OPTIONS sections.
    /// Returns None if man page is not available.
    pub fn parse_man_page(&self, tool_name: &str) -> Option<ParsedHelp> {
        let output = Command::new("man")
            .args(["-P", "cat", tool_name])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let text = String::from_utf8_lossy(&output.stdout);
        // Extract SYNOPSIS, DESCRIPTION, OPTIONS sections
        // Return ParsedHelp or None
        Some(ParsedHelp {
            description: extract_man_description(&text),
            ..Default::default()
        })
    }
}

fn extract_man_description(text: &str) -> String {
    // Find DESCRIPTION section and extract first paragraph
    let mut in_desc = false;
    let mut lines = Vec::new();

    for line in text.lines() {
        if line.trim() == "DESCRIPTION" {
            in_desc = true;
            continue;
        }
        if in_desc {
            if line.trim().is_empty() && !lines.is_empty() {
                break;
            }
            if !line.trim().is_empty() {
                lines.push(line.trim());
            }
        }
    }

    lines.join(" ").chars().take(200).collect()
}
```

---

## 11. Module: `src/scanner/parsers/completion.rs`

### Struct: `CompletionParser`

```rust
use std::path::PathBuf;

/// Tier 3 parser: extracts metadata from shell completion scripts.
///
/// Handles zsh and bash completion files.
pub struct CompletionParser;

impl CompletionParser {
    /// Parse shell completion scripts for subcommand/flag discovery.
    ///
    /// Checks:
    /// 1. `/usr/share/zsh/functions/Completion/_<tool>`
    /// 2. `/etc/bash_completion.d/<tool>`
    /// Returns ParsedHelp or None.
    pub fn parse_completions(&self, tool_name: &str) -> Option<ParsedHelp> {
        let zsh_path = PathBuf::from(format!(
            "/usr/share/zsh/functions/Completion/_{tool_name}"
        ));
        let bash_path = PathBuf::from(format!(
            "/etc/bash_completion.d/{tool_name}"
        ));

        let content = if zsh_path.exists() {
            std::fs::read_to_string(&zsh_path).ok()?
        } else if bash_path.exists() {
            std::fs::read_to_string(&bash_path).ok()?
        } else {
            return None;
        };

        // Parse completion functions to extract subcommand and flag names
        let subcommands = extract_completion_subcommands(&content);
        let flags = extract_completion_flags(&content);

        Some(ParsedHelp {
            subcommand_names: subcommands,
            // Flags from completions are less reliable, stored separately
            ..Default::default()
        })
    }
}

fn extract_completion_subcommands(content: &str) -> Vec<String> {
    // Parse case statements and subcommand arrays from completion scripts
    Vec::new() // placeholder
}

fn extract_completion_flags(content: &str) -> Vec<String> {
    // Parse --flag patterns from completion scripts
    Vec::new() // placeholder
}
```

---

## 12. Module: `src/scanner/discovery.rs`

### Struct: `SubcommandDiscovery`

```rust
use std::process::Command;

use tracing::warn;

use super::pipeline::ParserPipeline;
use super::protocol::ParsedHelp;
use crate::models::{HelpFormat, ScannedCommand, StructuredOutputInfo};

/// Recursively discovers and scans subcommands.
pub struct SubcommandDiscovery<'a> {
    pipeline: &'a ParserPipeline,
    max_depth: u32,
}

impl<'a> SubcommandDiscovery<'a> {
    pub fn new(pipeline: &'a ParserPipeline, max_depth: u32) -> Self {
        Self { pipeline, max_depth }
    }

    /// Recursively discover subcommands.
    ///
    /// Returns a list of ScannedCommand with nested subcommands.
    pub fn discover(
        &self,
        tool_name: &str,
        parent_command: &[String],
        subcommand_names: &[String],
        depth: u32,
    ) -> Vec<ScannedCommand> {
        if depth >= self.max_depth {
            warn!(
                tool = tool_name,
                depth = depth,
                "Max subcommand depth reached"
            );
            return Vec::new();
        }

        let mut commands = Vec::new();

        for sub_name in subcommand_names {
            let mut full_cmd: Vec<String> = parent_command.to_vec();
            full_cmd.push(sub_name.clone());

            // Run --help for this subcommand
            let help_text = match self.run_help(tool_name, &full_cmd) {
                Some(text) => text,
                None => continue,
            };

            // Parse help text
            let parsed = self.pipeline.parse(&help_text, tool_name, None);

            // Recursively discover nested subcommands
            let nested = if !parsed.subcommand_names.is_empty() {
                self.discover(tool_name, &full_cmd, &parsed.subcommand_names, depth + 1)
            } else {
                Vec::new()
            };

            commands.push(ScannedCommand {
                name: sub_name.clone(),
                full_command: full_cmd.join(" "),
                description: parsed.description,
                flags: parsed.flags,
                positional_args: parsed.positional_args,
                subcommands: nested,
                examples: parsed.examples,
                help_format: HelpFormat::Unknown,
                structured_output: parsed.structured_output,
                raw_help: help_text,
            });
        }

        commands
    }

    /// Run `<tool> <subcommand...> --help` and capture output.
    fn run_help(&self, tool_name: &str, full_cmd: &[String]) -> Option<String> {
        let mut args: Vec<&str> = full_cmd[1..].iter().map(|s| s.as_str()).collect();
        args.push("--help");

        let output = Command::new(tool_name)
            .args(&args)
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Some tools output help to stderr
        if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            Some(stderr)
        } else if !stdout.trim().is_empty() {
            Some(stdout)
        } else {
            warn!(command = %full_cmd.join(" "), "Empty help output");
            None
        }
    }
}
```

---

## 13. Module: `src/scanner/output_detect.rs`

### Struct: `StructuredOutputDetector`

```rust
use regex::Regex;

use crate::models::{ScannedFlag, StructuredOutputInfo};

/// Known patterns for JSON output flags.
const JSON_PATTERNS: &[(&str, &str)] = &[
    (r"--format\b", "--format json"),
    (r"--output-format\b", "--output-format json"),
    (r"-o\s+json\b|--output\s+json\b", "-o json"),
    (r"--json\b", "--json"),
    (r"-j\b", "-j"),
];

/// Detects if a CLI tool supports structured (JSON) output.
pub struct StructuredOutputDetector;

impl StructuredOutputDetector {
    /// Detect structured output support from flags and help text.
    pub fn detect(&self, flags: &[ScannedFlag], help_text: &str) -> StructuredOutputInfo {
        // Check parsed flags first
        for flag in flags {
            let long = flag.long_name.as_deref().unwrap_or("");
            if matches!(long, "--format" | "--output-format" | "--output") {
                if let Some(ref enums) = flag.enum_values {
                    if enums.iter().any(|v| v == "json") {
                        return StructuredOutputInfo {
                            supported: true,
                            flag: Some(format!("{long} json")),
                            format: Some("json".to_string()),
                        };
                    }
                }
            }
            if long == "--json" {
                return StructuredOutputInfo {
                    supported: true,
                    flag: Some("--json".to_string()),
                    format: Some("json".to_string()),
                };
            }
        }

        // Fall back to regex patterns on raw help text
        for &(pattern, flag_str) in JSON_PATTERNS {
            if let Ok(re) = Regex::new(pattern) {
                if re.is_match(help_text) {
                    return StructuredOutputInfo {
                        supported: true,
                        flag: Some(flag_str.to_string()),
                        format: Some("json".to_string()),
                    };
                }
            }
        }

        StructuredOutputInfo::default()
    }
}
```

---

## 14. Module: `src/scanner/cache.rs`

### Struct: `ScanCache`

```rust
use std::path::{Path, PathBuf};

use crate::models::ScannedCLITool;

/// Filesystem cache for scan results.
pub struct ScanCache {
    cache_dir: PathBuf,
}

impl ScanCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Retrieve cached scan result. Returns None on cache miss or corruption.
    pub fn get(&self, tool_name: &str, tool_version: Option<&str>) -> Option<ScannedCLITool> {
        let key = format!("{}_{}.scan.json", tool_name, tool_version.unwrap_or("unknown"));
        let path = self.cache_dir.join(&key);

        let contents = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Store scan result in cache.
    pub fn put(&self, tool: &ScannedCLITool) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let key = format!(
            "{}_{}.scan.json",
            tool.name,
            tool.version.as_deref().unwrap_or("unknown")
        );
        let path = self.cache_dir.join(&key);
        let json = serde_json::to_string_pretty(tool)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Remove cached result for a tool.
    pub fn invalidate(&self, tool_name: &str) {
        // Remove all cache files matching tool_name_*.scan.json
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&format!("{tool_name}_")) && name_str.ends_with(".scan.json") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}
```

---

## 15. Module: `src/scanner/orchestrator.rs`

### Struct: `ScanOrchestrator`

```rust
use std::process::Command;

use tracing::info;

use super::cache::ScanCache;
use super::discovery::SubcommandDiscovery;
use super::parsers::man::ManPageParser;
use super::parsers::completion::CompletionParser;
use super::pipeline::ParserPipeline;
use super::resolver::ToolResolver;
use crate::config::ApexeConfig;
use crate::models::{HelpFormat, ScannedCLITool, StructuredOutputInfo};

/// Top-level coordinator for the scanning process.
pub struct ScanOrchestrator {
    config: ApexeConfig,
    resolver: ToolResolver,
    pipeline: ParserPipeline,
    cache: ScanCache,
    man_parser: ManPageParser,
    completion_parser: CompletionParser,
}

impl ScanOrchestrator {
    pub fn new(config: ApexeConfig) -> Self {
        let cache = ScanCache::new(config.cache_dir.clone());
        Self {
            config,
            resolver: ToolResolver,
            pipeline: ParserPipeline::new(None),
            cache,
            man_parser: ManPageParser,
            completion_parser: CompletionParser,
        }
    }

    /// Scan one or more CLI tools.
    pub fn scan(
        &self,
        tool_names: &[String],
        no_cache: bool,
        depth: u32,
    ) -> anyhow::Result<Vec<ScannedCLITool>> {
        let mut results = Vec::new();

        for tool_name in tool_names {
            // Resolve binary
            let resolved = self.resolver.resolve(tool_name)?;

            // Check cache
            if !no_cache {
                if let Some(cached) = self.cache.get(tool_name, resolved.version.as_deref()) {
                    info!(tool = %tool_name, "Using cached scan result");
                    results.push(cached);
                    continue;
                }
            }

            // Run --help
            let help_output = Command::new(tool_name)
                .arg("--help")
                .output()?;

            let help_text = if help_output.stdout.is_empty() {
                String::from_utf8_lossy(&help_output.stderr).to_string()
            } else {
                String::from_utf8_lossy(&help_output.stdout).to_string()
            };

            // Parse help text
            let parsed = self.pipeline.parse(&help_text, tool_name, None);

            // Discover subcommands
            let discovery = SubcommandDiscovery::new(&self.pipeline, depth);
            let subcommands = discovery.discover(
                tool_name,
                &[tool_name.to_string()],
                &parsed.subcommand_names,
                0,
            );

            // Build ScannedCLITool
            let mut tool = ScannedCLITool {
                name: tool_name.clone(),
                binary_path: resolved.binary_path,
                version: resolved.version,
                subcommands,
                global_flags: parsed.flags,
                structured_output: parsed.structured_output,
                scan_tier: 1,
                warnings: Vec::new(),
            };

            // Tier 2 enrichment: man pages
            if let Some(man_help) = self.man_parser.parse_man_page(tool_name) {
                if tool.subcommands.is_empty() && !man_help.description.is_empty() {
                    // Enrich description if Tier 1 was sparse
                    tool.scan_tier = 2;
                }
            }

            // Tier 3 enrichment: shell completions
            if let Some(_comp_help) = self.completion_parser.parse_completions(tool_name) {
                tool.scan_tier = tool.scan_tier.max(3);
            }

            // Cache result
            let _ = self.cache.put(&tool);

            results.push(tool);
        }

        Ok(results)
    }
}
```

---

## 16. Test Scenarios

| Test ID | Scenario | Input | Expected |
|---------|----------|-------|----------|
| F2-T01 | Resolve git binary | `resolve("git")` | `ResolvedTool { binary_path: "/usr/bin/git", .. }` |
| F2-T02 | Resolve nonexistent tool | `resolve("zzz_no_such_tool")` | `Err(ToolNotFound)` |
| F2-T03 | Parse GNU help (git commit) | Pre-captured `git commit --help` | `--message/-m STRING required`, `--all/-a BOOLEAN` |
| F2-T04 | Parse Click help (flask) | Pre-captured `flask --help` | Commands: run, shell, routes |
| F2-T05 | Parse Cobra help (kubectl) | Pre-captured `kubectl --help` | Available Commands: apply, get, describe, ... |
| F2-T06 | Parse Clap help (rg) | Pre-captured `rg --help` | `--pattern/-e STRING`, `--type/-t STRING` |
| F2-T07 | Enum detection | `--format {json,text,csv}` in help | `enum_values: Some(vec!["json","text","csv"])` |
| F2-T08 | Default detection | `[default: 80]` in description | `default: Some("80")` |
| F2-T09 | Required detection | `required` in description | `required: true` |
| F2-T10 | Boolean flag detection | `--verbose` with no value | `value_type: ValueType::Boolean` |
| F2-T11 | Path type inference | `--config FILE` | `value_type: ValueType::Path` |
| F2-T12 | Integer type inference | `--count NUM` | `value_type: ValueType::Integer` |
| F2-T13 | Repeatable flag detection | `--include PATTERN (can be repeated)` | `repeatable: true` |
| F2-T14 | Structured output: docker | `--format` with json in enum | `supported: true, flag: "--format json"` |
| F2-T15 | Structured output: none | Tool without json flag | `supported: false` |
| F2-T16 | Subcommand discovery depth | `docker` with depth=2 | `docker container ls` found, not deeper |
| F2-T17 | Cache hit | Second scan of same tool | Returns cached result, no subprocess calls |
| F2-T18 | Cache bypass | `--no-cache` flag | Fresh scan, subprocess called |
| F2-T19 | Timeout handling | Slow tool (mocked) | `Err(ScanTimeout)`, warning in result |
| F2-T20 | Fallback on unparseable help | Gibberish help text | ParsedHelp with raw text as description |
| F2-T21 | Plugin parser used | Custom plugin registered | Plugin's `parse()` called when `can_parse()` returns true |
| F2-T22 | Plugin priority ordering | Multiple plugins | Lower priority number runs first |
| F2-T23 | Man page enrichment | git with man page | Additional descriptions merged |
| F2-T24 | Help on stderr | Tool outputs help to stderr | Help text captured from stderr |

### Example Test (rstest)

```rust
use rstest::rstest;

#[rstest]
#[case("--format {json,text,csv}", vec!["json", "text", "csv"])]
#[case("--output {yaml,toml}", vec!["yaml", "toml"])]
fn test_enum_extraction(#[case] help_line: &str, #[case] expected: Vec<&str>) {
    let flags = extract_flags(&format!("Options:\n  {help_line}  Some description"));
    assert!(!flags.is_empty());
    let enums = flags[0].enum_values.as_ref().unwrap();
    assert_eq!(enums, &expected.iter().map(|s| s.to_string()).collect::<Vec<_>>());
}
```
