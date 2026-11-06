use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::ApexeConfig;

const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;

/// Translate a host hook event into a governed execution request.
#[derive(Debug, clap::Args)]
#[command(
    after_help = "Reads one JSON event from stdin and writes the host's hook response to stdout.\n\nClaude Code: configure this command as a PreToolUse hook for Bash. Simple external commands are rewritten to `apexe sh -c '<command>'`; shell builtins, pipelines, redirections and compound commands are left unchanged and rely on the installed PATH shims."
)]
pub struct HookArgs {
    /// Hook protocol to serve.
    #[arg(value_enum)]
    pub host: HookHost,
    /// Route every Bash call through a persistent governed shell session.
    ///
    /// Experimental: the session preserves cwd, variables, functions and
    /// aliases across calls. It is started on demand and is scoped to the
    /// host session id and project directory.
    #[arg(long)]
    pub persistent: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum HookHost {
    Claude,
}

impl HookArgs {
    pub fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        match self.host {
            HookHost::Claude => claude(config, self.persistent),
        }
    }
}

fn claude(config: &ApexeConfig, persistent: bool) -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES + 1)
        .read_to_string(&mut input)?;
    anyhow::ensure!(
        input.len() as u64 <= MAX_HOOK_INPUT_BYTES,
        "hook input exceeds 1 MiB"
    );
    let event: Value = serde_json::from_str(&input)?;
    let tool_name = event
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(tool_name, "Bash" | "bash" | "Shell" | "shell") {
        return Ok(());
    }
    let Some(tool_input) = event.get("tool_input").and_then(Value::as_object) else {
        return Ok(());
    };
    let Some(command) = tool_input.get("command").and_then(Value::as_str) else {
        return Ok(());
    };
    if command.trim().is_empty() {
        return Ok(());
    }

    let executable = std::env::current_exe()?.canonicalize()?;
    if is_routed(command, &executable) {
        return Ok(());
    }
    let shim_dir = shim_directory(config);
    if !shim_dir.join(crate::runtime_path::MANIFEST_NAME).is_file() {
        return deny(
            "apexe shims are not installed for this environment; run 'apexe env --enable' \
             before relying on the hook",
        );
    }
    if persistent {
        let session_id = event
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let cwd = event
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        let socket = session_path(session_id, &cwd);
        let executable = executable
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("apexe executable path must be valid UTF-8"))?;
        let rewritten = format!(
            "APEXE_SESSION_SOCKET={} APEXE_SESSION_CWD={} {} sh -c {}",
            shell_words::quote(socket.to_str().unwrap_or_default()),
            shell_words::quote(cwd.to_str().unwrap_or_default()),
            shell_words::quote(executable),
            shell_words::quote(command)
        );
        return allow(
            tool_input,
            &rewritten,
            "Routed through the apexe persistent governed session",
        );
    }
    if !should_rewrite(command) {
        return Ok(());
    }

    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("apexe executable path must be valid UTF-8"))?;
    let rewritten = format!(
        "{} sh -c {}",
        shell_words::quote(executable),
        shell_words::quote(command)
    );
    allow(
        tool_input,
        &rewritten,
        "Routed through the apexe governed shell",
    )
}

fn allow(tool_input: &Map<String, Value>, command: &str, reason: &str) -> anyhow::Result<()> {
    let mut updated = tool_input.clone();
    updated.insert("command".into(), Value::String(command.to_string()));
    print_json(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": reason,
            "updatedInput": Value::Object(updated),
        }
    }))
}

fn session_path(session_id: &str, cwd: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut hasher);
    cwd.hash(&mut hasher);
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("apexe-session-{uid}-{:x}.sock", hasher.finish()))
}

fn shim_directory(config: &ApexeConfig) -> PathBuf {
    std::env::var_os("APEXE_SHIM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("bin"))
}

fn is_routed(command: &str, executable: &Path) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let Some(first) = tokens.first() else {
        return false;
    };
    let name = Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first);
    name == "apexe" || name == "apexe-sh" || executable.to_str().is_some_and(|path| first == path)
}

/// Rewrite only single simple commands.
///
/// Stateful builtins must stay in the host's persistent shell. Pipelines,
/// redirections, command substitution and compound commands may mix builtins
/// and externals, which a one-shot `apexe sh -c` would not preserve; those keep
/// relying on the installed PATH shims.
fn should_rewrite(command: &str) -> bool {
    if command.chars().any(|character| {
        matches!(
            character,
            '\n' | '\r' | '|' | '&' | ';' | '>' | '<' | '(' | ')' | '`'
        )
    }) || command.contains("$(")
    {
        return false;
    }
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    let Some(first) = tokens.first() else {
        return false;
    };
    !matches!(
        first.as_str(),
        "cd" | "export"
            | "unset"
            | "alias"
            | "unalias"
            | "set"
            | "shopt"
            | "hash"
            | "ulimit"
            | "umask"
            | "exec"
            | "source"
            | "."
            | "eval"
            | "read"
            | "exit"
            | "return"
            | "trap"
            | "command"
            | "builtin"
            | "type"
            | "which"
            | "echo"
            | "printf"
            | "pwd"
            | "true"
            | "false"
            | "test"
            | "["
            | ":"
            | "wait"
            | "jobs"
            | "fg"
            | "bg"
            | "kill"
            | "let"
            | "local"
            | "declare"
            | "typeset"
            | "readonly"
            | "shift"
            | "getopts"
            | "times"
            | "history"
            | "fc"
            | "disown"
            | "enable"
            | "help"
            | "bind"
            | "complete"
            | "compgen"
            | "dirs"
            | "pushd"
            | "popd"
            | "sudo"
            | "doas"
            | "su"
    )
}

fn deny(reason: &str) -> anyhow::Result<()> {
    print_json(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
}

fn print_json(value: Value) -> anyhow::Result<()> {
    use std::io::Write;

    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_external_command_is_rewritten() {
        assert!(should_rewrite("ls -la"));
        assert!(should_rewrite("sh -c 'ls -la'"));
    }

    #[test]
    fn test_stateful_and_compound_commands_are_left_alone() {
        for command in [
            "cd /tmp",
            "export FOO=bar",
            "ls | head",
            "ls && cat file",
            "echo hi > file",
            "echo $(date)",
            "set -e",
        ] {
            assert!(!should_rewrite(command), "{command}");
        }
    }

    #[test]
    fn test_routed_commands_are_not_rewritten_again() {
        let executable = Path::new("/opt/apexe/bin/apexe");
        for command in ["apexe sh -c 'ls'", "apexe-sh -c 'ls'", "apexe exec -- ls"] {
            assert!(is_routed(command, executable), "{command}");
        }
        assert!(!is_routed("ls -la", executable));
    }
}
