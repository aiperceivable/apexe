use apcore::module::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::models::ScannedCLITool;

/// Risk class assigned to a scanned command, mirroring the vocabulary used by
/// downstream capability consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    Readonly,
    Write,
    Destructive,
    OpenWorld,
}

/// One invocable command together with the contract a consumer needs to call it.
///
/// This is the flattened, per-command view of a scan. The nested
/// [`ScannedCLITool`] tree describes what was *parsed*; a `CommandContract`
/// describes what can be *invoked*.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandContract {
    /// Stable module identifier (e.g. `cli.git.commit`).
    pub id: String,
    /// Invocation form (e.g. `git commit`).
    pub command: String,
    /// Human-readable description.
    pub description: String,
    /// Inferred risk class.
    pub risk: Risk,
    /// JSON Schema for the command's inputs.
    pub input_schema: JsonValue,
    /// JSON Schema for the command's output.
    pub output_schema: JsonValue,
    /// Hand-written example invocations, verbatim from the man page.
    ///
    /// Command lines rather than structured inputs, deliberately: an example
    /// like `find / \! -name "*.c" -print` does not survive being parsed back
    /// into schema fields, and a consumer that guessed at one would produce a
    /// call the tool rejects. The schema states what *can* be passed; these
    /// state what a human actually passes, which is the part flag parsing
    /// cannot recover.
    ///
    /// Defaulted on deserialize so reports written before this field still load.
    #[serde(default)]
    pub examples: Vec<String>,
}

/// The machine-readable envelope emitted by `apexe scan --format json|yaml`.
///
/// Always a single document, even for a multi-tool scan, so consumers can parse
/// stdout directly. `tools` carries the full parse tree; `commands` is the
/// flattened contract list most consumers actually want.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub tools: Vec<ScannedCLITool>,
    pub commands: Vec<CommandContract>,
}

impl ScanReport {
    /// Build a report from scanned tools and the modules derived from them.
    pub fn new(tools: Vec<ScannedCLITool>, modules: &[ScannedModule]) -> Self {
        let commands = modules.iter().map(CommandContract::from_module).collect();
        Self { tools, commands }
    }
}

impl CommandContract {
    /// Derive a command contract from a converted module.
    fn from_module(module: &ScannedModule) -> Self {
        Self {
            id: module.module_id.clone(),
            command: command_from_module_id(&module.module_id),
            description: module.description.clone(),
            risk: risk_from_annotations(module.annotations.as_ref()),
            input_schema: module.input_schema.clone(),
            output_schema: module.output_schema.clone(),
            // The converter stores each man-page invocation as a `ModuleExample`
            // title with no structured `inputs`, so the title is the whole
            // example. Without this the flattened contract silently lost the
            // examples that the generated binding carried.
            examples: module
                .examples
                .iter()
                .map(|example| example.title.clone())
                .filter(|title| !title.is_empty())
                .collect(),
        }
    }
}

/// Recover the invocation form from a namespaced module id.
///
/// `cli.git.commit` becomes `git commit`.
fn command_from_module_id(module_id: &str) -> String {
    module_id
        .split_once('.')
        .map_or(module_id, |(_namespace, rest)| rest)
        .replace('.', " ")
}

/// Collapse behavioral annotations into a single risk class.
///
/// Destructive outranks open-world: irreversibility is the property a consumer
/// must gate on first, and a command that is both is already the stricter case.
/// Open-world outranks readonly, because `curl` reading a URL still crosses a
/// trust boundary — that is what a credential gate and a network-denying sandbox
/// key on, and calling it readonly would hide it from both.
fn risk_from_annotations(annotations: Option<&ModuleAnnotations>) -> Risk {
    match annotations {
        Some(annotations) if annotations.destructive => Risk::Destructive,
        Some(annotations) if annotations.open_world => Risk::OpenWorld,
        Some(annotations) if annotations.readonly => Risk::Readonly,
        _ => Risk::Write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn module(module_id: &str, annotations: Option<ModuleAnnotations>) -> ScannedModule {
        let mut module = ScannedModule::new(
            module_id.to_string(),
            "A description".to_string(),
            json!({"type": "object"}),
            json!({"type": "object"}),
            vec![],
            format!("exec:///usr/bin/{module_id}"),
        );
        module.annotations = annotations;
        module
    }

    /// `open_world` is set explicitly rather than defaulted: apcore's default is
    /// `true` (MCP's conservative `openWorldHint`), which would make every
    /// fixture here an open-world command and hide what these cases assert.
    fn annotations(readonly: bool, destructive: bool) -> ModuleAnnotations {
        ModuleAnnotations {
            readonly,
            destructive,
            open_world: false,
            ..Default::default()
        }
    }

    fn open_world_annotations() -> ModuleAnnotations {
        ModuleAnnotations {
            open_world: true,
            ..annotations(false, false)
        }
    }

    #[test]
    fn test_command_from_module_id_strips_namespace() {
        assert_eq!(command_from_module_id("cli.git.commit"), "git commit");
    }

    #[test]
    fn test_command_from_module_id_root_command() {
        assert_eq!(command_from_module_id("cli.ls"), "ls");
    }

    #[test]
    fn test_risk_readonly() {
        assert_eq!(
            risk_from_annotations(Some(&annotations(true, false))),
            Risk::Readonly
        );
    }

    #[test]
    fn test_risk_destructive_wins_over_readonly() {
        assert_eq!(
            risk_from_annotations(Some(&annotations(true, true))),
            Risk::Destructive
        );
    }

    #[test]
    fn test_risk_open_world() {
        // The class exists so a consumer can demand a credential boundary and
        // refuse to run the command in a local sandbox. It was never emitted
        // while `open_world` was hardcoded true and therefore ignored here.
        assert_eq!(
            risk_from_annotations(Some(&open_world_annotations())),
            Risk::OpenWorld
        );
    }

    #[test]
    fn test_risk_destructive_wins_over_open_world() {
        let mut annotations = open_world_annotations();
        annotations.destructive = true;
        assert_eq!(risk_from_annotations(Some(&annotations)), Risk::Destructive);
    }

    #[test]
    fn test_risk_open_world_wins_over_readonly() {
        // `curl` reading a URL still crosses a trust boundary; reporting it as
        // readonly would hide it from the gate that exists for exactly that.
        let mut annotations = open_world_annotations();
        annotations.readonly = true;
        assert_eq!(risk_from_annotations(Some(&annotations)), Risk::OpenWorld);
    }

    #[test]
    fn test_risk_defaults_to_write() {
        assert_eq!(risk_from_annotations(None), Risk::Write);
        assert_eq!(
            risk_from_annotations(Some(&annotations(false, false))),
            Risk::Write
        );
    }

    #[test]
    fn test_risk_serializes_as_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Risk::OpenWorld).unwrap(),
            "\"open-world\""
        );
        assert_eq!(
            serde_json::to_string(&Risk::Readonly).unwrap(),
            "\"readonly\""
        );
    }

    #[test]
    fn test_scan_report_flattens_modules_into_commands() {
        let modules = vec![
            module("cli.git.commit", Some(annotations(false, false))),
            module("cli.git.log", Some(annotations(true, false))),
        ];
        let report = ScanReport::new(vec![], &modules);
        assert_eq!(report.commands.len(), 2);
        assert_eq!(report.commands[0].command, "git commit");
        assert_eq!(report.commands[0].risk, Risk::Write);
        assert_eq!(report.commands[1].command, "git log");
        assert_eq!(report.commands[1].risk, Risk::Readonly);
    }

    #[test]
    fn test_scan_report_round_trips_through_json() {
        let modules = vec![module("cli.ls", Some(annotations(true, false)))];
        let report = ScanReport::new(vec![], &modules);
        let encoded = serde_json::to_string(&report).unwrap();
        let decoded: ScanReport = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.commands.len(), 1);
        assert_eq!(decoded.commands[0].id, "cli.ls");
    }
}
