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
        let name = shell
            .as_deref()
            .and_then(|path| std::path::Path::new(path).file_stem())
            .and_then(|name| name.to_str());
        match name {
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
