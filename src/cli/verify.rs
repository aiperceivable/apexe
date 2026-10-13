use crate::config::ApexeConfig;
use crate::verification::{ModuleVerification, VerificationReport, VerificationStatus};
use apcore_toolkit::ScannedModule;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Compare fresh source contracts with bindings without replacing the baseline.
#[derive(Debug, Args, Serialize, Deserialize)]
pub struct VerifyArgs {
    #[arg(long)]
    pub bindings_dir: Option<PathBuf>,
    /// Explicit readonly behavioral cases in JSON; no behavioral execution by default.
    #[arg(long)]
    pub cases: Option<PathBuf>,
    /// Allow explicitly reviewed, digest-pinned dry-run cases; isolation is mandatory.
    #[arg(long, requires = "cases")]
    #[serde(default)]
    pub allow_dry_run: bool,
    /// Operating-system isolation policy for behavioral verification.
    #[arg(long, value_enum, default_value = "auto")]
    #[serde(default)]
    pub isolation: crate::verification::IsolationMode,
    /// Record digest-bound case evidence for subsequent binding loads.
    #[arg(long, requires = "cases")]
    #[serde(default)]
    pub record: bool,
    /// Reapply the explicit overlay used to produce the published baseline.
    #[arg(long)]
    pub overlay: Option<PathBuf>,
    /// Persist the result and its binding/binary digests as JSON.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Fail on additive and cosmetic drift as well as breaking or unclassified changes.
    #[arg(long)]
    pub strict: bool,
    #[arg(long, default_value = "2", value_parser = clap::value_parser!(u32).range(1..=5))]
    pub depth: u32,
}

impl VerifyArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        let bindings = self.bindings_dir.as_deref().unwrap_or(&config.bindings_dir);
        let modules =
            crate::output::load_modules_from_dir(bindings, config.bindings_pattern.as_deref())?;
        anyhow::ensure!(!modules.is_empty(), "No bindings to verify");
        let drift = self.check_sources(config, &modules)?;
        let verifier = self
            .cases
            .is_some()
            .then(crate::verification::verifier_source)
            .transpose()?;
        let (behavioral, case_source) = self.check_behavior(config, bindings, &modules)?;
        let report = VerificationReport {
            version: 1,
            checked_on: chrono::Utc::now().to_rfc3339(),
            drift,
            behavioral,
            case_source,
            verifier,
        };
        if self.record {
            crate::verification::record_evidence(bindings, &report)?;
        }
        let json = serde_json::to_vec_pretty(&report)?;
        if let Some(path) = self.report {
            write_report(&path, bindings, &json)?;
        }
        use std::io::Write;
        std::io::stdout().lock().write_all(&json)?;
        writeln!(std::io::stdout().lock())?;
        // An omitted behavioral run is recorded honestly without failing L0.
        let failed = report
            .drift
            .iter()
            .any(|entry| entry.status == VerificationStatus::Unverifiable)
            || report.failed_drift(self.strict)
            || self.cases.is_some() && report.failed_behavioral();
        anyhow::ensure!(
            !failed,
            "Verification found breaking, unclassified or unverifiable contracts"
        );
        Ok(())
    }

    fn check_behavior(
        &self,
        config: &ApexeConfig,
        bindings: &std::path::Path,
        modules: &[ScannedModule],
    ) -> anyhow::Result<(
        Vec<ModuleVerification>,
        Option<crate::verification::CaseSource>,
    )> {
        let mut case_source = None;
        let behavioral = match self.cases.as_deref() {
            Some(path) => {
                let cases = crate::verification::read_cases(path)?;
                case_source = cases.source.clone();
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                runtime.block_on(crate::verification::run_cases(cases, modules, bindings, config.default_timeout,
                    crate::verification::BehaviorOptions { allow_dry_run: self.allow_dry_run, isolation: self.isolation }))?
            }
            None => modules.iter().map(|module| crate::verification::result(module, VerificationStatus::Unverifiable,
                Some("Behavioral verification not requested; overlay confidence is an author assertion".into()))).collect::<anyhow::Result<Vec<_>>>()?,
        };
        Ok((behavioral, case_source))
    }

    fn check_sources(
        &self,
        config: &ApexeConfig,
        modules: &[ScannedModule],
    ) -> anyhow::Result<Vec<ModuleVerification>> {
        let mut groups: BTreeMap<String, Vec<ScannedModule>> = BTreeMap::new();
        for module in modules {
            let binary = crate::scanner::resolver::extract_binary_path(&module.target)
                .ok_or_else(|| anyhow::anyhow!("Verification currently requires CLI bindings"))?;
            groups
                .entry(binary.into())
                .or_default()
                .push(module.clone());
        }
        let mut report = Vec::new();
        let mut scanner = crate::scanner::ScanOrchestrator::new(config.clone());
        if let Some(path) = &self.overlay {
            scanner.load_overlay(path)?;
        }
        for (binary, previous) in groups {
            let outcome = scanner.scan(&[binary], true, self.depth);
            if !outcome.failures.is_empty() {
                for module in &previous {
                    report.push(crate::verification::result(
                        module,
                        VerificationStatus::Unverifiable,
                        Some(
                            "Source could not be rescanned; no unchanged verdict is inferred"
                                .into(),
                        ),
                    )?);
                }
                continue;
            }
            let fresh = crate::adapter::CliToolConverter::new().convert_all(&outcome.tools);
            report.extend(crate::verification::compare(&previous, &fresh)?);
        }
        Ok(report)
    }
}

fn write_report(
    path: &std::path::Path,
    bindings: &std::path::Path,
    json: &[u8],
) -> anyhow::Result<()> {
    let absolute = std::path::absolute(path)?;
    let destination = if absolute.exists() {
        absolute.canonicalize()?
    } else {
        absolute
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Report needs a parent directory"))?
            .canonicalize()?
            .join(
                absolute
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("Report needs a filename"))?,
            )
    };
    anyhow::ensure!(
        !destination.starts_with(bindings.canonicalize()?),
        "Verification reports must stay outside the binding baseline directory"
    );
    std::fs::write(absolute, json)?;
    Ok(())
}
