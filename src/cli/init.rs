use std::collections::BTreeMap;
use std::path::PathBuf;

use apcore_toolkit::ScannedModule;
use serde::{Deserialize, Serialize};

use crate::config::ApexeConfig;
use crate::scanner::ScanOrchestrator;

const FALLBACK_TOOLS: &[&str] = &["cat", "head", "ls", "wc"];
const OWNER_KEY: &str = "apexe_init_source";

/// Prepare reviewed command bindings and a starter ACL for this machine.
#[derive(Debug, clap::Args, Serialize, Deserialize)]
#[command(
    after_help = "Examples:\n  apexe init                                Prepare this machine\n  apexe init --offline --format json         Use cached or local rules\n  apexe init --refresh                       Refresh unchanged init-owned bindings\n\nNext: inspect 'apexe list --verbose', review the ACL, then use 'apexe exec',\n'apexe env --enable' or 'apexe serve --acl ~/.apexe/acl.yaml'."
)]
pub struct InitArgs {
    /// Rules Git repository; defaults to default_overlay_repository.
    #[arg(long, value_name = "SOURCE", value_parser = parse_source)]
    pub overlays: Option<String>,
    /// Pin a branch, tag or commit for the selected rules repository.
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// Update the rules snapshot and regenerate bindings owned by init.
    #[arg(long)]
    pub refresh: bool,
    /// Disable network downloads; cached and local Git sources still work.
    #[arg(long, env = "APEXE_OFFLINE")]
    pub offline: bool,
    /// Initialization report format.
    #[arg(long, value_enum, default_value = "table")]
    pub format: ReportFormat,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFormat {
    Table,
    Json,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolStatus {
    Ready,
    Existing,
    NotInstalled,
    NoMatchingOverlay,
    InvalidName,
    Failed,
}

#[derive(Serialize, Deserialize)]
struct ToolReport {
    command: String,
    status: ToolStatus,
    detail: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct InitReport {
    corpora: Vec<crate::corpus::InstalledCorpus>,
    degraded: bool,
    warnings: Vec<String>,
    bindings_dir: PathBuf,
    acl: PathBuf,
    tools: Vec<ToolReport>,
    written: Vec<String>,
    preserved: Vec<String>,
    native_ready: Vec<String>,
}

fn parse_source(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err("Rules source must not be empty".into());
    }
    Ok(value.into())
}

impl InitArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        self.validate(config)?;
        write_progress(self.format, format_args!("[1/3] Preparing CLI rules..."))?;
        let source = crate::corpus::bootstrap::prepare(
            config,
            self.overlays.as_deref(),
            self.reference.as_deref(),
            self.refresh,
            self.offline,
        )?;
        let orchestrator = ScanOrchestrator::new(config.clone());
        let mut candidates = orchestrator.covered_commands();
        let fallback = candidates.is_empty();
        let degraded = !source.warnings.is_empty() || fallback;
        let mut warnings = source.warnings;
        if candidates.is_empty() {
            candidates = FALLBACK_TOOLS.iter().map(|name| (*name).into()).collect();
            warnings.push(
                "No usable rules loaded; fallback bindings have no reviewed native shim contract"
                    .into(),
            );
        }
        let report = self.initialize(config, &orchestrator, candidates, degraded, warnings)?;
        write_report(&report, self.format)?;
        anyhow::ensure!(
            !report
                .tools
                .iter()
                .any(|tool| matches!(tool.status, ToolStatus::Failed)),
            "Initialization encountered scan failures; inspect the report"
        );
        anyhow::ensure!(!report.written.is_empty() || !report.preserved.is_empty(),
            "No applicable installed tools were initialized; inspect the report or use apexe scan <tool>");
        Ok(())
    }

    fn validate(&self, config: &ApexeConfig) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=5).contains(&config.scan_depth),
            "Initialization scan_depth must be between 1 and 5"
        );
        anyhow::ensure!(
            config.default_timeout > 0,
            "Initialization timeout must be positive"
        );
        validate_existing_acl(config)?;
        if let Some(source) = &self.overlays {
            parse_source(source).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }

    fn initialize(
        &self,
        config: &ApexeConfig,
        orchestrator: &ScanOrchestrator,
        candidates: Vec<String>,
        degraded: bool,
        warnings: Vec<String>,
    ) -> anyhow::Result<InitReport> {
        let mut report = create_report(config, degraded, warnings)?;
        let scan = scan_options(config, candidates);
        write_progress(
            self.format,
            format_args!("[2/3] Scanning {} candidate tool(s)...", scan.tools.len()),
        )?;
        let modules = discover_modules(
            orchestrator,
            &scan,
            orchestrator.overlay_count() == 0,
            &mut report,
            self.format,
            config.json_output_preference,
        )?;
        let existing = crate::output::load_modules_from_dir(
            &config.bindings_dir,
            config.bindings_pattern.as_deref(),
        )?;
        let protected = protected_bindings(config, &existing)?;
        let admitted = select_writes(modules, &protected, self.refresh, &mut report);
        validate_write_pattern(config, &admitted)?;
        if report
            .preserved
            .iter()
            .any(|id| !existing.iter().any(|module| &module.module_id == id))
        {
            report.degraded = true;
            report.warnings.push(
                "Some preserved bindings are excluded by bindings.pattern and are not available to execution".into(),
            );
        }
        report.native_ready = native_ready_modules(&existing, &admitted);
        write_progress(
            self.format,
            format_args!("[3/3] Saving bindings and ACL..."),
        )?;
        save_bindings(&scan, config, &admitted, &existing, &report.acl)?;
        Ok(report)
    }
}

fn save_bindings(
    scan: &super::ScanArgs,
    config: &ApexeConfig,
    admitted: &[ScannedModule],
    existing: &[ScannedModule],
    acl: &std::path::Path,
) -> anyhow::Result<()> {
    if !admitted.is_empty() {
        scan.write_bindings(admitted, &config.bindings_dir)?;
        scan.write_acl(admitted, config)?;
    } else if !acl.exists() && !existing.is_empty() {
        scan.write_acl(existing, config)?;
    }
    Ok(())
}

fn create_report(
    config: &ApexeConfig,
    degraded: bool,
    warnings: Vec<String>,
) -> anyhow::Result<InitReport> {
    Ok(InitReport {
        corpora: crate::corpus::CorpusStore::new(config).list()?,
        degraded,
        warnings,
        bindings_dir: config.bindings_dir.clone(),
        acl: config.config_dir.join("acl.yaml"),
        tools: vec![],
        written: vec![],
        preserved: vec![],
        native_ready: vec![],
    })
}

fn protected_bindings(
    config: &ApexeConfig,
    selected: &[ScannedModule],
) -> anyhow::Result<Vec<ScannedModule>> {
    let mut protected = selected.to_vec();
    if config.bindings_pattern.is_some() {
        // The writer always uses canonical filenames, even when the loader's
        // configured pattern excludes them. Protect those files from replacement.
        for module in crate::output::load_modules_from_dir(&config.bindings_dir, None)? {
            if !protected
                .iter()
                .any(|existing| existing.module_id == module.module_id)
            {
                protected.push(module);
            }
        }
    }
    Ok(protected)
}

fn validate_write_pattern(config: &ApexeConfig, modules: &[ScannedModule]) -> anyhow::Result<()> {
    if config.bindings_pattern.is_none() || modules.is_empty() {
        return Ok(());
    }
    let staging = tempfile::tempdir()?;
    crate::output::YamlOutput::without_verification().write(modules, staging.path(), false)?;
    let visible =
        crate::output::load_modules_from_dir(staging.path(), config.bindings_pattern.as_deref())?;
    anyhow::ensure!(
        visible.len() == modules.len(),
        "Configured bindings.pattern excludes initialized binding filenames; use a pattern that includes *.binding.yaml"
    );
    Ok(())
}

fn validate_existing_acl(config: &ApexeConfig) -> anyhow::Result<()> {
    let path = config.config_dir.join("acl.yaml");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            crate::governance::AclManager::from_config(&path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn scan_options(config: &ApexeConfig, tools: Vec<String>) -> super::ScanArgs {
    super::ScanArgs {
        tools,
        output_dir: None,
        depth: config.scan_depth,
        no_cache: true,
        offline: true,
        format: "json".into(),
        skills_dir: None,
        overlay: None,
        verify: true,
        dry_run: false,
    }
}

fn discover_modules(
    orchestrator: &ScanOrchestrator,
    scan: &super::ScanArgs,
    fallback: bool,
    report: &mut InitReport,
    format: ReportFormat,
    json_output_preference: bool,
) -> anyhow::Result<Vec<ScannedModule>> {
    let mut modules = Vec::new();
    for (index, name) in scan.tools.iter().enumerate() {
        write_progress(
            format,
            format_args!(
                "  [{}/{}] Checking {}...",
                index + 1,
                scan.tools.len(),
                name.escape_debug()
            ),
        )?;
        modules.extend(discover_command(
            orchestrator,
            scan,
            name,
            fallback,
            report,
            json_output_preference,
        ));
    }
    Ok(modules)
}

fn discover_command(
    orchestrator: &ScanOrchestrator,
    scan: &super::ScanArgs,
    name: &String,
    fallback: bool,
    report: &mut InitReport,
    json_output_preference: bool,
) -> Vec<ScannedModule> {
    if !super::shim::valid_name(name) {
        report
            .tools
            .push(tool_report(name, ToolStatus::InvalidName, None));
        return Vec::new();
    }
    if !command_is_available(name) {
        report
            .tools
            .push(tool_report(name, ToolStatus::NotInstalled, None));
        return Vec::new();
    }
    let outcome = orchestrator.scan(std::slice::from_ref(name), scan.no_cache, scan.depth);
    for failure in outcome.failures {
        report
            .tools
            .push(tool_report(name, ToolStatus::Failed, Some(failure.error)));
    }
    let mut modules = Vec::new();
    for tool in outcome.tools {
        if !fallback && tool.overlay.is_none() {
            report
                .tools
                .push(tool_report(name, ToolStatus::NoMatchingOverlay, None));
            continue;
        }
        report
            .tools
            .push(tool_report(name, ToolStatus::Ready, tool.overlay.clone()));
        let mut converted = crate::adapter::CliToolConverter::new()
            .with_json_output_preference(json_output_preference)
            .convert(&tool);
        for module in &mut converted {
            stamp_module(module, tool.overlay.as_deref().unwrap_or("fallback"));
        }
        modules.extend(converted);
    }
    modules
}

/// Human-facing progress is part of the table output, independent of logging.
fn write_progress(format: ReportFormat, message: std::fmt::Arguments<'_>) -> anyhow::Result<()> {
    use std::io::Write;
    if matches!(format, ReportFormat::Json) {
        return Ok(());
    }
    let mut output = std::io::stdout().lock();
    writeln!(output, "{message}")?;
    output.flush()?;
    Ok(())
}

fn command_is_available(name: &str) -> bool {
    which::which_in(
        name,
        Some(crate::runtime_path::search_path()),
        std::env::current_dir().unwrap_or_default(),
    )
    .is_ok()
}

fn stamp_module(module: &mut ScannedModule, source: &str) {
    module.metadata.insert(
        "apexe_scan_format".into(),
        serde_json::json!(crate::output::yaml::SCAN_FORMAT_VERSION),
    );
    if let Ok(digest) = ownership_digest(module) {
        module.metadata.insert(
            OWNER_KEY.into(),
            serde_json::json!({"source":source,"digest":digest}),
        );
    }
}

fn ownership_digest(module: &ScannedModule) -> anyhow::Result<String> {
    let mut original = module.clone();
    original.metadata.remove(OWNER_KEY);
    // YAMLWriter omits transient scanner warnings from binding documents.
    original.warnings.clear();
    original.metadata.remove("confidence");
    original.metadata.remove("verification");
    let mut canonical = serde_json::to_value(original)?;
    canonical.sort_all_objects();
    crate::verification::digest(&canonical)
}

fn native_ready_modules(existing: &[ScannedModule], writes: &[ScannedModule]) -> Vec<String> {
    let mut modules: BTreeMap<&str, &ScannedModule> = existing
        .iter()
        .map(|module| (module.module_id.as_str(), module))
        .collect();
    for module in writes {
        modules.insert(&module.module_id, module);
    }
    modules
        .values()
        .filter(|module| super::shim::eligible(module).is_ok())
        .map(|module| module.module_id.clone())
        .collect()
}

fn is_owned(module: &ScannedModule) -> bool {
    let Some(expected) = module
        .metadata
        .get(OWNER_KEY)
        .and_then(|owner| owner["digest"].as_str())
    else {
        return false;
    };
    ownership_digest(module).is_ok_and(|actual| actual == expected)
}

fn tool_report(name: &str, status: ToolStatus, detail: Option<String>) -> ToolReport {
    ToolReport {
        command: name.into(),
        status,
        detail,
    }
}

fn select_writes(
    modules: Vec<ScannedModule>,
    existing: &[ScannedModule],
    refresh: bool,
    report: &mut InitReport,
) -> Vec<ScannedModule> {
    let existing: BTreeMap<&str, &ScannedModule> = existing
        .iter()
        .map(|module| (module.module_id.as_str(), module))
        .collect();
    let mut writes = Vec::new();
    for module in modules {
        if let Some(previous) = existing.get(module.module_id.as_str()) {
            if !refresh || !is_owned(previous) {
                report.preserved.push(module.module_id.clone());
                continue;
            }
        }
        report.written.push(module.module_id.clone());
        writes.push(module);
    }
    if writes.is_empty() && !report.preserved.is_empty() {
        for tool in &mut report.tools {
            if matches!(tool.status, ToolStatus::Ready) {
                tool.status = ToolStatus::Existing;
            }
        }
    }
    writes
}

fn write_report(report: &InitReport, format: ReportFormat) -> anyhow::Result<()> {
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    if matches!(format, ReportFormat::Json) {
        serde_json::to_writer_pretty(&mut output, report)?;
        writeln!(output)?;
        return Ok(());
    }
    write_rules_summary(&mut output, report)?;
    for tool in &report.tools {
        writeln!(
            output,
            "{}: {} {}",
            tool.command,
            serde_json::to_value(&tool.status)?
                .as_str()
                .unwrap_or("unknown"),
            tool.detail.as_deref().unwrap_or("")
        )?;
    }
    for warning in &report.warnings {
        writeln!(output, "Warning: {warning}")?;
    }
    writeln!(
        output,
        "Bindings: {}\nACL: {}",
        report.bindings_dir.display(),
        report.acl.display()
    )?;
    writeln!(
        output,
        "Native-ready modules (ACL still applies): {:?}",
        report.native_ready
    )?;
    writeln!(
        output,
        "Review the ACL, then use apexe list, apexe exec, apexe env or apexe serve --acl <PATH>."
    )?;
    Ok(())
}

fn write_rules_summary(
    output: &mut impl std::io::Write,
    report: &InitReport,
) -> anyhow::Result<()> {
    writeln!(
        output,
        "Initialized {} binding(s); preserved {} existing binding(s).",
        report.written.len(),
        report.preserved.len()
    )?;
    for corpus in &report.corpora {
        writeln!(
            output,
            "Rules: {} @ {} ({})",
            corpus.name, corpus.commit, corpus.repository
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_ownership_survives_binding_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let mut module = ScannedModule::new(
            "cli.toy".into(),
            "Test module".into(),
            serde_json::json!({"type":"object"}),
            serde_json::json!({"type":"object"}),
            vec!["cli".into()],
            "exec:///bin/echo".into(),
        );
        module.warnings.push("Transient scan warning".into());
        stamp_module(&mut module, "toy@unknown");
        crate::output::YamlOutput::without_verification()
            .write(&[module.clone()], directory.path(), false)
            .unwrap();
        let loaded = crate::output::load_modules_from_dir(directory.path(), None).unwrap();
        assert!(
            is_owned(&loaded[0]),
            "Before: {}\nAfter: {}",
            serde_json::to_string(&module).unwrap(),
            serde_json::to_string(&loaded[0]).unwrap()
        );
    }
    #[cfg(unix)]
    #[test]
    fn test_init_ownership_survives_scanned_module_round_trip() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("toy");
        std::fs::write(&binary, "#!/bin/sh\ncase \"$1\" in --version) echo 'toy 1.0';; *) printf 'Usage: toy [OPTIONS]\\nOptions:\\n  --ready  Test operation\\n';; esac\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rules = root.path().join("rules");
        std::fs::create_dir(&rules).unwrap();
        std::fs::write(rules.join("toy.json"), r#"{"schema_version":"1.1","command":"toy","variant":"unknown","mode":"authoritative","confidence":"low","flags":[{"long":"--ready","type":"boolean","description":"Test operation"}],"annotations":{"readonly":true,"open_world":false}}"#).unwrap();
        let config = ApexeConfig {
            cache_dir: root.path().join("cache"),
            config_dir: root.path().join("config"),
            overlay_dirs: vec![rules],
            ..Default::default()
        };
        let outcome =
            ScanOrchestrator::new(config).scan(&[binary.to_string_lossy().into()], true, 2);
        let mut module = crate::adapter::CliToolConverter::new()
            .convert(&outcome.tools[0])
            .remove(0);
        stamp_module(&mut module, "toy@unknown");
        let directory = root.path().join("bindings");
        crate::output::YamlOutput::without_verification()
            .write(&[module.clone()], &directory, false)
            .unwrap();
        let loaded = crate::output::load_modules_from_dir(&directory, None).unwrap();
        assert!(
            is_owned(&loaded[0]),
            "Before: {}\nAfter: {}",
            serde_json::to_string(&module).unwrap(),
            serde_json::to_string(&loaded[0]).unwrap()
        );
    }
}
