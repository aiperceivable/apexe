//! Source drift and explicitly admitted readonly behavioral verification.
mod behavior;
mod drift;
mod evidence;
mod isolation;
mod runner;

use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) use behavior::BehaviorOptions;
pub(crate) use behavior::{read_cases, run_cases};
pub(crate) use drift::compare;
pub(crate) use evidence::{attach_evidence, record_evidence};
pub use isolation::IsolationMode;

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
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub isolation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub version: u32,
    pub checked_on: String,
    pub drift: Vec<ModuleVerification>,
    pub behavioral: Vec<ModuleVerification>,
    #[serde(default)]
    pub case_source: Option<CaseSource>,
    #[serde(default)]
    pub verifier: Option<VerifierSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierSource {
    pub path: std::path::PathBuf,
    pub version: String,
    pub digest: String,
}

pub(crate) fn verifier_source() -> anyhow::Result<VerifierSource> {
    let path = std::env::current_exe()?;
    Ok(VerifierSource {
        digest: file_digest(&path)?,
        path,
        version: crate::VERSION.into(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSource {
    pub path: std::path::PathBuf,
    pub digest: String,
}

pub(crate) fn case_file_digest(path: &std::path::Path) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "Invalid behavioral case file"
    );
    Ok(format!("{:x}", Sha256::digest(std::fs::read(path)?)))
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
    let mut value = serde_json::to_value(value)?;
    // SDK dependencies may enable preserve_order globally. Object insertion
    // order must not invalidate evidence between independently loaded processes.
    value.sort_all_objects();
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

pub(crate) fn binding_digest(module: &ScannedModule) -> anyhow::Result<String> {
    let mut module = module.clone();
    module.metadata.remove("verification");
    module.metadata.remove("confidence");
    digest(&module)
}

pub(crate) fn binary_digest(module: &ScannedModule) -> anyhow::Result<String> {
    let binary = crate::scanner::resolver::extract_binary_path(&module.target)
        .ok_or_else(|| anyhow::anyhow!("Not a CLI target"))?;
    file_digest(std::path::Path::new(binary))
}

pub(crate) fn file_digest(path: &std::path::Path) -> anyhow::Result<String> {
    use std::io::Read;
    anyhow::ensure!(
        std::fs::metadata(path)?.is_file(),
        "Digest source must be a regular file"
    );
    let mut file = std::fs::File::open(path)?;
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
        mode: None,
        isolation: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_ignores_object_key_order_at_every_depth() {
        let first: serde_json::Value =
            serde_json::from_str(r#"{"first":1,"nested":{"a":2,"b":3},"items":[{"c":4,"d":5}]}"#)
                .unwrap();
        let second: serde_json::Value =
            serde_json::from_str(r#"{"items":[{"d":5,"c":4}],"nested":{"b":3,"a":2},"first":1}"#)
                .unwrap();
        assert_eq!(digest(&first).unwrap(), digest(&second).unwrap());
        assert_ne!(
            digest(&serde_json::json!([1, 2])).unwrap(),
            digest(&serde_json::json!([2, 1])).unwrap()
        );
    }
}
