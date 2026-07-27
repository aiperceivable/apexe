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
/// `open_world` is not consulted: the CLI adapter sets it unconditionally, so
/// it carries no signal here.
fn risk_from_annotations(annotations: Option<&ModuleAnnotations>) -> Risk {
    match annotations {
        Some(annotations) if annotations.destructive => Risk::Destructive,
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

    fn annotations(readonly: bool, destructive: bool) -> ModuleAnnotations {
        ModuleAnnotations {
            readonly,
            destructive,
            ..Default::default()
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
