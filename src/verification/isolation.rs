use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    #[default]
    Auto,
    Required,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct IsolationPlan {
    pub launcher: PathBuf,
    pub arguments: Vec<String>,
    pub backend: String,
}

pub(super) fn prepare(
    root: &Path,
    executable: &Path,
    binary: &Path,
    readonly: bool,
) -> anyhow::Result<IsolationPlan> {
    #[cfg(target_os = "macos")]
    {
        let profile = root.join("verification.sb");
        let _ = (executable, binary);
        std::fs::write(&profile, macos_profile(root, readonly)?)?;
        Ok(IsolationPlan {
            launcher: "/usr/bin/sandbox-exec".into(),
            arguments: vec!["-f".into(), path_text(&profile)?],
            backend: "sandbox-exec".into(),
        })
    }
    #[cfg(target_os = "linux")]
    {
        let launcher = which::which_in(
            "bwrap",
            Some(crate::runtime_path::search_path()),
            std::env::current_dir()?,
        )?;
        Ok(IsolationPlan {
            launcher,
            arguments: linux_arguments(root, executable, binary, readonly)?,
            backend: "bubblewrap".into(),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (root, executable, binary, readonly);
        anyhow::bail!("No supported local verification isolation backend on this platform")
    }
}

impl IsolationPlan {
    pub fn command(&self, executable: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.launcher);
        command.args(&self.arguments).arg(executable);
        command
    }

    pub async fn probe(&self) -> anyhow::Result<()> {
        let mut command = self.command(Path::new("/bin/true"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .kill_on_drop(true);
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(5), command.output()).await??;
        anyhow::ensure!(
            output.status.success(),
            "Isolation unavailable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

fn path_text(path: &Path) -> anyhow::Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Isolation paths must be UTF-8"))?;
    anyhow::ensure!(
        !text.chars().any(char::is_control),
        "Isolation paths must not contain control characters"
    );
    Ok(text.into())
}

#[cfg(any(target_os = "macos", test))]
fn macos_profile(root: &Path, readonly: bool) -> anyhow::Result<String> {
    let writable = if readonly {
        root.join("home")
    } else {
        root.to_path_buf()
    };
    let writable = serde_json::to_string(&path_text(&writable.canonicalize()?)?)?;
    Ok(format!("(version 1)\n(deny default)\n(allow file-read*)\n(allow process-fork)\n(allow process-exec)\n(allow sysctl-read)\n(allow signal (target same-sandbox))\n(allow file-write* (subpath {writable}))\n"))
}

#[cfg(any(target_os = "linux", test))]
fn linux_arguments(
    root: &Path,
    executable: &Path,
    binary: &Path,
    readonly: bool,
) -> anyhow::Result<Vec<String>> {
    let mut arguments = vec![
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--cap-drop".into(),
        "ALL".into(),
    ];
    // Expose system binaries/libraries read-only, never host socket/runtime directories.
    for directory in ["/usr", "/bin", "/lib", "/lib64", "/etc"] {
        if Path::new(directory).exists() {
            arguments.extend(["--ro-bind".into(), directory.into(), directory.into()]);
        }
    }
    arguments.extend([
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
    ]);
    for binary in [executable, binary] {
        arguments.extend(["--ro-bind".into(), path_text(binary)?, path_text(binary)?]);
    }
    arguments.extend(["--bind".into(), path_text(root)?, path_text(root)?]);
    if readonly {
        arguments.extend([
            "--ro-bind".into(),
            path_text(&root.join("work"))?,
            path_text(&root.join("work"))?,
        ]);
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_profile_defaults_to_denial_and_only_allows_fixture_writes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let profile = macos_profile(root.path(), true).unwrap();
        assert!(profile.contains("(deny default)"));
        assert!(!profile.contains("allow network"));
        assert!(!profile.contains("allow mach-lookup"));
        assert_eq!(profile.matches("allow file-write*").count(), 1);
        assert!(profile.contains("target same-sandbox"));
    }

    #[test]
    fn test_linux_plan_is_private_and_never_mounts_the_host_root_or_sockets() {
        let root = tempfile::tempdir().unwrap();
        let arguments = linux_arguments(
            root.path(),
            Path::new("/usr/bin/apexe"),
            Path::new("/usr/bin/cat"),
            true,
        )
        .unwrap();
        assert!(arguments.contains(&"--unshare-all".into()));
        assert!(arguments.contains(&"--new-session".into()));
        assert!(!arguments
            .windows(3)
            .any(|parts| parts == ["--ro-bind", "/", "/"]));
        assert!(!arguments
            .iter()
            .any(|part| part == "/run" || part == "/var/run"));
        assert_eq!(arguments.iter().filter(|part| *part == "--bind").count(), 1);
    }
}
