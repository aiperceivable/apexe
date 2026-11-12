use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, ValueEnum, Serialize, Deserialize)]
pub enum Shell {
    #[default]
    Sh,
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl Shell {
    pub(super) fn detect() -> anyhow::Result<Self> {
        let shell = std::env::var_os("SHELL");
        Self::from_path(std::path::Path::new(shell.as_deref().unwrap_or_default()))
    }

    pub(super) fn from_path(path: &std::path::Path) -> anyhow::Result<Self> {
        match path.file_stem().and_then(|name| name.to_str()) {
            Some("sh" | "dash") => Ok(Self::Sh),
            Some("bash") => Ok(Self::Bash),
            Some("zsh") => Ok(Self::Zsh),
            Some("fish") => Ok(Self::Fish),
            Some("pwsh" | "powershell") => Ok(Self::Powershell),
            None if cfg!(windows) => Ok(Self::Powershell),
            _ => anyhow::bail!("Cannot detect shell; pass --shell and optionally --rc-file"),
        }
    }
}
