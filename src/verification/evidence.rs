use super::{binary_digest, binding_digest, VerificationReport, VerificationStatus};
use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

const RECORD_DIRECTORY: &str = ".apexe-verification";
const RECORD_NAME: &str = "evidence.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceRecord {
    version: u32,
    checked_on: String,
    cases: super::CaseSource,
    verifier: super::VerifierSource,
    modules: BTreeMap<String, CaseEvidence>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseEvidence {
    binding_digest: String,
    binary_digest: String,
    case_digests: Vec<String>,
    modes: Vec<String>,
    isolation: Vec<String>,
}

pub(crate) fn record_evidence(bindings: &Path, report: &VerificationReport) -> anyhow::Result<()> {
    let cases = report
        .case_source
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No explicit case source to record"))?;
    anyhow::ensure!(
        super::case_file_digest(&cases.path)? == cases.digest,
        "Case definitions changed during verification"
    );
    let verifier = report
        .verifier
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No verifier identity to record"))?;
    anyhow::ensure!(
        super::file_digest(&verifier.path)? == verifier.digest,
        "Verifier changed during verification"
    );
    let mut record = EvidenceRecord {
        version: 1,
        checked_on: report.checked_on.clone(),
        cases: cases.clone(),
        verifier: verifier.clone(),
        modules: BTreeMap::new(),
    };
    for drift in &report.drift {
        if let Some(evidence) = collect_case_evidence(report, drift) {
            record.modules.insert(drift.module_id.clone(), evidence);
        }
    }
    let directory = bindings.join(RECORD_DIRECTORY);
    std::fs::create_dir_all(&directory)?;
    anyhow::ensure!(
        !std::fs::symlink_metadata(&directory)?
            .file_type()
            .is_symlink(),
        "Verification directory must not be a symlink"
    );
    let destination = directory.join(RECORD_NAME);
    if std::fs::symlink_metadata(&destination)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        anyhow::bail!("Verification record must not be a symlink");
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(&mut temporary, &record)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(destination)?;
    Ok(())
}

fn collect_case_evidence(
    report: &VerificationReport,
    drift: &super::ModuleVerification,
) -> Option<CaseEvidence> {
    let cases: Vec<_> = report
        .behavioral
        .iter()
        .filter(|case| case.module_id == drift.module_id)
        .collect();
    if drift.status != VerificationStatus::Unchanged
        || cases
            .iter()
            .any(|case| case.status != VerificationStatus::Passed)
    {
        return None;
    }
    let binary = cases.first()?.binary_digest.as_ref()?;
    if cases.iter().any(|case| {
        case.case_digest.is_none()
            || case.binding_digest != drift.binding_digest
            || case.binary_digest.as_ref() != Some(binary)
    }) {
        return None;
    }
    Some(CaseEvidence {
        binding_digest: drift.binding_digest.clone(),
        binary_digest: binary.clone(),
        case_digests: cases
            .iter()
            .filter_map(|case| case.case_digest.clone())
            .collect(),
        modes: cases.iter().filter_map(|case| case.mode.clone()).collect(),
        isolation: cases
            .iter()
            .filter_map(|case| case.isolation.clone())
            .collect(),
    })
}

pub(crate) fn attach_evidence(
    bindings: &Path,
    modules: &mut [ScannedModule],
) -> anyhow::Result<()> {
    for module in modules.iter_mut() {
        module.metadata.remove("verification");
        if module
            .metadata
            .get("confidence")
            .is_some_and(|confidence| confidence == "verified")
        {
            module
                .metadata
                .insert("confidence".into(), serde_json::json!("high"));
        }
    }
    let Some(record) = read_record(bindings)? else {
        return Ok(());
    };
    if super::case_file_digest(&record.cases.path).ok().as_ref() != Some(&record.cases.digest) {
        return Ok(());
    }
    if record.verifier.version != crate::VERSION
        || super::file_digest(&record.verifier.path).ok().as_ref() != Some(&record.verifier.digest)
    {
        return Ok(());
    }
    for module in modules {
        if let Some(evidence) = record.modules.get(&module.module_id) {
            apply_case_evidence(module, evidence, &record.checked_on)?;
            if let Some(verification) = module.metadata.get_mut("verification") {
                verification["verifier"] = serde_json::json!({"version":record.verifier.version,"digest":record.verifier.digest});
            }
        }
    }
    Ok(())
}

fn read_record(bindings: &Path) -> anyhow::Result<Option<EvidenceRecord>> {
    let directory = bindings.join(RECORD_DIRECTORY);
    if directory.try_exists()? {
        anyhow::ensure!(
            !std::fs::symlink_metadata(&directory)?
                .file_type()
                .is_symlink(),
            "Verification directory must not be a symlink"
        );
    }
    let path = directory.join(RECORD_NAME);
    if !path.try_exists()? {
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "Invalid verification record file"
    );
    let record: EvidenceRecord = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        record.version == 1,
        "Unsupported verification record version"
    );
    Ok(Some(record))
}

fn apply_case_evidence(
    module: &mut ScannedModule,
    evidence: &CaseEvidence,
    checked_on: &str,
) -> anyhow::Result<()> {
    if evidence.case_digests.is_empty()
        || evidence
            .case_digests
            .iter()
            .any(|digest| !valid_digest(digest))
        || binding_digest(module)? != evidence.binding_digest
        || binary_digest(module).ok().as_ref() != Some(&evidence.binary_digest)
    {
        return Ok(());
    }
    module.metadata.insert(
        "confidence".into(),
        serde_json::json!(crate::models::Confidence::Verified),
    );
    module.metadata.insert(
        "verification".into(),
        serde_json::json!({
            "scope":"explicit_cases", "checked_on":checked_on, "case_digests":evidence.case_digests,
            "binding_digest":evidence.binding_digest,"binary_digest":evidence.binary_digest,
            "modes":evidence.modes,"isolation":evidence.isolation,
        }),
    );
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
