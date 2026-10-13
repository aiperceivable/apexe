use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::config::ApexeConfig;

/// Persistent updates to apexe's own configuration file.
#[derive(Debug, clap::Subcommand, Serialize, Deserialize)]
pub enum ConfigAction {
    /// Set a configuration value without replacing other settings.
    Set {
        /// Configuration key (log.level; log_level is also accepted).
        #[arg(value_parser = ["log.level", "log_level"])]
        key: String,
        /// Log level: off, error, warn, info, debug or trace.
        #[arg(value_parser = crate::config::parse_log_level)]
        value: String,
    },
}

impl ConfigAction {
    pub(super) fn execute(self, config: &ApexeConfig) -> anyhow::Result<()> {
        match self {
            Self::Set { key, value } => {
                anyhow::ensure!(
                    key == "log.level" || key == "log_level",
                    "Unsupported configuration key '{key}'"
                );
                crate::config::parse_log_level(&value).map_err(anyhow::Error::msg)?;
                set_log_level(&config.config_dir.join("config.yaml"), &value)
            }
        }
    }
}

fn set_log_level(path: &Path, level: &str) -> anyhow::Result<()> {
    let mut settings = if path.exists() {
        serde_yaml::from_str::<Mapping>(&std::fs::read_to_string(path)?)?
    } else {
        Mapping::new()
    };
    settings.insert(
        Value::String("log_level".into()),
        Value::String(level.into()),
    );
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Configuration path has no parent"))?;
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    if path.exists() {
        temporary
            .as_file()
            .set_permissions(std::fs::metadata(path)?.permissions())?;
    }
    temporary.write_all(serde_yaml::to_string(&settings)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_log_level_preserves_existing_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::fs::write(&path, "default_timeout: 99\ncustom: [one, two]\n").unwrap();
        set_log_level(&path, "debug").unwrap();
        let settings: Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(settings["log_level"], "debug");
        assert_eq!(settings["default_timeout"], 99);
        assert_eq!(settings["custom"][1], "two");
    }

    #[test]
    fn test_set_log_level_refuses_malformed_config_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        let contents = "log_level: [invalid";
        std::fs::write(&path, contents).unwrap();
        assert!(set_log_level(&path, "warn").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), contents);
    }
}
