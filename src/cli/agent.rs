use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::config::ApexeConfig;

use super::sh::{self, CheckFormat, CheckReport, ENTRY_NAME};

/// Print host configuration or check the installed launcher for one host.
#[derive(Debug, clap::Args)]
#[command(
    after_help = "Examples:\n  apexe agent opencode                    Print the OpenCode shell configuration\n  apexe agent --check pi                  Check pi launcher and config readiness\n  apexe agent --check claude --format json\n\nHosts that accept a custom shell use the installed apexe-sh entry. Codex and\nClaude Code cannot replace their shell; their output explains the supported\nPATH or hook integration."
)]
pub struct AgentArgs {
    /// Host to configure or check.
    #[arg(value_enum)]
    pub host: AgentHost,
    /// Check launcher and host configuration instead of printing install text.
    #[arg(long)]
    pub check: bool,
    /// Output format for --check.
    #[arg(long, value_enum, default_value = "text", requires = "check")]
    pub format: CheckFormat,
    /// Write the host configuration instead of only printing it.
    #[arg(long, conflicts_with = "check")]
    pub write: bool,
    /// Generate an MCP-only deployment instead of shell configuration.
    #[arg(long, conflicts_with = "check")]
    pub mcp_only: bool,
    /// Configuration scope for --write.
    #[arg(long, value_enum, requires = "write")]
    pub scope: Option<AgentScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AgentScope {
    Project,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHost {
    Opencode,
    Pi,
    Codex,
    Claude,
}

#[derive(Debug, Serialize)]
struct HostReport {
    host: AgentHost,
    binary: Option<PathBuf>,
    config: Option<PathBuf>,
    configured: bool,
    note: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    host: HostReport,
    launcher: CheckReport,
}

impl AgentArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        if self.check {
            return check(config, self.host, self.format);
        }
        if self.mcp_only {
            return mcp(config, self.host, self.write, self.scope);
        }
        install(config, self.host, self.write, self.scope)
    }
}

fn install(
    config: &ApexeConfig,
    host: AgentHost,
    write: bool,
    scope: Option<AgentScope>,
) -> anyhow::Result<()> {
    let report = sh::check_report(config, None, false, &[])?;
    for warning in &report.warnings {
        eprintln!("Warning: {warning}");
    }
    for problem in &report.problems {
        eprintln!("Problem: {problem}");
    }
    if write {
        if selects_shell_by_path(host) {
            // REFUSAL, not a warning: these hosts start every command through
            // the entry point, so writing `shellPath` at a launcher that is not
            // installed does not degrade to the system shell — it breaks the
            // host's own command execution, which is reported to the user as an
            // unrelated `EACCES` long after apexe ran. An operator who reinstalls
            // `apexe env --enable` afterwards gets a working host; one who instead
            // fills the gap by hand gets a file that is not the launcher and
            // silently runs commands outside apexe.
            anyhow::ensure!(
                report.problems.is_empty(),
                "refusing to write {} configuration: {} problem(s) mean apexe-sh is not \
                 installed, and this host starts every command through it. Run 'apexe env \
                 --enable', then 'apexe agent --check {}'.",
                host_name(host),
                report.problems.len(),
                host_name(host)
            );
        }
        return write_host(host, scope, &report);
    }
    match host {
        AgentHost::Opencode => println!("{}", report.host_config.opencode),
        AgentHost::Pi => println!("{}", report.host_config.pi),
        AgentHost::Codex => println!("{}", report.host_config.codex),
        AgentHost::Claude => println!("{}", report.host_config.claude_code),
    }
    Ok(())
}

/// Whether the host selects its shell by path, rather than loading shims from
/// `PATH` or a hook.
///
/// Only these hosts hand `apexe-sh` to something that spawns it directly, so
/// only these hosts are bricked by an entry point that is absent. Codex and
/// Claude Code cannot replace their shell; their configuration names a shim
/// directory or a hook command and keeps working through the system shell.
fn selects_shell_by_path(host: AgentHost) -> bool {
    matches!(host, AgentHost::Opencode | AgentHost::Pi)
}

fn mcp(
    config: &ApexeConfig,
    host: AgentHost,
    write: bool,
    scope: Option<AgentScope>,
) -> anyhow::Result<()> {
    let report = sh::check_report(config, None, false, &[])?;
    let executable = std::env::current_exe()?.canonicalize()?;
    let mut arguments = vec!["serve".to_string()];
    if let Some(acl) = &report.shims.acl {
        arguments.push("--acl".into());
        arguments.push(acl.display().to_string());
    } else {
        eprintln!("Warning: no ACL loaded; the MCP server will not enforce access control");
    }
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("apexe executable path must be valid UTF-8"))?;

    match host {
        AgentHost::Opencode => {
            let mut command = vec![json!(executable)];
            command.extend(arguments.iter().map(|argument| json!(argument)));
            let snippet = json!({
                "mcp": {
                    "apexe": {
                        "type": "local",
                        "command": command,
                        "enabled": true
                    }
                },
                "tools": {"bash": false}
            });
            if write {
                let path = host_config_path(host, scope)?;
                write_json(&path, |object| {
                    object.insert("mcp".into(), snippet["mcp"].clone());
                    let tools = object
                        .entry("tools")
                        .or_insert_with(|| json!({}))
                        .as_object_mut();
                    if let Some(tools) = tools {
                        tools.insert("bash".into(), json!(false));
                    }
                })?;
                println!("Wrote {}", path.display());
            } else {
                println!("{}", serde_json::to_string_pretty(&snippet)?);
            }
        }
        AgentHost::Claude => {
            let mcp = json!({
                "mcpServers": {
                    "apexe": {
                        "command": executable,
                        "args": arguments
                    }
                }
            });
            if write {
                let path = host_config_path(host, scope)?;
                let selected_scope = scope.unwrap_or(AgentScope::Project);
                if selected_scope == AgentScope::Project {
                    let mcp_path = std::env::current_dir()?.join(".mcp.json");
                    write_json(&mcp_path, |object| {
                        object.insert("mcpServers".into(), mcp["mcpServers"].clone());
                    })?;
                    println!("Wrote {}", mcp_path.display());
                } else {
                    println!(
                        "Run `claude mcp add --scope user apexe -- {} {}` to register the user-scope server.",
                        executable,
                        arguments.join(" ")
                    );
                }
                write_json(&path, |object| ensure_permission_deny(object, "Bash"))?;
                println!("Wrote {}", path.display());
            } else {
                println!("{}", serde_json::to_string_pretty(&mcp)?);
                println!("Also add `Bash` to permissions.deny in .claude/settings.json.");
            }
        }
        AgentHost::Codex => {
            let quoted = shell_words::quote(executable);
            let args = arguments
                .iter()
                .map(|argument| format!("{:?}", argument))
                .collect::<Vec<_>>()
                .join(", ");
            println!("[mcp_servers.apexe]\ncommand = {quoted}\nargs = [{args}]");
            println!("Codex keeps its shell; combine this with a sandbox or accept that MCP-only is not strict.");
            anyhow::ensure!(
                !write,
                "Codex MCP configuration is printed; merge it into config.toml manually"
            );
        }
        AgentHost::Pi => {
            println!("pi has no stable MCP-only profile in the tested version; use `apexe agent pi` for shellPath integration.");
            anyhow::ensure!(!write, "pi MCP configuration is not generated");
        }
    }
    Ok(())
}

fn ensure_permission_deny(object: &mut Map<String, Value>, rule: &str) {
    let permissions = object
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(permissions) = permissions else {
        return;
    };
    let deny = permissions
        .entry("deny")
        .or_insert_with(|| json!([]))
        .as_array_mut();
    let Some(deny) = deny else {
        return;
    };
    if !deny.iter().any(|value| value == rule) {
        deny.push(json!(rule));
    }
}

fn write_host(
    host: AgentHost,
    scope: Option<AgentScope>,
    report: &CheckReport,
) -> anyhow::Result<()> {
    let entry = &report.shims.entry_point;
    match host {
        AgentHost::Opencode => {
            let path = host_config_path(host, scope)?;
            write_json(&path, |object| {
                object.insert("shell".into(), json!(entry));
            })?;
            println!("Wrote {}", path.display());
        }
        AgentHost::Pi => {
            if scope == Some(AgentScope::Project) {
                anyhow::bail!("pi shellPath is user-level; use --scope user");
            }
            let path = host_config_path(host, Some(AgentScope::User))?;
            write_json(&path, |object| {
                object.insert("shellPath".into(), json!(entry));
            })?;
            println!("Wrote {}", path.display());
        }
        AgentHost::Codex => {
            let status = std::process::Command::new(std::env::current_exe()?)
                .args(["env", "--reload", "--shell", "zsh"])
                .status()?;
            anyhow::ensure!(status.success(), "apexe env --reload --shell zsh failed");
            println!("Reloaded zsh startup configuration for Codex");
        }
        AgentHost::Claude => {
            let path = host_config_path(host, scope)?;
            let executable = std::env::current_exe()?.canonicalize()?;
            write_json(&path, |object| ensure_claude_hook(object, &executable))?;
            println!("Wrote {}", path.display());
        }
    }
    Ok(())
}

fn host_config_path(host: AgentHost, scope: Option<AgentScope>) -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot locate home directory"))?;
    let cwd = std::env::current_dir()?;
    let scope = scope.unwrap_or(match host {
        AgentHost::Pi => AgentScope::User,
        _ => AgentScope::Project,
    });
    let path = match (host, scope) {
        (AgentHost::Opencode, AgentScope::Project) => cwd.join("opencode.json"),
        (AgentHost::Opencode, AgentScope::User) => home.join(".config/opencode/opencode.json"),
        (AgentHost::Pi, AgentScope::User) => home.join(".pi/agent/settings.json"),
        (AgentHost::Pi, AgentScope::Project) => {
            anyhow::bail!("pi shellPath is user-level; use --scope user")
        }
        (AgentHost::Claude, AgentScope::Project) => cwd.join(".claude/settings.json"),
        (AgentHost::Claude, AgentScope::User) => home.join(".claude/settings.json"),
        (AgentHost::Codex, _) => anyhow::bail!("Codex configuration is written by apexe env"),
    };
    if host == AgentHost::Opencode && !path.exists() {
        let jsonc = path.with_extension("jsonc");
        anyhow::ensure!(
            !jsonc.exists(),
            "{} exists; edit it manually or remove it before --write",
            jsonc.display()
        );
    }
    Ok(path)
}

fn write_json(path: &Path, mutate: impl FnOnce(&mut Map<String, Value>)) -> anyhow::Result<()> {
    let existing_permissions = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut root = if path.exists() {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str::<Value>(&text)
            .map_err(|error| anyhow::anyhow!("{} is not valid JSON: {error}", path.display()))?
    } else {
        Value::Object(Map::new())
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} must contain a JSON object", path.display()))?;
    mutate(object);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("apexe-tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&root)?)?;
    if let Some(permissions) = existing_permissions {
        std::fs::set_permissions(&temporary, permissions)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn ensure_claude_hook(object: &mut Map<String, Value>, executable: &Path) {
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(hooks) = hooks else {
        return;
    };
    let entries = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut();
    let Some(entries) = entries else {
        return;
    };
    let command = format!(
        "{} hook claude",
        shell_words::quote(executable.to_str().unwrap_or("apexe"))
    );
    if entries.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value.contains("apexe hook claude"))
                })
            })
    }) {
        return;
    }
    entries.push(json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": 30
        }]
    }));
}

fn check(config: &ApexeConfig, host: AgentHost, format: CheckFormat) -> anyhow::Result<()> {
    let launcher = sh::check_report(config, None, false, &[])?;
    let host_report = inspect_host(host, &launcher);
    let report = DoctorReport {
        host: host_report,
        launcher,
    };

    match format {
        CheckFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        CheckFormat::Text => print_text(&report),
    }

    if !report.launcher.problems.is_empty() {
        anyhow::bail!(
            "agent shell is not ready: {} problem(s) reported",
            report.launcher.problems.len()
        );
    }
    Ok(())
}

fn inspect_host(host: AgentHost, launcher: &CheckReport) -> HostReport {
    let binary = which::which(binary_name(host)).ok();
    let mut first_existing = None;
    for path in config_candidates(host) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if first_existing.is_none() {
            first_existing = Some(path.clone());
        }
        if host_text_configured(host, &text, &launcher.shims.directory) {
            return HostReport {
                host,
                binary,
                config: Some(path),
                configured: true,
                note: configured_note(host),
            };
        }
    }

    if host == AgentHost::Claude && path_contains_shims(&launcher.shims.directory) {
        return HostReport {
            host,
            binary,
            config: first_existing,
            configured: true,
            note: "Claude Code has no apexe hook, but this process PATH contains the shim directory; Bash calls that resolve through PATH are governed".into(),
        };
    }

    HostReport {
        host,
        binary,
        config: first_existing,
        configured: false,
        note: "no apexe host configuration found; run `apexe agent <host>` for the supported setup"
            .into(),
    }
}

fn config_candidates(host: AgentHost) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    match host {
        AgentHost::Opencode => vec![
            cwd.join("opencode.json"),
            home.join(".config/opencode/opencode.json"),
        ],
        AgentHost::Pi => vec![home.join(".pi/agent/settings.json")],
        AgentHost::Codex => vec![
            home.join(".zprofile"),
            home.join(".zshrc"),
            home.join(".bash_profile"),
            home.join(".profile"),
        ],
        AgentHost::Claude => vec![
            cwd.join(".claude/settings.json"),
            home.join(".claude/settings.json"),
        ],
    }
}

fn host_text_configured(host: AgentHost, text: &str, shim_dir: &Path) -> bool {
    match host {
        AgentHost::Opencode => text.contains("apexe-sh"),
        AgentHost::Pi => text.contains("shellPath") && text.contains("apexe-sh"),
        AgentHost::Codex => {
            text.contains("env --shim-dir") || text.contains(shim_dir.to_string_lossy().as_ref())
        }
        AgentHost::Claude => text.contains("apexe hook claude"),
    }
}

/// Report a host that disabling the shim directory leaves unable to start a
/// command.
///
/// `env --disable` removes the `apexe-sh` entry point, and a host configured to
/// use it as its shell does not fall back to the system shell: it loses the
/// ability to run anything, and the failure surfaces as an `EACCES` from the
/// host rather than as anything naming apexe. Disabling stays possible — an
/// uninstall has to be — so this states the consequence instead of refusing.
pub(crate) fn warn_if_host_selects_removed_launcher(shim_dir: &Path) {
    const PATH_SELECTING: [AgentHost; 2] = [AgentHost::Opencode, AgentHost::Pi];
    for host in PATH_SELECTING {
        for path in config_candidates(host) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if host_text_configured(host, &text, shim_dir) {
                eprintln!(
                    "Warning: {} still selects {} as its shell; disabling it broke that host. \
                     Run 'apexe env --enable' to restore it, or remove the setting from {}.",
                    host_name(host),
                    shim_dir.join(ENTRY_NAME).display(),
                    path.display()
                );
            }
        }
    }
}

fn path_contains_shims(shim_dir: &Path) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|candidate| candidate == shim_dir))
}

/// A host binary found on `PATH`, with whether its configuration already
/// selects apexe.
struct DetectedHost {
    host: AgentHost,
    binary: PathBuf,
    configured: bool,
}

/// Name the detected agent hosts that still run commands outside apexe.
///
/// `env --enable` installs the launcher and the PATH shims, but it does not
/// edit another tool's configuration: the scope (project versus user) and the
/// consent (a Claude Code hook answers `allow`) belong to the operator. The gap
/// that leaves is silent — an installed but unconfigured host runs every
/// command ungoverned — so this states the remaining step at the moment the
/// launcher becomes available, rather than leaving it to the documentation.
///
/// Nothing is written and nothing is refused: this is a report, not an
/// installation step.
pub(crate) fn report_unconfigured_hosts(shim_dir: &Path) {
    use std::io::IsTerminal;
    // `env --enable` is also how scripts and tests install shims, and a hint
    // list there would land in captured output that expects none.
    if !std::io::stderr().is_terminal() {
        return;
    }
    let lines = hint_lines(&detected_hosts(shim_dir));
    if lines.is_empty() {
        return;
    }
    eprintln!("Detected agent hosts that do not use apexe yet:");
    for line in lines {
        eprintln!("  {line}");
    }
}

fn detected_hosts(shim_dir: &Path) -> Vec<DetectedHost> {
    const HOSTS: [AgentHost; 4] = [
        AgentHost::Opencode,
        AgentHost::Pi,
        AgentHost::Codex,
        AgentHost::Claude,
    ];
    HOSTS
        .iter()
        .filter_map(|host| {
            let binary = which::which(binary_name(*host)).ok()?;
            // Configuration files only. `inspect_host` also accepts a process
            // PATH that already contains the shim directory, which answers a
            // different question: this report runs inside a governed shell, so
            // that PATH is inherited from the caller and says nothing about how
            // the host will be launched later.
            Some(DetectedHost {
                host: *host,
                binary,
                configured: config_mentions_apexe(*host, shim_dir),
            })
        })
        .collect()
}

fn hint_lines(hosts: &[DetectedHost]) -> Vec<String> {
    hosts
        .iter()
        .filter(|detected| !detected.configured)
        .map(|detected| {
            format!(
                "{:<8} {}  ->  {}",
                host_name(detected.host),
                detected.binary.display(),
                remedy(detected.host)
            )
        })
        .collect()
}

/// The step that host needs, phrased so it can be pasted as-is.
fn remedy(host: AgentHost) -> &'static str {
    match host {
        AgentHost::Opencode => "apexe agent opencode --write --scope user",
        AgentHost::Pi => "apexe agent pi --write",
        AgentHost::Codex => {
            "no configuration file: run 'apexe env --reload --shell <the shell Codex uses>', \
             then restart Codex"
        }
        AgentHost::Claude => "apexe agent claude --write --scope user",
    }
}

fn config_mentions_apexe(host: AgentHost, shim_dir: &Path) -> bool {
    host_is_configured(host, &config_candidates(host), shim_dir)
}

fn host_is_configured(host: AgentHost, candidates: &[PathBuf], shim_dir: &Path) -> bool {
    candidates.iter().any(|path| {
        std::fs::read_to_string(path).is_ok_and(|text| host_text_configured(host, &text, shim_dir))
    })
}

fn binary_name(host: AgentHost) -> &'static str {
    match host {
        AgentHost::Opencode => "opencode",
        AgentHost::Pi => "pi",
        AgentHost::Codex => "codex",
        AgentHost::Claude => "claude",
    }
}

fn configured_note(host: AgentHost) -> String {
    match host {
        AgentHost::Opencode => "OpenCode is configured to use apexe-sh; restart it after changing the file".into(),
        AgentHost::Pi => "pi is configured to use apexe-sh; restart it after changing the file".into(),
        AgentHost::Codex => "Codex loads shims from its login-shell startup files; restart Codex after reloading shims".into(),
        AgentHost::Claude => "Claude Code has an apexe PreToolUse hook configured".into(),
    }
}

fn print_text(report: &DoctorReport) {
    if report.launcher.problems.is_empty() {
        println!(
            "[PASS] LAUNCHER {} (manifest: {})",
            report.launcher.shims.entry_point.display(),
            if report.launcher.shims.manifest_present {
                "present"
            } else {
                "missing"
            }
        );
    } else {
        for problem in &report.launcher.problems {
            println!("[FAIL] LAUNCHER {problem}");
        }
    }
    for warning in &report.launcher.warnings {
        println!("[WARN] LAUNCHER {warning}");
    }
    match &report.host.binary {
        Some(path) => println!(
            "[PASS] HOST {} {}",
            host_name(report.host.host),
            path.display()
        ),
        None => println!(
            "[WARN] HOST {} executable not found on PATH",
            host_name(report.host.host)
        ),
    }
    if report.host.configured {
        println!(
            "[PASS] CONFIG {}",
            report
                .host
                .config
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<process environment>".into())
        );
    } else {
        println!(
            "[WARN] CONFIG {}",
            report
                .host
                .config
                .as_ref()
                .map_or("no host configuration file found", |path| path
                    .to_str()
                    .unwrap_or("host configuration file found"))
        );
    }
    println!("[INFO] {}", report.host.note);
}

fn host_name(host: AgentHost) -> &'static str {
    match host {
        AgentHost::Opencode => "opencode",
        AgentHost::Pi => "pi",
        AgentHost::Codex => "codex",
        AgentHost::Claude => "claude",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(host: AgentHost, configured: bool) -> DetectedHost {
        DetectedHost {
            host,
            binary: PathBuf::from(format!("/usr/local/bin/{}", host_name(host))),
            configured,
        }
    }

    #[test]
    fn test_hint_lines_reports_only_unconfigured_hosts() {
        let hosts = [
            detected(AgentHost::Pi, false),
            detected(AgentHost::Opencode, true),
        ];
        let lines = hint_lines(&hosts);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("pi "));
        assert!(lines[0].contains("apexe agent pi --write"));
    }

    #[test]
    fn test_hint_lines_is_empty_when_every_detected_host_is_configured() {
        let hosts = [
            detected(AgentHost::Pi, true),
            detected(AgentHost::Claude, true),
        ];
        assert!(hint_lines(&hosts).is_empty());
    }

    #[test]
    fn test_hint_lines_names_user_scope_for_hosts_with_project_config() {
        let hosts = [
            detected(AgentHost::Opencode, false),
            detected(AgentHost::Claude, false),
        ];
        let lines = hint_lines(&hosts);
        assert!(lines[0].contains("--scope user"));
        assert!(lines[1].contains("--scope user"));
    }

    #[test]
    fn test_hint_lines_explains_codex_has_no_configuration_file() {
        let lines = hint_lines(&[detected(AgentHost::Codex, false)]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no configuration file"));
        assert!(lines[0].contains("apexe env --reload --shell"));
    }

    #[test]
    fn test_host_is_configured_reads_the_selection_from_a_candidate_file() {
        let root = tempfile::tempdir().unwrap();
        let settings = root.path().join("settings.json");
        let shim_dir = Path::new("/home/u/.apexe/bin");
        std::fs::write(&settings, r#"{"shellPath": "/home/u/.apexe/bin/apexe-sh"}"#).unwrap();
        assert!(host_is_configured(
            AgentHost::Pi,
            std::slice::from_ref(&settings),
            shim_dir
        ));
        std::fs::write(&settings, r#"{"theme": "dark"}"#).unwrap();
        assert!(!host_is_configured(
            AgentHost::Pi,
            std::slice::from_ref(&settings),
            shim_dir
        ));
        assert!(!host_is_configured(
            AgentHost::Pi,
            &[root.path().join("missing.json")],
            shim_dir
        ));
    }
}
