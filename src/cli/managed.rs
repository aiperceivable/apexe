//! `apexe managed`: backend probes and policy compilation.
//!
//! These are the two operator-facing verbs the v1 specification needs before a
//! session can start: what the selected backend can enforce, and whether a
//! policy compiles against it. Launching a session is not wired to a CLI yet —
//! the Linux isolation backend is the release gate and does not exist on this
//! host, so a `session` verb would be a promise nothing keeps.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::config::ApexeConfig;
use crate::managed::backend::{probe, BackendId, CapabilityReport};
use crate::managed::effect::RootId;
use crate::managed::error::ManagedError;
use crate::managed::policy::{Policy, ResourceMap, RootKind, RootSpec};

/// `apexe managed <command>`
#[derive(Debug, Args)]
pub struct ManagedArgs {
    #[command(subcommand)]
    pub command: ManagedCommand,
}

/// The managed subcommands.
#[derive(Debug, Subcommand)]
pub enum ManagedCommand {
    /// Report what the selected isolation backend can enforce right now.
    Probe(ProbeArgs),
    /// Validate a policy and compile it against the selected backend.
    #[command(name = "policy-check")]
    PolicyCheck(PolicyCheckArgs),
}

/// `apexe managed probe`
#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// Isolation backend to probe.
    #[arg(long, default_value = "linux", value_parser = ["linux", "sbpl"])]
    pub backend: String,

    /// Output format.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,
}

/// `apexe managed policy-check`
#[derive(Debug, Args)]
pub struct PolicyCheckArgs {
    /// Policy file to validate.
    #[arg(long, value_name = "FILE")]
    pub file: PathBuf,

    /// Backend the policy will be compiled for.
    #[arg(long, default_value = "linux", value_parser = ["linux", "sbpl"])]
    pub backend: String,

    /// Resource root the launcher will provide. Repeatable; `id` means a
    /// writable workspace and `id:reference` means an immutable reference.
    /// Defaults to `workspace` plus `reference:reference`.
    #[arg(long = "root", value_name = "ID[:reference]")]
    pub roots: Vec<String>,

    /// Output format.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    pub format: String,
}

impl ManagedArgs {
    /// Run the selected managed command.
    pub fn execute(&self, _config: &ApexeConfig) -> anyhow::Result<()> {
        match &self.command {
            ManagedCommand::Probe(args) => args.execute(),
            ManagedCommand::PolicyCheck(args) => args.execute(),
        }
    }
}

impl ProbeArgs {
    fn execute(&self) -> anyhow::Result<()> {
        let backend = BackendId::parse(&self.backend)?;
        let report = probe(backend);
        if self.format == "json" {
            println!("{}", serde_json::to_string_pretty(&report.to_json())?);
        } else {
            print_report(&report);
        }
        Ok(())
    }
}

impl PolicyCheckArgs {
    fn execute(&self) -> anyhow::Result<()> {
        let backend = BackendId::parse(&self.backend)?;
        let report = probe(backend);
        let resources = resource_map(&self.roots)?;
        let bytes = std::fs::read(&self.file).map_err(|error| {
            anyhow::anyhow!(
                "POLICY_INVALID: cannot read {}: {error}",
                self.file.display()
            )
        })?;
        let outcome = Policy::parse(&bytes, &resources)
            .and_then(|policy| policy.compile(&report).map(|compiled| (policy, compiled)));
        match outcome {
            Ok((policy, compiled)) => {
                if self.format == "json" {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "backend": backend.as_str(),
                            "supported": report.supported,
                            "policy_digest": compiled.digest,
                            "forms": compiled.forms.keys().map(|form| form.as_str()).collect::<Vec<_>>(),
                            "capabilities": report.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                            "missing": report.missing,
                            "notes": report.notes,
                            "policy": policy.to_json(),
                        }))?
                    );
                } else {
                    println!("Policy: {}", self.file.display());
                    println!(
                        "Backend: {} (supported: {})",
                        backend.as_str(),
                        report.supported
                    );
                    println!("Digest: {}", compiled.digest);
                    println!(
                        "Forms: {}",
                        compiled
                            .forms
                            .keys()
                            .map(|form| form.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    print_report(&report);
                }
                Ok(())
            }
            Err(error) => Err(managed_failure(&error)),
        }
    }
}

/// Build the launcher's resource map from `--root` values.
fn resource_map(roots: &[String]) -> anyhow::Result<ResourceMap> {
    let defaults = ["workspace".to_string(), "reference:reference".to_string()];
    let values: &[String] = if roots.is_empty() { &defaults } else { roots };
    let mut map = ResourceMap::new();
    for value in values {
        let (id, kind) = match value.split_once(':') {
            Some((id, "reference")) => (id, RootKind::Reference),
            Some((id, "workspace")) => (id, RootKind::Workspace),
            Some((_, other)) => {
                anyhow::bail!(
                    "POLICY_INVALID: unknown root kind '{other}'; use 'id' or 'id:reference'"
                );
            }
            None => (value.as_str(), RootKind::Workspace),
        };
        let id = RootId::parse(id).map_err(|error| anyhow::anyhow!("{}", error.message))?;
        map.add(
            id,
            RootSpec {
                kind,
                protected: Vec::new(),
            },
        );
    }
    Ok(map)
}

fn print_report(report: &CapabilityReport) {
    println!(
        "Backend: {} (supported: {})",
        report.backend.as_str(),
        report.supported
    );
    if let Some(abi) = report.landlock_abi {
        println!("Landlock ABI: {abi}");
    }
    println!(
        "Capabilities: {}",
        if report.capabilities.is_empty() {
            "none".to_string()
        } else {
            report
                .capabilities
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    for missing in &report.missing {
        println!("Missing: {missing}");
    }
    for note in &report.notes {
        println!("Note: {note}");
    }
}

fn managed_failure(error: &ManagedError) -> anyhow::Error {
    let mut message = format!(
        "{} ({}): {}",
        error.code.as_str(),
        error.phase,
        error.message
    );
    if let Some(field) = &error.field {
        message.push_str(&format!(" [field: {field}]"));
    }
    if let Some(rule_id) = &error.rule_id {
        message.push_str(&format!(" [rule: {rule_id}]"));
    }
    anyhow::anyhow!(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_the_default_resource_map_has_a_workspace_and_a_reference() {
        let map = resource_map(&[]).unwrap();
        assert!(!map.is_reference(&RootId::parse("workspace").unwrap()));
        assert!(map.is_reference(&RootId::parse("reference").unwrap()));
    }

    #[test]
    fn test_an_explicit_root_kind_is_honoured() {
        let map = resource_map(&["data:reference".to_string()]).unwrap();
        assert!(map.is_reference(&RootId::parse("data").unwrap()));
    }

    #[test]
    fn test_an_unknown_root_kind_is_refused() {
        let error = resource_map(&["data:elsewhere".to_string()]).unwrap_err();
        assert!(error.to_string().contains("POLICY_INVALID"));
    }
}
