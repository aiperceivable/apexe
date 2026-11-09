use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::ApexeConfig;

mod session;

/// Name of the stable entry point that hosts can configure as their shell.
pub(crate) const ENTRY_NAME: &str = "apexe-sh";

/// Run a shell with the apexe shim environment, or report readiness.
#[derive(Debug, clap::Args)]
#[command(
    trailing_var_arg = true,
    after_help = "Examples:\n  apexe sh --check                         Inspect the launcher and shim environment\n  apexe sh --check --format json           Machine-readable diagnostics\n  apexe sh -c 'ls -la'                     Run one command through the governed shell\n  apexe sh -- /path/to/script.sh           Run a script with the inherited shell\n\nUse APEXE_REAL_SHELL or --shell to select the real shell. apexe-sh is the\nstable entry point for hosts that only accept a shell path."
)]
pub struct ShArgs {
    /// Real shell to execute instead of apexe-sh.
    #[arg(long, value_name = "PATH")]
    pub shell: Option<PathBuf>,
    /// Print launcher diagnostics and exit without executing a shell.
    #[arg(long)]
    pub check: bool,
    /// Output format for --check.
    #[arg(long, value_enum, default_value = "text", requires = "check")]
    pub format: CheckFormat,
    /// Restrict PATH to the shim directory plus explicit allow paths.
    ///
    /// This reduces accidental discovery of ungoverned binaries. It is not a
    /// sandbox: absolute paths, builtins and interpreters can still run.
    #[arg(long)]
    pub locked: bool,
    /// Extra directory kept on PATH when --locked is used. Repeatable.
    #[arg(long, value_name = "DIR", requires = "locked")]
    pub allow_path: Vec<PathBuf>,
    /// Arguments forwarded verbatim to the real shell.
    #[arg(value_name = "ARGS", num_args = 0.., allow_hyphen_values = true)]
    pub argv: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum CheckFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellSource {
    Explicit,
    ApexeRealShell,
    EnvironmentShell,
    Fallback,
}

impl ShellSource {
    fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ApexeRealShell => "APEXE_REAL_SHELL",
            Self::EnvironmentShell => "SHELL",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct HostConfig {
    pub(crate) opencode: String,
    pub(crate) pi: String,
    pub(crate) codex: String,
    pub(crate) claude_code: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct UnverifiedModule {
    module_id: String,
    command_path: Vec<String>,
    reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ShimReport {
    pub(crate) directory: PathBuf,
    pub(crate) entry_point: PathBuf,
    pub(crate) entry_installed: bool,
    pub(crate) manifest_present: bool,
    pub(crate) manifest_version: Option<u32>,
    pub(crate) bindings_dir: Option<PathBuf>,
    pub(crate) acl: Option<PathBuf>,
    pub(crate) commands: Vec<String>,
    pub(crate) scanned_commands: Vec<String>,
    pub(crate) unverified_modules: Vec<UnverifiedModule>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalEnvReport {
    pub(crate) policy: &'static str,
    pub(crate) denylist: Vec<&'static str>,
    pub(crate) restricted_search_path: bool,
    pub(crate) allowed_paths: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CheckReport {
    pub(crate) real_shell: PathBuf,
    pub(crate) real_shell_source: ShellSource,
    pub(crate) local_env: LocalEnvReport,
    pub(crate) shims: ShimReport,
    pub(crate) warnings: Vec<String>,
    pub(crate) problems: Vec<String>,
    pub(crate) host_config: HostConfig,
}

impl ShArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<i32> {
        if self.check {
            check(
                config,
                self.shell.as_deref(),
                self.format,
                self.locked,
                &self.allow_path,
            )?;
            return Ok(0);
        }
        launch(
            config,
            self.shell.as_deref(),
            &self.argv,
            self.locked,
            &self.allow_path,
        )
    }
}

/// Whether the process was invoked through the stable `apexe-sh` entry point.
pub fn is_entry_invocation(raw: &[OsString]) -> bool {
    raw.first()
        .and_then(|value| Path::new(value).file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ENTRY_NAME || name == "apexe-sh.exe")
}

/// Execute `apexe-sh` itself: all arguments belong to the real shell.
pub fn run_entry(raw: &[OsString]) -> anyhow::Result<i32> {
    let config = crate::config::load_config(None)?;
    launch(&config, None, &raw[1..], false, &[])
}

fn launch(
    config: &ApexeConfig,
    explicit_shell: Option<&Path>,
    argv: &[OsString],
    locked: bool,
    allow_path: &[PathBuf],
) -> anyhow::Result<i32> {
    if std::env::var_os(session::DAEMON_ENV).is_some() {
        let socket = std::env::var_os(session::SOCKET_ENV).ok_or_else(|| {
            anyhow::anyhow!("{} is required for a session daemon", session::SOCKET_ENV)
        })?;
        return session::run_daemon(Path::new(&socket));
    }
    if let Some(socket) = std::env::var_os(session::SOCKET_ENV) {
        let command = session_command(argv)?;
        return session::run_client(Path::new(&socket), &command);
    }
    let shim_dir = shim_directory(config);
    let current = std::env::current_exe()?;
    let (shell, _) = resolve_real_shell(explicit_shell, &current)?;
    let (locked, allow_path) = restricted_options(locked, allow_path)?;
    if locked {
        super::shim::read_manifest(&shim_dir).map_err(|error| {
            anyhow::anyhow!(
                "locked mode requires installed shims in {}: {error}; run 'apexe env --enable'",
                shim_dir.display()
            )
        })?;
    }
    let environment = governed_environment(&shim_dir, &shell, locked, &allow_path)?;
    exec_shell(&shell, argv, &environment)
}

fn restricted_options(
    locked: bool,
    allow_path: &[PathBuf],
) -> anyhow::Result<(bool, Vec<PathBuf>)> {
    let environment_locked = std::env::var("APEXE_SHELL_LOCKED")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true"));
    let mut paths: Vec<PathBuf> = allow_path
        .iter()
        .map(std::path::absolute)
        .collect::<std::io::Result<_>>()?;
    if let Some(value) = std::env::var_os("APEXE_SHELL_ALLOW_PATH") {
        paths.extend(std::env::split_paths(&value));
    }
    Ok((locked || environment_locked, paths))
}

fn session_command(argv: &[OsString]) -> anyhow::Result<String> {
    let mut arguments = argv.iter();
    while let Some(argument) = arguments.next() {
        if argument == "-c" {
            return arguments
                .next()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("-c requires a UTF-8 command"));
        }
    }
    anyhow::bail!("session client requires -c <command>")
}

fn shim_directory(config: &ApexeConfig) -> PathBuf {
    std::env::var_os("APEXE_SHIM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("bin"))
}

fn resolve_real_shell(
    explicit: Option<&Path>,
    current_exe: &Path,
) -> anyhow::Result<(PathBuf, ShellSource)> {
    if let Some(path) = explicit {
        return validate_shell(path, ShellSource::Explicit, current_exe);
    }
    if let Some(value) = std::env::var_os("APEXE_REAL_SHELL") {
        return validate_shell(Path::new(&value), ShellSource::ApexeRealShell, current_exe);
    }
    if let Some(value) = std::env::var_os("SHELL") {
        let path = Path::new(&value);
        if !is_same_executable(path, current_exe) {
            return validate_shell(path, ShellSource::EnvironmentShell, current_exe);
        }
    }
    for candidate in ["/bin/bash", "/bin/sh"] {
        if Path::new(candidate).is_file() {
            return Ok((PathBuf::from(candidate), ShellSource::Fallback));
        }
    }
    anyhow::bail!(
        "no usable shell found; set APEXE_REAL_SHELL or pass --shell <PATH> \
         instead of pointing SHELL at {ENTRY_NAME}"
    )
}

fn validate_shell(
    shell: &Path,
    source: ShellSource,
    current_exe: &Path,
) -> anyhow::Result<(PathBuf, ShellSource)> {
    let resolved = if shell.is_absolute() {
        shell.to_path_buf()
    } else {
        which::which(shell)?
    };
    anyhow::ensure!(
        !is_same_executable(&resolved, current_exe),
        "refusing to execute {ENTRY_NAME} as its own real shell; set APEXE_REAL_SHELL or --shell"
    );
    anyhow::ensure!(
        resolved.is_file(),
        "real shell {} is not a file",
        resolved.display()
    );
    Ok((resolved, source))
}

fn is_same_executable(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn governed_environment(
    shim_dir: &Path,
    real_shell: &Path,
    locked: bool,
    allow_path: &[PathBuf],
) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let mut directories = vec![shim_dir.to_path_buf()];
    if locked {
        directories.extend(allow_path.iter().cloned());
    } else {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        directories.extend(std::env::split_paths(&crate::runtime_path::clean_path(
            &inherited,
        )));
    }
    let path = std::env::join_paths(directories)?;
    let depth = std::env::var("APEXE_SHELL_DEPTH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    Ok(vec![
        (OsString::from("PATH"), path),
        (
            OsString::from("APEXE_SHIM_DIR"),
            shim_dir.as_os_str().into(),
        ),
        (
            OsString::from("APEXE_REAL_SHELL"),
            real_shell.as_os_str().into(),
        ),
        (
            OsString::from("APEXE_SHELL_DEPTH"),
            OsString::from((depth + 1).to_string()),
        ),
    ])
}

#[cfg(unix)]
fn exec_shell(
    shell: &Path,
    argv: &[OsString],
    environment: &[(OsString, OsString)],
) -> anyhow::Result<i32> {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(shell);
    command.args(argv);
    for (key, value) in environment {
        command.env(key, value);
    }
    let error = command.exec();
    Err(anyhow::anyhow!(
        "failed to execute real shell {}: {error}",
        shell.display()
    ))
}

#[cfg(not(unix))]
fn exec_shell(
    _shell: &Path,
    _argv: &[OsString],
    _environment: &[(OsString, OsString)],
) -> anyhow::Result<i32> {
    anyhow::bail!("apexe sh is only supported on Unix")
}

fn check(
    config: &ApexeConfig,
    explicit_shell: Option<&Path>,
    format: CheckFormat,
    locked: bool,
    allow_path: &[PathBuf],
) -> anyhow::Result<()> {
    let report = check_report(config, explicit_shell, locked, allow_path)?;
    match format {
        CheckFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        CheckFormat::Text => print_text(&report),
    }
    if !report.problems.is_empty() {
        anyhow::bail!(
            "agent shell is not ready: {} problem(s) reported",
            report.problems.len()
        );
    }
    Ok(())
}

pub(crate) fn check_report(
    config: &ApexeConfig,
    explicit_shell: Option<&Path>,
    locked: bool,
    allow_path: &[PathBuf],
) -> anyhow::Result<CheckReport> {
    let current = std::env::current_exe()?;
    let (real_shell, source) = resolve_real_shell(explicit_shell, &current)?;
    let directory = shim_directory(config);
    let entry_point = directory.join(ENTRY_NAME);
    let mut warnings = Vec::new();
    let mut problems = Vec::new();

    let entry_installed = is_owned_link(&entry_point, &current);
    if !entry_installed {
        problems.push(format!(
            "{} is missing or is not owned by this apexe; run 'apexe env --enable'",
            entry_point.display()
        ));
    }

    let manifest = match super::shim::read_manifest(&directory) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            problems.push(format!(
                "{} is not usable: {error}; run 'apexe env --enable'",
                directory.join(crate::runtime_path::MANIFEST_NAME).display()
            ));
            None
        }
    };

    let unverified_modules = manifest
        .as_ref()
        .map(unverified_modules)
        .unwrap_or_default();
    let scanned_commands = manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .commands
                .iter()
                .filter(|(_, routes)| {
                    routes.iter().any(|route| {
                        matches!(
                            route.tier,
                            super::shim::RouteTier::Scanned | super::shim::RouteTier::Legacy
                        )
                    })
                })
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default();
    let (manifest_present, manifest_version, bindings_dir, acl, commands) = match &manifest {
        Some(manifest) => {
            let acl = super::local::acl_path(manifest.acl.as_deref(), config)?;
            if acl.is_none() {
                warnings.push(
                    "no ACL loaded; access control is inactive for shim-routed commands".into(),
                );
            }
            (
                true,
                Some(manifest.version),
                Some(manifest.bindings_dir.clone()),
                acl,
                manifest.commands.keys().cloned().collect::<Vec<_>>(),
            )
        }
        None => (false, None, None, None, Vec::new()),
    };

    if source == ShellSource::Fallback {
        warnings.push(format!(
            "SHELL points at {ENTRY_NAME}; using {} instead. Set APEXE_REAL_SHELL to choose explicitly.",
            real_shell.display()
        ));
    }
    warnings.push(
        "login shells (-l) may reset PATH after profile startup; use the managed block from 'apexe env --enable'"
            .into(),
    );

    let (locked, allow_path) = restricted_options(locked, allow_path)?;
    let host_config = host_config(&entry_point, &directory);
    let report = CheckReport {
        real_shell,
        real_shell_source: source,
        local_env: LocalEnvReport {
            policy: "inherit",
            denylist: crate::module::executor::LOCAL_ENV_DENYLIST.to_vec(),
            restricted_search_path: locked,
            allowed_paths: allow_path,
        },
        shims: ShimReport {
            directory: directory.clone(),
            entry_point: entry_point.clone(),
            entry_installed,
            manifest_present,
            manifest_version,
            bindings_dir,
            acl,
            commands,
            scanned_commands,
            unverified_modules,
        },
        warnings,
        problems,
        host_config,
    };

    Ok(report)
}

fn unverified_modules(manifest: &super::shim::Manifest) -> Vec<UnverifiedModule> {
    let Ok(modules) = crate::output::load_modules_from_dir(
        &manifest.bindings_dir,
        manifest.bindings_pattern.as_deref(),
    ) else {
        return Vec::new();
    };
    let verified: BTreeSet<&str> = manifest
        .commands
        .values()
        .flatten()
        .map(|route| route.module_id.as_str())
        .collect();
    modules
        .iter()
        .filter(|module| {
            module.target.starts_with("exec://") && !verified.contains(module.module_id.as_str())
        })
        .map(|module| UnverifiedModule {
            module_id: module.module_id.clone(),
            command_path: module
                .metadata
                .get("command_path")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            reason: super::shim::eligible(module)
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "not registered in the shim manifest".into()),
        })
        .collect()
}

fn is_owned_link(entry_point: &Path, executable: &Path) -> bool {
    #[cfg(unix)]
    {
        std::fs::read_link(entry_point)
            .ok()
            .is_some_and(|target| is_same_executable(&target, executable))
    }
    #[cfg(not(unix))]
    {
        std::fs::read(entry_point)
            .ok()
            .zip(std::fs::read(executable).ok())
            .is_some_and(|(left, right)| left == right)
    }
}

fn host_config(entry_point: &Path, shim_dir: &Path) -> HostConfig {
    let entry = entry_point.display().to_string();
    let opencode = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "shell": entry,
    });
    let pi = serde_json::json!({ "shellPath": entry });
    let directory = shim_dir.to_string_lossy();
    let directory = shell_words::quote(&directory);
    let codex = format!(
        "Codex CLI uses its recognized shell; SHELL={ENTRY_NAME} is not sufficient.\n\
         For zsh: apexe env --reload --shell zsh --shim-dir {directory}\n\
         For bash: apexe env --reload --shell bash --shim-dir {directory}\n\
         Choose the shell Codex actually uses. Restart Codex to refresh its shell snapshot."
    );
    let claude_code = format!(
        "Claude Code (verified with 2.1.318) rejects SHELL={ENTRY_NAME} and CLAUDE_CODE_SHELL={ENTRY_NAME}, \
         falling back to /bin/zsh.\n\
         Start Claude Code where PATH already contains the shim directory:\n\
         PATH={directory}:\"$PATH\" claude\n\
         Activating shims in the launching terminal first has the same effect.\n\
         Its shell snapshot copies the launch environment's PATH, so startup-file edits alone are not enough."
    );
    HostConfig {
        opencode: serde_json::to_string_pretty(&opencode).unwrap_or_default(),
        pi: serde_json::to_string_pretty(&pi).unwrap_or_default(),
        codex,
        claude_code,
    }
}

fn print_text(report: &CheckReport) {
    println!(
        "[PASS] SHELL {} ({})",
        report.real_shell.display(),
        report.real_shell_source.label()
    );
    if report.shims.entry_installed {
        println!("[PASS] ENTRY {}", report.shims.entry_point.display());
    } else {
        println!("[FAIL] ENTRY {}", report.shims.entry_point.display());
    }
    if report.shims.manifest_present {
        let version = report
            .shims
            .manifest_version
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".into());
        println!(
            "[PASS] MANIFEST {} (version {version}, {} command(s))",
            report.shims.directory.display(),
            report.shims.commands.len()
        );
        println!("[INFO] SHIMS {}", report.shims.commands.join(", "));
        if !report.shims.scanned_commands.is_empty() {
            println!(
                "[WARN] SCANNED {} route(s) without a reviewed overlay: {}; they run the original argv after strict schema validation",
                report.shims.scanned_commands.len(),
                report.shims.scanned_commands.join(", ")
            );
        }
        if !report.shims.unverified_modules.is_empty() {
            println!(
                "[WARN] COVERAGE {} module(s) are not shim-eligible: {}",
                report.shims.unverified_modules.len(),
                report
                    .shims
                    .unverified_modules
                    .iter()
                    .map(|module| module.module_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    } else {
        println!("[FAIL] MANIFEST {}", report.shims.directory.display());
    }
    match &report.shims.acl {
        Some(path) => println!("[PASS] ACL {}", path.display()),
        None => println!("[WARN] ACL no ACL loaded; access control is inactive"),
    }
    println!(
        "[INFO] ENV {} (denylist: {})",
        report.local_env.policy,
        report.local_env.denylist.join(", ")
    );
    if report.local_env.restricted_search_path {
        println!(
            "[INFO] PATH restricted to shims plus: {}",
            if report.local_env.allowed_paths.is_empty() {
                "<none>".into()
            } else {
                report
                    .local_env
                    .allowed_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    }
    for warning in &report.warnings {
        println!("[WARN] {warning}");
    }
    for problem in &report.problems {
        println!("[FAIL] {problem}");
    }
    println!(
        "Host config (opencode.json):\n{}",
        report.host_config.opencode
    );
    println!("Host config (pi settings.json):\n{}", report.host_config.pi);
    println!("Host setup (Codex CLI):\n{}", report.host_config.codex);
    println!(
        "Host setup (Claude Code):\n{}",
        report.host_config.claude_code
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_entry_invocation_matches_stable_name() {
        for name in ["apexe-sh", "./apexe-sh", "/tmp/bin/apexe-sh"] {
            assert!(is_entry_invocation(&[OsString::from(name)]));
        }
        for name in ["apexe", "apexe.exe", "ls", "apexe-sh-wrapper"] {
            assert!(!is_entry_invocation(&[OsString::from(name)]));
        }
    }

    #[test]
    fn test_resolve_real_shell_rejects_self_reference() {
        let current = std::env::current_exe().unwrap();
        let error = resolve_real_shell(Some(&current), &current).unwrap_err();
        assert!(error.to_string().contains("refusing to execute"));
    }
}
