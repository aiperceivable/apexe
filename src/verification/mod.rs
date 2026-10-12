//! Source drift and explicitly admitted readonly behavioral verification.
mod behavior;
mod drift;

use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) use behavior::{read_cases, run_cases};
pub(crate) use drift::compare;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Breaking,
    Additive,
    Cosmetic,
    Review,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub before: Value,
    pub after: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Unchanged,
    Changed,
    Unverifiable,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleVerification {
    pub module_id: String,
    pub status: VerificationStatus,
    pub reason: Option<String>,
    pub changes: Vec<Change>,
    /// The binding and executable tested by this report, not an author claim.
    pub binding_digest: String,
    pub binary_digest: Option<String>,
    pub case_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub version: u32,
    pub checked_on: String,
    pub drift: Vec<ModuleVerification>,
    pub behavioral: Vec<ModuleVerification>,
}

impl VerificationReport {
    pub(crate) fn failed_drift(&self, strict: bool) -> bool {
        self.drift.iter().any(|entry| {
            matches!(
                entry.status,
                VerificationStatus::Unverifiable | VerificationStatus::Failed
            ) || entry.changes.iter().any(|change| {
                change.kind == ChangeKind::Breaking || change.kind == ChangeKind::Review || strict
            })
        })
    }

    pub(crate) fn failed_behavioral(&self) -> bool {
        self.behavioral
            .iter()
            .any(|entry| entry.status != VerificationStatus::Passed)
    }
}

pub(crate) fn digest(value: &impl Serialize) -> anyhow::Result<String> {
    let value = serde_json::to_value(value)?;
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

pub(crate) fn binding_digest(module: &ScannedModule) -> anyhow::Result<String> {
    digest(
        &serde_json::json!({"id":module.module_id,"target":module.target,
        "input_schema":module.input_schema,"output_schema":module.output_schema,
        "annotations":module.annotations,"source_contract":module.metadata.get("source_contract")}),
    )
}

pub(crate) fn binary_digest(module: &ScannedModule) -> anyhow::Result<String> {
    use std::io::Read;
    let binary = crate::scanner::resolver::extract_binary_path(&module.target)
        .ok_or_else(|| anyhow::anyhow!("Not a CLI target"))?;
    let mut file = std::fs::File::open(binary)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn result(
    module: &ScannedModule,
    status: VerificationStatus,
    reason: Option<String>,
) -> anyhow::Result<ModuleVerification> {
    Ok(ModuleVerification {
        module_id: module.module_id.clone(),
        status,
        reason,
        changes: vec![],
        binding_digest: binding_digest(module)?,
        binary_digest: binary_digest(module).ok(),
        case_digest: None,
    })
}
