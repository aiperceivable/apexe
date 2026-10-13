use super::{entry_path, owned};
use crate::cli::shim::{Manifest, Route};
use crate::config::ApexeConfig;
use apcore_toolkit::ScannedModule;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Warn,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Warn => "WARN",
            Self::Skip => "SKIP",
        }
    }
}

struct Finding {
    status: Status,
    label: String,
    detail: String,
}

#[derive(Default)]
struct Report {
    findings: Vec<Finding>,
}

#[derive(Debug, thiserror::Error)]
#[error("Shim environment check failed: {failures} failed check(s)")]
struct CheckFailed {
    failures: usize,
}

impl Report {
    fn add(&mut self, status: Status, label: &str, detail: String) {
        self.findings.push(Finding {
            status,
            label: label.into(),
            detail,
        });
    }

    fn record<T>(
        &mut self,
        label: &str,
        result: anyhow::Result<T>,
        describe: impl FnOnce(&T) -> String,
    ) -> Option<T> {
        match result {
            Ok(value) => {
                self.add(Status::Pass, label, describe(&value));
                Some(value)
            }
            Err(error) => {
                self.add(Status::Fail, label, format!("{error:#}"));
                None
            }
        }
    }

    fn write(&self, output: &mut impl std::io::Write) -> anyhow::Result<()> {
        for finding in &self.findings {
            // Keep paths and parser errors on one line, including control characters.
            let detail: String = finding.detail.escape_debug().collect();
            let label: String = finding.label.escape_debug().collect();
            writeln!(output, "[{}] {label}: {detail}", finding.status.label())?;
        }
        let count = |status| {
            self.findings
                .iter()
                .filter(|item| item.status == status)
                .count()
        };
        writeln!(
            output,
            "Summary: {} passed, {} failed, {} warnings, {} skipped",
            count(Status::Pass),
            count(Status::Fail),
            count(Status::Warn),
            count(Status::Skip)
        )?;
        writeln!(output, "Scope: checks the process PATH; shell aliases/functions and command hashes cannot be inspected. No wrapped commands are executed; ACL loading does not prove a specific call is allowed.")?;
        if count(Status::Fail) > 0 {
            return Err(CheckFailed {
                failures: count(Status::Fail),
            }
            .into());
        }
        Ok(())
    }
}

pub(super) fn execute(directory: &Path, config: &ApexeConfig) -> anyhow::Result<()> {
    let mut report = Report::default();
    check_path(directory, &mut report);
    let manifest = report.record("MANIFEST", load_manifest(directory), |_| {
        format!(
            "{} is valid",
            directory.join(crate::runtime_path::MANIFEST_NAME).display()
        )
    });
    if let Some(manifest) = manifest {
        check_links(directory, &manifest, &mut report);
        check_activation(directory, &manifest, &mut report);
        check_persistent_scripts(directory, &manifest, &mut report);
        let registered = check_bindings(&manifest, config, &mut report);
        check_acl(&manifest, config, registered.as_deref(), &mut report);
    } else {
        for label in ["LINKS", "BINDINGS", "ACL"] {
            report.add(
                Status::Skip,
                label,
                "Requires a valid manifest; repair or reinstall with apexe env --enable".into(),
            );
        }
    }
    report.write(&mut std::io::stdout().lock())
}

fn same_directory(left: &Path, right: &Path) -> bool {
    std::path::absolute(left)
        .ok()
        .zip(std::path::absolute(right).ok())
        .is_some_and(|(left, right)| left == right)
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn check_path(directory: &Path, report: &mut Report) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let copies = std::env::split_paths(&path)
        .filter(|path| same_directory(path, directory))
        .count();
    report.record("PATH", (|| {
        anyhow::ensure!(copies > 0, "{} is absent from PATH; open a new terminal or evaluate the output of apexe env --shim-dir <DIR>", directory.display());
        Ok(())
    })(), |_| format!("{} is present", directory.display()));
    if copies > 1 {
        report.add(
            Status::Warn,
            "PATH",
            format!(
                "Shim directory occurs {copies} times; reactivate with apexe env --shim-dir <DIR>"
            ),
        );
    }
    if let Some(active) = std::env::var_os("APEXE_SHIM_DIR") {
        report.record(
            "PATH APEXE_SHIM_DIR",
            (|| {
                anyhow::ensure!(
                    same_directory(Path::new(&active), directory),
                    "APEXE_SHIM_DIR={} differs from {}; activate the intended directory",
                    Path::new(&active).display(),
                    directory.display()
                );
                Ok(())
            })(),
            |_| "Matches the checked directory".into(),
        );
    }
}

fn load_manifest(directory: &Path) -> anyhow::Result<Manifest> {
    let manifest = crate::cli::shim::read_manifest(directory)?;
    for (label, path) in [
        ("executable", &manifest.executable),
        ("bindings_dir", &manifest.bindings_dir),
    ] {
        anyhow::ensure!(
            path.is_absolute(),
            "Manifest {label} must be absolute; reinstall shims"
        );
    }
    if let Some(acl) = &manifest.acl {
        anyhow::ensure!(
            acl.is_absolute(),
            "Manifest ACL must be absolute; reinstall shims"
        );
    }
    if let Some(source) = &manifest.source_scripts_dir {
        anyhow::ensure!(
            source.is_absolute(),
            "Manifest source script directory must be absolute; reload shims"
        );
    }
    validate_routes(&manifest)?;
    Ok(manifest)
}

fn validate_routes(manifest: &Manifest) -> anyhow::Result<()> {
    anyhow::ensure!(
        !manifest.commands.is_empty(),
        "Manifest contains no commands; rescan and reload shims"
    );
    for (name, routes) in &manifest.commands {
        anyhow::ensure!(
            crate::cli::shim::valid_name(name) && !super::reserved_shim_name(name),
            "Invalid command name in manifest; reinstall shims"
        );
        anyhow::ensure!(
            !routes.is_empty(),
            "Manifest command {name} has no routes; reload shims"
        );
        let mut paths = BTreeSet::new();
        for route in routes {
            anyhow::ensure!(
                route.command_path.first() == Some(name)
                    && route.command_path.iter().all(|part| !part.is_empty())
                    && !route.module_id.is_empty(),
                "Invalid route for {name}; rescan and reload shims"
            );
            anyhow::ensure!(
                paths.insert(&route.command_path),
                "Duplicate route for {name}; reload shims"
            );
        }
    }
    Ok(())
}

fn require_executable(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.is_absolute(),
        "Executable path must be absolute: {}",
        path.display()
    );
    which::which(path).map_err(|error| {
        anyhow::anyhow!("{} is missing or not executable: {error}", path.display())
    })?;
    Ok(())
}

fn check_links(directory: &Path, manifest: &Manifest, report: &mut Report) {
    report.record(
        "LINKS executable",
        require_executable(&manifest.executable),
        |_| manifest.executable.display().to_string(),
    );
    report.record(
        "LINKS running executable",
        (|| {
            let running = std::env::current_exe()?.canonicalize()?;
            anyhow::ensure!(running == manifest.executable.canonicalize()?,
                "Running apexe {} differs from the installed executable {}; run the installed apexe or reload shims with this installation",
                running.display(), manifest.executable.display());
            Ok(())
        })(),
        |_| "Matches the installed apexe".into(),
    );
    for name in manifest.commands.keys() {
        let entry = entry_path(directory, name);
        report.record(
            &format!("LINKS {name}"),
            (|| {
                anyhow::ensure!(
                    owned(&entry, manifest),
                    "{} is missing or not an owned link to {}; reinstall shims",
                    entry.display(),
                    manifest.executable.display()
                );
                require_executable(&entry)
            })(),
            |_| format!("{} -> {}", entry.display(), manifest.executable.display()),
        );
        report.record(
            &format!("PATH {name}"),
            resolve_entry(directory, name),
            |path| path.display().to_string(),
        );
    }
}

fn resolve_entry(directory: &Path, name: &str) -> anyhow::Result<PathBuf> {
    // Inspect the caller's PATH, not runtime_path::search_path(), which strips shims.
    let resolved = which::which_in(name, std::env::var_os("PATH"), std::env::current_dir()?)?;
    anyhow::ensure!(
        resolved
            .parent()
            .is_some_and(|parent| same_directory(parent, directory)),
        "{name} resolves to {} instead of {}; activate shims before other command directories",
        resolved.display(),
        entry_path(directory, name).display()
    );
    Ok(resolved)
}

fn check_activation(directory: &Path, manifest: &Manifest, report: &mut Report) {
    use crate::cli::activation;
    for (name, script, generated) in [
        (
            activation::NAME,
            manifest.activation_script.as_deref(),
            activation::generate(directory, &manifest.executable),
        ),
        (
            activation::DEACTIVATE_NAME,
            manifest.deactivation_script.as_deref(),
            activation::generate_deactivation(directory, &manifest.executable),
        ),
    ] {
        let label = format!("LINKS activation {name}");
        let Some(script) = script else {
            report.add(Status::Warn, &label, format!("Legacy installation has no {name} script; run apexe env --reload to generate it"));
            continue;
        };
        report.record(
            &label,
            (|| {
                anyhow::ensure!(
                    script == generated?,
                    "Shell script targets a different installation; reload shims"
                );
                activation::require_owned(directory, name, script)
            })(),
            |_| format!("Source {} in sh/bash/zsh", directory.join(name).display()),
        );
    }
}

fn check_persistent_scripts(directory: &Path, manifest: &Manifest, report: &mut Report) {
    if let Some(source) = &manifest.source_scripts_dir {
        report.record(
            "LINKS persistent source scripts",
            crate::cli::source_scripts::check(source, directory, &manifest.executable),
            |_| {
                format!(
                    "Owned apexe scripts in {} remain available after disable",
                    source.display()
                )
            },
        );
    } else {
        report.add(
            Status::Warn,
            "LINKS persistent source scripts",
            "Legacy installation has no persistent source helpers; run apexe env --reload".into(),
        );
    }
}

fn check_bindings(
    manifest: &Manifest,
    config: &ApexeConfig,
    report: &mut Report,
) -> Option<Vec<String>> {
    let modules = report.record(
        "BINDINGS",
        crate::output::load_modules_from_dir(
            &manifest.bindings_dir,
            manifest.bindings_pattern.as_deref(),
        )
        .map_err(Into::into),
        |modules| {
            format!(
                "Loaded {} modules from {}",
                modules.len(),
                manifest.bindings_dir.display()
            )
        },
    )?;
    let executor = report.record(
        "BINDINGS registry",
        build_registry(manifest, config, modules.clone()),
        |executor| format!("{} modules registered", executor.registry().count()),
    )?;
    for (name, routes) in &manifest.commands {
        for route in routes {
            report.record(
                &format!("BINDINGS {}", route.module_id),
                check_route(
                    name,
                    route,
                    manifest,
                    &modules,
                    executor.registry().has(&route.module_id),
                ),
                |_| "Route, native executable and overlay contract are current".into(),
            );
        }
    }
    Some(executor.registry().module_ids_full(true))
}

fn build_registry(
    manifest: &Manifest,
    config: &ApexeConfig,
    modules: Vec<ScannedModule>,
) -> anyhow::Result<std::sync::Arc<apcore::Executor>> {
    let mut options = crate::cli::local::options(
        config,
        &manifest.bindings_dir,
        manifest.bindings_pattern.as_deref(),
        None,
        manifest.filter.clone(),
    );
    options.audit_path = None;
    options.enable_logging = false;
    Ok(
        crate::module::registry::build_executor_from_modules_with_io(
            &options,
            modules,
            crate::module::ExecutionIo::Passthrough {
                stdin: crate::module::StdinMode::Null,
            },
        )?,
    )
}

fn check_route(
    name: &str,
    route: &Route,
    manifest: &Manifest,
    modules: &[ScannedModule],
    registered: bool,
) -> anyhow::Result<()> {
    let matching: Vec<_> = modules
        .iter()
        .filter(|module| module.module_id == route.module_id)
        .collect();
    anyhow::ensure!(matching.len() == 1, "{} has {} matching bindings; rescan and reload (removed routes refuse execution without native fallback)", route.module_id, matching.len());
    let module = matching[0];
    anyhow::ensure!(
        manifest.filter.admits(module),
        "{} is excluded by the installed filter; reload shims",
        route.module_id
    );
    crate::cli::shim::eligible(module)?;
    anyhow::ensure!(
        crate::cli::shim::command_path(module)? == route.command_path,
        "{name} command_path changed; rescan and reload shims"
    );
    let binary = crate::scanner::resolver::extract_binary_path(&module.target)
        .ok_or_else(|| anyhow::anyhow!("Invalid CLI target for {}", route.module_id))?;
    require_executable(Path::new(binary))?;
    anyhow::ensure!(
        Some(Path::new(binary).canonicalize()?) != manifest.executable.canonicalize().ok(),
        "Binding targets the shim executable; rescan with a clean PATH"
    );
    anyhow::ensure!(
        registered,
        "{} failed runtime registration; repair bindings and rescan",
        route.module_id
    );
    Ok(())
}

fn check_acl(
    manifest: &Manifest,
    config: &ApexeConfig,
    registered: Option<&[String]>,
    report: &mut Report,
) {
    let acl = report.record("ACL", load_acl(manifest, config), |(path, _)| {
        format!("{} loaded; access control is active", path.display())
    });
    let Some((path, manager)) = acl else {
        return;
    };
    let Some(registered) = registered else {
        report.add(
            Status::Skip,
            "ACL targets",
            "Cannot validate target patterns until bindings load".into(),
        );
        return;
    };
    let validation =
        crate::governance::validate_acl_rules(manager.into_inner().rules(), registered);
    report.record(
        "ACL targets",
        validation
            .fatal_error(&path)
            .map_or(Ok(()), |error| Err(error.into())),
        |_| "No target naming errors".into(),
    );
    let unmatched = validation.unmatched_targets.len();
    if unmatched > 0 {
        report.add(
            Status::Warn,
            "ACL targets",
            format!("{unmatched} target pattern(s) match no registered modules; review the policy"),
        );
    }
}

fn load_acl(
    manifest: &Manifest,
    config: &ApexeConfig,
) -> anyhow::Result<(PathBuf, crate::governance::AclManager)> {
    let path = crate::cli::local::acl_path(manifest.acl.as_deref(), config)?
        .ok_or_else(|| anyhow::anyhow!("No ACL loaded; access control is inactive. Configure {} or reinstall with --acl <PATH>", config.config_dir.join("acl.yaml").display()))?;
    let manager = crate::governance::AclManager::from_config(&path)?;
    Ok((path, manager))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_report_returns_failure_and_escapes_control_characters() {
        let mut report = Report::default();
        report.add(Status::Fail, "LINKS\ncat", "bad\npath\u{1b}[31m".into());
        let mut output = Vec::new();
        assert!(report.write(&mut output).is_err());
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("[FAIL] LINKS\\ncat: bad\\npath\\u{1b}[31m"));
        assert!(text.contains("0 passed, 1 failed, 0 warnings, 0 skipped"));
    }

    #[test]
    fn test_report_warnings_and_skips_are_not_failures() {
        let mut report = Report::default();
        report.add(Status::Warn, "PATH", "Duplicate directory".into());
        report.add(Status::Skip, "ACL targets", "Unavailable bindings".into());
        let mut output = Vec::new();
        report.write(&mut output).unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("0 failed, 1 warnings, 1 skipped"));
    }

    #[test]
    fn test_same_directory_distinguishes_missing_directories() {
        let root = tempfile::TempDir::new().unwrap();
        assert!(same_directory(
            &root.path().join("a"),
            &root.path().join("a")
        ));
        assert!(!same_directory(
            &root.path().join("a"),
            &root.path().join("b")
        ));
    }
}
