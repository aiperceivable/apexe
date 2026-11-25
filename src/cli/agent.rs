use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::ApexeConfig;

use super::sh::{self, CheckFormat, CheckReport, ENTRY_NAME};

/// Print host configuration or check the installed launcher for one host.
#[derive(Debug, clap::Args)]
#[command(
    after_help = "Examples:\n  apexe agent opencode                    Print the OpenCode shell configuration\n  apexe agent antigravity                 Print the Antigravity hooks.json configuration\n  apexe agent --check pi                  Check pi launcher and config readiness\n  apexe agent --check claude --format json\n\nHosts that accept a custom shell use the installed apexe-sh entry. Codex,\nClaude Code and Antigravity cannot replace their shell; their output explains\nthe supported PATH or PreToolUse hook integration."
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
    Antigravity,
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
    let mut report = sh::check_report(config, None, false, &[])?;
    if write {
        check_shell_acl(&mut report, config);
    }
    for warning in &report.warnings {
        eprintln!("Warning: {warning}");
    }
    for problem in &report.problems {
        eprintln!("Problem: {problem}");
    }
    if write {
        anyhow::ensure!(
            report.problems.is_empty(),
            "refusing to write {} configuration: {} installation problem(s). Run 'apexe agent --check {}'.",
            host_name(host),
            report.problems.len(),
            host_name(host)
        );
        return write_host(host, scope, &report);
    }
    match host {
        AgentHost::Opencode => println!("{}", report.host_config.opencode),
        AgentHost::Pi => println!("{}", report.host_config.pi),
        AgentHost::Codex => println!("{}", report.host_config.codex),
        AgentHost::Claude => println!("{}", report.host_config.claude_code),
        AgentHost::Antigravity => println!("{}", report.host_config.antigravity),
    }
    Ok(())
}

fn load_acl(
    selected: Option<&Path>,
    config: &ApexeConfig,
) -> anyhow::Result<(PathBuf, apcore::ACL)> {
    let path = selected.map_or_else(|| config.config_dir.join("acl.yaml"), Path::to_path_buf);
    let policy = crate::governance::AclManager::from_config(&path).with_context(|| {
        format!("Cannot load ACL at {}. Restore or repair the selected policy, or run 'apexe init' to prepare a missing default ACL", path.display())
    })?;
    Ok((std::path::absolute(path)?, policy.into_inner()))
}

fn check_shell_acl(report: &mut CheckReport, config: &ApexeConfig) {
    if report.shims.manifest_present {
        match load_acl(report.shims.acl.as_deref(), config) {
            Err(error) => report.problems.push(format!("{error:#}")),
            Ok((path, _)) if !report.shims.acl_pinned => report.problems.push(format!(
                "ACL {} is not pinned in this shim installation; run 'apexe env --reload --shim-dir {}' to bind the policy",
                path.display(), report.shims.directory.display()
            )),
            Ok(_) => {}
        }
    }
}

fn mcp_arguments(config: &ApexeConfig) -> anyhow::Result<Vec<String>> {
    let directory = sh::shim_directory(config);
    let manifest = directory.join(crate::runtime_path::MANIFEST_NAME);
    let installation = match std::fs::symlink_metadata(&manifest) {
        Ok(_) => Some(super::shim::read_manifest(&directory).with_context(|| {
            format!(
                "Cannot read selected installation from {}",
                manifest.display()
            )
        })?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let (acl, policy) = load_acl(
        installation.as_ref().and_then(|entry| entry.acl.as_deref()),
        config,
    )?;
    crate::module::registry::validate_approval_backend(&acl, &policy, false)?;
    let mut arguments = vec!["serve".into(), "--acl".into(), config_path_arg(&acl)?];
    if let Some(installation) = installation {
        anyhow::ensure!(
            installation.bindings_pattern == config.bindings_pattern,
            "The binding pattern in {} differs from the effective apcore configuration. Restore the matching pattern before generating host configuration",
            manifest.display()
        );
        arguments.extend([
            "--bindings-dir".into(),
            config_path_arg(&installation.bindings_dir)?,
        ]);
    }
    Ok(arguments)
}

fn config_path_arg(path: &Path) -> anyhow::Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        anyhow::anyhow!(
            "Host configuration requires a valid UTF-8 path: {}",
            path.display()
        )
    })
}

fn mcp(
    config: &ApexeConfig,
    host: AgentHost,
    write: bool,
    scope: Option<AgentScope>,
) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let arguments = if host == AgentHost::Pi {
        Vec::new()
    } else {
        mcp_arguments(config)?
    };
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("apexe executable path must be valid UTF-8"))?;

    match host {
        AgentHost::Opencode => {
            let mut command = vec![json!(executable)];
            command.extend(arguments.iter().map(|argument| json!(argument)));
            let service = json!({
                "type": "local",
                "command": command,
            });
            let snippet = json!({
                "mcp": {"servers": {"apexe": service}},
                "permissions": [opencode_shell_deny()]
            });
            if write {
                let path = host_config_path(host, scope)?;
                let settings = prepare_json(&path, |object| {
                    let mcp = object_entry(object, "mcp")?;
                    merge_service(mcp, "servers", &service)?;
                    let rules = array_entry(object, "permissions", "permissions")?;
                    if !rules.iter().any(|rule| rule == &opencode_shell_deny()) {
                        rules.push(opencode_shell_deny());
                    }
                    Ok(())
                })?;
                verify_opencode_permission_sources(&path, scope, &settings["permissions"])?;
                publish_json(&path, &settings)?;
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
                let settings =
                    prepare_json(&path, |object| ensure_permission_deny(object, "Bash"))?;
                let completed = if selected_scope == AgentScope::Project {
                    let mcp_path = std::env::current_dir()?.join(".mcp.json");
                    write_json(&mcp_path, |object| {
                        merge_service(object, "mcpServers", &mcp["mcpServers"]["apexe"])
                    })?;
                    println!("Wrote {}", mcp_path.display());
                    Some(mcp_path)
                } else {
                    let command = shell_words::join(
                        std::iter::once(executable).chain(arguments.iter().map(String::as_str)),
                    );
                    println!("Run `claude mcp add --scope user apexe -- {command}` to register the user-scope server.");
                    None
                };
                let result = publish_json(&path, &settings);
                match completed {
                    Some(completed) => result.with_context(|| format!(
                        "Already wrote {}; failed to write {}. Correct the error and retry the same command",
                        completed.display(), path.display()
                    ))?,
                    None => result?,
                }
                println!("Wrote {}", path.display());
                println!(
                    "Reload Claude Code and verify MCP access and Bash denial in the running host."
                );
            } else {
                println!("{}", serde_json::to_string_pretty(&mcp)?);
                println!("Also add `Bash` to permissions.deny in .claude/settings.json.");
            }
        }
        AgentHost::Codex => {
            let quoted = toml_basic_string(executable);
            let args = arguments
                .iter()
                .map(|argument| toml_basic_string(argument))
                .collect::<Vec<_>>()
                .join(", ");
            println!("[mcp_servers.apexe]\ncommand = {quoted}\nargs = [{args}]");
            println!(
                "# Codex keeps its shell; combine this configuration with a sandbox if needed."
            );
            anyhow::ensure!(
                !write,
                "Codex MCP configuration is printed; merge it into config.toml manually"
            );
        }
        AgentHost::Pi => {
            println!("pi has no stable MCP-only profile in the tested version; use `apexe agent pi` for shellPath integration.");
            anyhow::ensure!(!write, "pi MCP configuration is not generated");
        }
        AgentHost::Antigravity => {
            write_antigravity_mcp(executable, &arguments, write, scope)?;
        }
    }
    Ok(())
}

fn write_antigravity_mcp(
    executable: &str,
    arguments: &[String],
    write: bool,
    scope: Option<AgentScope>,
) -> anyhow::Result<()> {
    let service = json!({
        "command": executable,
        "args": arguments
    });
    let snippet = json!({
        "mcpServers": {
            "apexe": service
        }
    });
    if !write {
        println!("{}", serde_json::to_string_pretty(&snippet)?);
        println!(
            "# Merge into ~/.gemini/config/mcp_config.json. Antigravity keeps its terminal tool; restrict run_command in Settings if you need strict MCP-only access."
        );
        return Ok(());
    }
    if scope == Some(AgentScope::Project) {
        anyhow::bail!(
            "Antigravity MCP configuration is user-level (~/.gemini/config/mcp_config.json); use --scope user"
        );
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot locate home directory"))?;
    let path = home.join(".gemini/config/mcp_config.json");
    write_json(&path, |object| {
        merge_service(object, "mcpServers", &service)
    })?;
    println!("Wrote {}", path.display());
    println!(
        "Antigravity keeps its terminal tool; restrict run_command in Antigravity Settings if you need strict MCP-only access."
    );
    Ok(())
}

fn toml_basic_string(value: &str) -> String {
    // JSON string escapes are valid TOML basic-string escapes. TOML also
    // requires escaping DEL, which serde_json leaves as a literal character.
    json!(value).to_string().replace('\u{7f}', "\\u007F")
}

fn object_entry<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
) -> anyhow::Result<&'a mut Map<String, Value>> {
    object
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("'{key}' must be an object"))
}

fn array_entry<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    label: &str,
) -> anyhow::Result<&'a mut Vec<Value>> {
    object
        .entry(key)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("'{label}' must be an array"))
}

fn merge_service(
    object: &mut Map<String, Value>,
    key: &str,
    service: &Value,
) -> anyhow::Result<()> {
    object_entry(object, key)?.insert("apexe".into(), service.clone());
    Ok(())
}

fn ensure_permission_deny(object: &mut Map<String, Value>, rule: &str) -> anyhow::Result<()> {
    let permissions = object_entry(object, "permissions")?;
    let deny = array_entry(permissions, "deny", "permissions.deny")?;
    anyhow::ensure!(
        deny.iter().all(Value::is_string),
        "'permissions.deny' must contain only strings"
    );
    if !deny.iter().any(|value| value == rule) {
        deny.push(json!(rule));
    }
    Ok(())
}

fn opencode_shell_deny() -> Value {
    json!({"action": "shell", "resource": "*", "effect": "deny"})
}

// Let the supported host locate and normalize its config sources. Do not
// guess the ordering of user, project, or legacy permission rules.
fn verify_opencode_permission_sources(
    path: &Path,
    scope: Option<AgentScope>,
    rules: &Value,
) -> anyhow::Result<()> {
    verify_opencode_permissions(path, scope.unwrap_or(AgentScope::Project), rules).map_err(|error| anyhow::anyhow!(
        "Cannot verify OpenCode MCP-only permissions for {}: {error:#}. Use 'apexe agent opencode --mcp-only' and configure permissions manually",
        path.display()
    ))
}

fn verify_opencode_permissions(
    path: &Path,
    scope: AgentScope,
    rules: &Value,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var_os("OPENCODE_CONFIG_CONTENT").is_none(),
        "OPENCODE_CONFIG_CONTENT is set; cannot isolate the permission edit"
    );
    if scope == AgentScope::Project {
        anyhow::ensure!(
            std::env::var_os("OPENCODE_DISABLE_PROJECT_CONFIG").is_none(),
            "OPENCODE_DISABLE_PROJECT_CONFIG is set; cannot verify the project permission edit"
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let version = runtime.block_on(opencode_output(&["--version"], None))?;
    let version = version.trim();
    anyhow::ensure!(
        matches!(version, "opencode v2.3.0" | "2.3.0"),
        "automatic MCP-only edits require OpenCode 2.3.0"
    );
    if scope == AgentScope::User {
        let config_dir = runtime.block_on(opencode_output(&["debug", "paths", "config"], None))?;
        ensure_opencode_config_dir(path, config_dir.trim())?;
    }
    let diagnostics =
        runtime.block_on(opencode_output(&["--standalone", "debug", "config"], None))?;
    for source in serde_json::from_str::<Vec<Value>>(&diagnostics)? {
        let name = source["path"].as_str().unwrap_or("unnamed config source");
        verify_opencode_source(name, &source["info"])?;
    }
    verify_opencode_source("proposed configuration", &json!({"permissions": rules}))?;

    // Test the proposal without publishing it. The source check above rejects
    // conflicts even if the diagnostic overlay would hide them through order.
    let candidate = json!({"permissions": rules}).to_string();
    let diagnostics = runtime.block_on(opencode_output(
        &["--standalone", "debug", "agents"],
        Some(&candidate),
    ))?;
    verify_opencode_agents(&diagnostics)
}

fn ensure_opencode_config_dir(path: &Path, config_dir: &str) -> anyhow::Result<()> {
    let actual = Path::new(config_dir);
    let matches = path.parent().is_some_and(|expected| {
        actual == expected
            || expected
                .canonicalize()
                .ok()
                .is_some_and(|directory| actual.canonicalize().ok() == Some(directory))
    });
    anyhow::ensure!(
        matches,
        "OpenCode uses a different user configuration directory"
    );
    Ok(())
}

#[derive(Deserialize)]
struct OpenCodePermission {
    action: String,
    resource: String,
    effect: OpenCodeEffect,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum OpenCodeEffect {
    Allow,
    Ask,
    Deny,
}

fn verify_opencode_source(name: &str, info: &Value) -> anyhow::Result<()> {
    if let Some(rules) = info.get("permissions") {
        refuse_opencode_conflicts(name, rules)?;
    }
    if let Some(agents) = info.get("agents") {
        let agents = agents
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("'{name}': unrecognized OpenCode agents"))?;
        for (agent, info) in agents {
            if let Some(rules) = info.get("permissions") {
                refuse_opencode_conflicts(&format!("{name} (agent {agent})"), rules)?;
            }
        }
    }
    Ok(())
}

fn refuse_opencode_conflicts(source: &str, rules: &Value) -> anyhow::Result<()> {
    let rules: Vec<OpenCodePermission> = serde_json::from_value(rules.clone())
        .with_context(|| format!("'{source}': unrecognized OpenCode permissions"))?;
    anyhow::ensure!(
        rules.iter().all(|rule| rule.effect == OpenCodeEffect::Deny
            || !(opencode_matches_action(&rule.action, b"shell")
                || opencode_matches_action(&rule.action, b"bash"))),
        "'{source}': conflicting OpenCode shell permission"
    );
    Ok(())
}

fn opencode_matches_action(pattern: &str, action: &[u8]) -> bool {
    // Track the matched prefixes of the fixed ASCII action. OpenCode action
    // patterns use whole-value '*' and '?' matching.
    let mut matched = vec![false; action.len() + 1];
    matched[0] = true;
    for symbol in pattern.bytes() {
        if symbol == b'*' {
            for index in 1..matched.len() {
                matched[index] |= matched[index - 1];
            }
        } else {
            for index in (1..matched.len()).rev() {
                matched[index] = matched[index - 1]
                    && (symbol == b'?'
                        || symbol == action[index - 1]
                        || (cfg!(windows) && symbol.eq_ignore_ascii_case(&action[index - 1])));
            }
            matched[0] = false;
        }
    }
    matched[action.len()]
}

fn verify_opencode_agents(diagnostics: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Agent {
        id: String,
        permissions: Vec<OpenCodePermission>,
    }
    let agents: Vec<Agent> = serde_json::from_str(diagnostics)?;
    anyhow::ensure!(
        !agents.is_empty(),
        "OpenCode reported no effective agent rules"
    );
    for agent in agents {
        let blanket_shell_deny = agent
            .permissions
            .iter()
            .rev()
            .filter(|rule| opencode_matches_action(&rule.action, b"shell"))
            .find(|rule| rule.effect != OpenCodeEffect::Deny || rule.resource == "*")
            .is_some_and(|rule| rule.effect == OpenCodeEffect::Deny);
        anyhow::ensure!(
            blanket_shell_deny,
            "cannot confirm Shell denial for OpenCode agent '{}'",
            agent.id
        );
    }
    Ok(())
}

async fn opencode_output(args: &[&str], candidate: Option<&str>) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new("opencode");
    command
        .args(args)
        .kill_on_drop(true)
        .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
        .env("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true")
        .stdin(std::process::Stdio::null());
    if let Some(candidate) = candidate {
        command.env("OPENCODE_CONFIG_CONTENT", candidate);
    }
    let output =
        tokio::time::timeout(std::time::Duration::from_secs(5), command.output()).await??;
    anyhow::ensure!(
        output.status.success(),
        "OpenCode diagnostics exited with {}",
        output.status
    );
    Ok(String::from_utf8(output.stdout)?)
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
                Ok(())
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
                Ok(())
            })?;
            println!("Wrote {}", path.display());
        }
        AgentHost::Codex => write_codex(report)?,
        AgentHost::Claude => {
            let path = host_config_path(host, scope)?;
            let executable = std::env::current_exe()?.canonicalize()?;
            write_json(&path, |object| ensure_claude_hook(object, &executable))?;
            println!("Wrote {}", path.display());
        }
        AgentHost::Antigravity => {
            let path = host_config_path(host, scope)?;
            let executable = std::env::current_exe()?.canonicalize()?;
            write_json(&path, |object| ensure_antigravity_hook(object, &executable))?;
            println!("Wrote {}", path.display());
        }
    }
    Ok(())
}

fn write_codex(report: &CheckReport) -> anyhow::Result<()> {
    let shell = super::shell::Shell::from_path(&report.real_shell)?
        .to_possible_value()
        .ok_or_else(|| anyhow::anyhow!("Unsupported Codex shell"))?;
    let name = shell.get_name();
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["env", "--reload", "--shell", name, "--shim-dir"])
        .arg(&report.shims.directory)
        .status()?;
    anyhow::ensure!(status.success(), "apexe env --reload --shell {name} failed");
    println!("Reloaded {name} startup configuration for Codex; restart Codex to refresh its shell snapshot");
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
        (AgentHost::Antigravity, AgentScope::Project) => cwd.join(".agents/hooks.json"),
        (AgentHost::Antigravity, AgentScope::User) => home.join(".gemini/config/hooks.json"),
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

fn write_json(
    path: &Path,
    mutate: impl FnOnce(&mut Map<String, Value>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let root = prepare_json(path, mutate)?;
    publish_json(path, &root)
}

fn prepare_json(
    path: &Path,
    mutate: impl FnOnce(&mut Map<String, Value>) -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
    let mut root = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(error) => return Err(error).with_context(|| format!("Cannot read {}", path.display())),
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} must contain a JSON object", path.display()))?;
    mutate(object).map_err(|error| anyhow::anyhow!("{}: {error:#}", path.display()))?;
    Ok(root)
}

fn publish_json(path: &Path, root: &Value) -> anyhow::Result<()> {
    let publish = || -> anyhow::Result<()> {
        let existing_permissions = match std::fs::metadata(path) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("apexe-tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(root)?)?;
        if let Some(permissions) = existing_permissions {
            std::fs::set_permissions(&temporary, permissions)?;
        }
        std::fs::rename(&temporary, path)?;
        Ok(())
    };
    publish().with_context(|| format!("Cannot write {}", path.display()))
}

fn hook_arguments(command: &str, host: &str) -> Option<Vec<String>> {
    let arguments = shell_words::split(command).ok()?;
    let executable = Path::new(arguments.first()?).file_stem()?.to_str()?;
    let owned = executable == "apexe"
        && arguments.get(1).is_some_and(|argument| argument == "hook")
        && arguments.get(2).is_some_and(|argument| argument == host)
        && (arguments.len() == 3 || arguments.len() == 4 && arguments[3] == "--persistent");
    owned.then_some(arguments)
}

fn repair_pretooluse_hooks(
    entries: &mut [Value],
    matcher: &str,
    host: &str,
    executable: &str,
) -> bool {
    let mut repaired = false;
    for entry in entries {
        if entry["matcher"] != matcher {
            continue;
        }
        let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        for hook in hooks {
            let Some(mut arguments) = hook
                .get("command")
                .and_then(Value::as_str)
                .and_then(|command| hook_arguments(command, host))
            else {
                continue;
            };
            arguments[0] = executable.into();
            hook["command"] = json!(shell_words::join(arguments));
            repaired = true;
        }
    }
    repaired
}

fn ensure_claude_hook(object: &mut Map<String, Value>, executable: &Path) -> anyhow::Result<()> {
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("apexe executable path must be valid UTF-8"))?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Claude hooks must be a JSON object"))?;
    let entries = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("Claude PreToolUse hooks must be an array"))?;
    if !repair_pretooluse_hooks(entries, "Bash", "claude", executable) {
        entries.push(json!({
            "matcher": "Bash",
            "hooks": [{"type":"command", "command":format!("{} hook claude", shell_words::quote(executable)), "timeout":30}]
        }));
    }
    Ok(())
}

fn ensure_antigravity_hook(
    object: &mut Map<String, Value>,
    executable: &Path,
) -> anyhow::Result<()> {
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("apexe executable path must be valid UTF-8"))?;
    if let Some(apexe) = object.get("apexe") {
        let apexe = apexe
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Antigravity 'apexe' hook must be a JSON object"))?;
        if let Some(pre_tool_use) = apexe.get("PreToolUse") {
            anyhow::ensure!(
                pre_tool_use.is_array(),
                "Antigravity PreToolUse hooks must be an array"
            );
        }
    }
    let mut repaired = false;
    for (name, spec) in object.iter_mut() {
        let disabled = spec.get("enabled").and_then(Value::as_bool) == Some(false);
        if disabled && name != "apexe" {
            continue;
        }
        let Some(entries) = spec.get_mut("PreToolUse").and_then(Value::as_array_mut) else {
            continue;
        };
        if repair_pretooluse_hooks(entries, "run_command", "antigravity", executable) {
            repaired = true;
            if disabled {
                if let Some(map) = spec.as_object_mut() {
                    map.remove("enabled");
                }
            }
        }
    }
    if !repaired {
        let spec = object_entry(object, "apexe")?;
        if spec.get("enabled").and_then(Value::as_bool) == Some(false) {
            spec.remove("enabled");
        }
        let entries = array_entry(spec, "PreToolUse", "apexe.PreToolUse")?;
        entries.push(json!({
            "matcher": "run_command",
            "hooks": [{"type":"command", "command":format!("{} hook antigravity", shell_words::quote(executable)), "timeout":30}]
        }));
    }
    Ok(())
}

fn check(config: &ApexeConfig, host: AgentHost, format: CheckFormat) -> anyhow::Result<()> {
    let mut launcher = sh::check_report(config, None, false, &[])?;
    check_shell_acl(&mut launcher, config);
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
    let binary = detect_host_installation(host);
    let mut first_existing = None;
    for path in config_candidates(host) {
        if first_existing.is_none() && path.is_file() {
            first_existing = Some(path.clone());
        }
        if let Some((configured, note)) =
            inspect_configuration_file(host, &path, &launcher.shims.directory)
        {
            return HostReport {
                host,
                binary,
                config: Some(path),
                configured,
                note,
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
        note: "no matching current apexe host configuration found; after `apexe env --enable`, run `apexe agent <host> --write` to install or repair the selected scope".into(),
    }
}

/// None means this source does not select an integration; a false result is
/// authoritative and must not be hidden by a lower-priority configuration.
fn inspect_configuration_file(
    host: AgentHost,
    path: &Path,
    shim_dir: &Path,
) -> Option<(bool, String)> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => return Some((false, format!("Cannot read host configuration: {error}"))),
    };
    if host == AgentHost::Opencode {
        let Ok(settings) = serde_json::from_str::<Map<String, Value>>(&text) else {
            return Some((false, "OpenCode configuration must be a valid JSON object; repair this source before checking lower-priority settings".into()));
        };
        if !settings.contains_key("shell") {
            return None;
        }
        let configured = host_text_configured(host, &text, shim_dir);
        let note = if configured {
            configured_note(host)
        } else {
            "OpenCode's effective shell selection does not match this installation; inspect this configuration or run `apexe agent opencode --write` for the same scope".into()
        };
        return Some((configured, note));
    }
    if host_text_configured(host, &text, shim_dir) {
        return Some((true, configured_note(host)));
    }
    if host == AgentHost::Claude && !claude_hooks(&text).is_empty() {
        return Some((false, "Claude hook selects a stale apexe executable; run `apexe env --enable`, then `apexe agent claude --write` for the same scope".into()));
    }
    if host == AgentHost::Antigravity && !antigravity_hooks(&text).is_empty() {
        return Some((false, "Antigravity hook selects a stale apexe executable; run `apexe env --enable`, then `apexe agent antigravity --write` for the same scope".into()));
    }
    None
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
        AgentHost::Codex => codex_startup_paths().unwrap_or_default(),
        AgentHost::Claude => vec![
            cwd.join(".claude/settings.json"),
            home.join(".claude/settings.json"),
        ],
        AgentHost::Antigravity => vec![
            cwd.join(".agents/hooks.json"),
            home.join(".gemini/config/hooks.json"),
        ],
    }
}

fn codex_startup_paths() -> anyhow::Result<Vec<PathBuf>> {
    let (real_shell, _) = sh::resolve_real_shell(None, &std::env::current_exe()?)?;
    let shell = super::shell::Shell::from_path(&real_shell)?;
    let mut paths = super::shell_init::startup_paths(shell, None)?;
    // The installer lists interactive rc files before the login profile.
    // Codex's noninteractive login shell does not read .bashrc or .zshrc.
    Ok(paths.pop().into_iter().collect())
}

fn selects_entry(text: &str, key: &str, expected: &Path) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let Some(value) = value.get(key).and_then(Value::as_str) else {
        return false;
    };
    let path = if let Some(relative) = value.strip_prefix("~/") {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        home.join(relative)
    } else {
        PathBuf::from(value)
    };
    path == expected
        || path.file_name() == expected.file_name()
            && path
                .parent()
                .and_then(|parent| parent.canonicalize().ok())
                .zip(
                    expected
                        .parent()
                        .and_then(|parent| parent.canonicalize().ok()),
                )
                .is_some_and(|(left, right)| left == right)
}

fn claude_hooks(text: &str) -> Vec<Vec<String>> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(entries) = value["hooks"]["PreToolUse"].as_array() else {
        return Vec::new();
    };
    collect_matching_hooks(entries, "Bash", "claude")
}

fn antigravity_hooks(text: &str) -> Vec<Vec<String>> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    map.values()
        .filter(|spec| spec.get("enabled").and_then(Value::as_bool) != Some(false))
        .filter_map(|spec| spec.get("PreToolUse").and_then(Value::as_array))
        .flat_map(|entries| collect_matching_hooks(entries, "run_command", "antigravity"))
        .collect()
}

fn collect_matching_hooks(entries: &[Value], matcher: &str, host: &str) -> Vec<Vec<String>> {
    entries
        .iter()
        .filter(|entry| entry["matcher"] == matcher)
        .filter_map(|entry| entry["hooks"].as_array())
        .flatten()
        .filter_map(|hook| {
            hook["command"]
                .as_str()
                .and_then(|command| hook_arguments(command, host))
        })
        .collect()
}

fn hooks_match_current_exe(hooks: &[Vec<String>]) -> bool {
    let current = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    hooks.iter().any(|arguments| {
        Path::new(&arguments[0])
            .canonicalize()
            .ok()
            .zip(current.as_ref())
            .is_some_and(|(left, right)| left == *right)
    })
}

fn host_text_configured(host: AgentHost, text: &str, shim_dir: &Path) -> bool {
    match host {
        AgentHost::Opencode => selects_entry(text, "shell", &shim_dir.join(ENTRY_NAME)),
        AgentHost::Pi => selects_entry(text, "shellPath", &shim_dir.join(ENTRY_NAME)),
        AgentHost::Codex => text.contains(shim_dir.to_string_lossy().as_ref()),
        AgentHost::Claude => hooks_match_current_exe(&claude_hooks(text)),
        AgentHost::Antigravity => hooks_match_current_exe(&antigravity_hooks(text)),
    }
}

/// Report host configuration that still depends on the removed installation.
///
/// OpenCode/pi lose their shell entry point; Claude and Antigravity hooks
/// refuse shell tool calls without a manifest. Uninstall stays explicit and
/// possible, but never silently edits another application's configuration to
/// hide that consequence.
pub(crate) fn warn_if_host_depends_on_removed_shims(shim_dir: &Path) {
    const DEPENDENT_HOSTS: [AgentHost; 4] = [
        AgentHost::Opencode,
        AgentHost::Pi,
        AgentHost::Claude,
        AgentHost::Antigravity,
    ];
    for host in DEPENDENT_HOSTS {
        for path in config_candidates(host) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if host_text_configured(host, &text, shim_dir) {
                match host {
                    AgentHost::Claude => {
                        eprintln!("Warning: claude still has an apexe hook in {}; without shims it will refuse Bash calls. Run 'apexe env --enable' to restore the installation, or explicitly remove that hook to stop routing through apexe.", path.display());
                    }
                    AgentHost::Antigravity => {
                        eprintln!("Warning: antigravity still has an apexe hook in {}; without shims it will refuse run_command calls. Run 'apexe env --enable' to restore the installation, or explicitly remove that hook to stop routing through apexe.", path.display());
                    }
                    _ => {
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

/// Detected hosts and whether each already selects apexe, for `setup`.
///
/// Detection lives here so that `env --enable`'s hint list and the one-step
/// installer cannot disagree about which hosts exist on this machine.
pub(crate) fn detected_host_status(shim_dir: &Path) -> Vec<(AgentHost, PathBuf, bool)> {
    detected_hosts(shim_dir)
        .into_iter()
        .map(|detected| (detected.host, detected.binary, detected.configured))
        .collect()
}

fn detected_hosts(shim_dir: &Path) -> Vec<DetectedHost> {
    const HOSTS: [AgentHost; 5] = [
        AgentHost::Opencode,
        AgentHost::Pi,
        AgentHost::Codex,
        AgentHost::Claude,
        AgentHost::Antigravity,
    ];
    HOSTS
        .iter()
        .filter_map(|host| {
            let binary = detect_host_installation(*host)?;
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
        AgentHost::Antigravity => "apexe agent antigravity --write --scope user",
    }
}

fn config_mentions_apexe(host: AgentHost, shim_dir: &Path) -> bool {
    host_is_configured(host, &config_candidates(host), shim_dir)
}

fn host_is_configured(host: AgentHost, candidates: &[PathBuf], shim_dir: &Path) -> bool {
    candidates
        .iter()
        .find_map(|path| {
            inspect_configuration_file(host, path, shim_dir).map(|(configured, _)| configured)
        })
        .unwrap_or(false)
}

fn detect_host_installation(host: AgentHost) -> Option<PathBuf> {
    match host {
        AgentHost::Opencode => which::which("opencode").ok(),
        AgentHost::Pi => which::which("pi").ok(),
        AgentHost::Codex => which::which("codex").ok(),
        AgentHost::Claude => which::which("claude").ok(),
        AgentHost::Antigravity => which::which("agy")
            .ok()
            .or_else(|| which::which("antigravity").ok())
            .or_else(detect_antigravity_home),
    }
}

fn detect_antigravity_home() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        ".gemini/antigravity",
        ".gemini/antigravity-ide",
        ".gemini/antigravity-cli",
    ]
    .into_iter()
    .map(|relative| home.join(relative))
    .find(|path| path.is_dir())
}

fn configured_note(host: AgentHost) -> String {
    match host {
        AgentHost::Opencode => "OpenCode is configured to use apexe-sh; restart it after changing the file".into(),
        AgentHost::Pi => "pi is configured to use apexe-sh; restart it after changing the file".into(),
        AgentHost::Codex => "Codex loads shims from its login-shell startup files; restart Codex after reloading shims".into(),
        AgentHost::Claude => "Claude Code has an apexe PreToolUse hook configured".into(),
        AgentHost::Antigravity => "Antigravity has an apexe PreToolUse hook configured".into(),
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
        None if report.host.host == AgentHost::Antigravity => println!(
            "[WARN] HOST antigravity installation not found on PATH or in ~/.gemini/antigravity*"
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

pub(crate) fn host_name(host: AgentHost) -> &'static str {
    match host {
        AgentHost::Opencode => "opencode",
        AgentHost::Pi => "pi",
        AgentHost::Codex => "codex",
        AgentHost::Claude => "claude",
        AgentHost::Antigravity => "antigravity",
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
            detected(AgentHost::Antigravity, false),
        ];
        let lines = hint_lines(&hosts);
        assert!(lines[0].contains("--scope user"));
        assert!(lines[1].contains("--scope user"));
        assert!(lines[2].contains("--scope user"));
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
