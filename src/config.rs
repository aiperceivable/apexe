use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Global apexe configuration.
///
/// Resolution priority: CLI flags > env vars > config file > defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApexeConfig {
    pub modules_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub audit_log: PathBuf,
    pub log_level: String,
    pub default_timeout: u64,
    pub scan_depth: u32,
    pub json_output_preference: bool,
}

impl Default for ApexeConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let apexe_dir = home.join(".apexe");
        Self {
            modules_dir: apexe_dir.join("modules"),
            cache_dir: apexe_dir.join("cache"),
            config_dir: apexe_dir.clone(),
            audit_log: apexe_dir.join("audit.jsonl"),
            log_level: "info".to_string(),
            default_timeout: 30,
            scan_depth: 2,
            json_output_preference: true,
        }
    }
}

impl ApexeConfig {
    /// Create all required directories if they do not exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.modules_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.config_dir)?;
        Ok(())
    }
}

/// Load configuration with three-tier resolution.
///
/// 1. Start with defaults
/// 2. If config file exists, parse YAML and override matching fields
/// 3. Check env vars (APEXE_MODULES_DIR, APEXE_CACHE_DIR, APEXE_LOG_LEVEL,
///    APEXE_TIMEOUT) and override matching fields
/// 4. Apply cli_overrides
/// 5. Return ApexeConfig
pub fn load_config(
    config_path: Option<&Path>,
    cli_overrides: Option<&std::collections::HashMap<String, String>>,
) -> anyhow::Result<ApexeConfig> {
    let mut config = ApexeConfig::default();

    // Load from config file
    let file_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("config.yaml"));

    if file_path.exists() {
        let contents = std::fs::read_to_string(&file_path)?;
        match serde_yaml::from_str::<ApexeConfig>(&contents) {
            Ok(file_config) => config = file_config,
            Err(e) => warn!(
                path = %file_path.display(),
                "Malformed config file, using defaults: {e}"
            ),
        }
    }

    // Override from env vars
    if let Ok(val) = std::env::var("APEXE_MODULES_DIR") {
        config.modules_dir = PathBuf::from(val);
    }
    if let Ok(val) = std::env::var("APEXE_CACHE_DIR") {
        config.cache_dir = PathBuf::from(val);
    }
    if let Ok(val) = std::env::var("APEXE_LOG_LEVEL") {
        config.log_level = val;
    }
    if let Ok(val) = std::env::var("APEXE_TIMEOUT") {
        match val.parse::<u64>() {
            Ok(t) => config.default_timeout = t,
            Err(_) => warn!("Invalid APEXE_TIMEOUT value: {val}, using default"),
        }
    }

    // Apply CLI overrides
    if let Some(overrides) = cli_overrides {
        if let Some(val) = overrides.get("modules_dir") {
            config.modules_dir = PathBuf::from(val);
        }
        if let Some(val) = overrides.get("log_level") {
            config.log_level = val.clone();
        }
        if let Some(val) = overrides.get("scan_depth") {
            if let Ok(d) = val.parse::<u32>() {
                config.scan_depth = d;
            }
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn test_default_modules_dir_ends_with_apexe_modules() {
        let config = ApexeConfig::default();
        assert!(
            config.modules_dir.ends_with(".apexe/modules"),
            "modules_dir should end with .apexe/modules, got: {:?}",
            config.modules_dir
        );
    }

    #[test]
    fn test_default_log_level_is_info() {
        let config = ApexeConfig::default();
        assert_eq!(config.log_level, "info");
    }

    #[test]
    fn test_default_timeout_is_30() {
        let config = ApexeConfig::default();
        assert_eq!(config.default_timeout, 30);
    }

    #[test]
    fn test_default_scan_depth_is_2() {
        let config = ApexeConfig::default();
        assert_eq!(config.scan_depth, 2);
    }

    #[test]
    fn test_default_json_output_preference_is_true() {
        let config = ApexeConfig::default();
        assert!(config.json_output_preference);
    }

    #[test]
    fn test_load_config_no_file_returns_defaults() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        assert_eq!(config.log_level, "info");
        assert_eq!(config.default_timeout, 30);
    }

    #[test]
    fn test_load_config_valid_yaml() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.yaml");
        let default = ApexeConfig {
            modules_dir: tmp.path().join("my_modules"),
            cache_dir: tmp.path().join("my_cache"),
            config_dir: tmp.path().to_path_buf(),
            audit_log: tmp.path().join("audit.jsonl"),
            log_level: "debug".to_string(),
            default_timeout: 60,
            scan_depth: 3,
            json_output_preference: false,
        };
        let yaml = serde_yaml::to_string(&default).unwrap();
        std::fs::write(&config_path, &yaml).unwrap();

        let config = load_config(Some(config_path.as_path()), None).unwrap();
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.default_timeout, 60);
        assert_eq!(config.scan_depth, 3);
        assert!(!config.json_output_preference);
    }

    #[test]
    fn test_load_config_malformed_yaml_returns_defaults() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(&config_path, "this is not: [valid: yaml: config").unwrap();

        let config = load_config(Some(config_path.as_path()), None).unwrap();
        // Should fall back to defaults
        assert_eq!(config.log_level, "info");
        assert_eq!(config.default_timeout, 30);
    }

    #[test]
    fn test_env_var_override_modules_dir() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        // Use a unique env var approach: set, load, unset
        let unique_dir = "/tmp/apexe_test_modules_dir_unique";
        unsafe { std::env::set_var("APEXE_MODULES_DIR", unique_dir) };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_MODULES_DIR") };

        assert_eq!(config.modules_dir, PathBuf::from(unique_dir));
    }

    #[test]
    fn test_env_var_override_cache_dir() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let unique_dir = "/tmp/apexe_test_cache_dir_unique";
        unsafe { std::env::set_var("APEXE_CACHE_DIR", unique_dir) };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_CACHE_DIR") };

        assert_eq!(config.cache_dir, PathBuf::from(unique_dir));
    }

    #[test]
    fn test_env_var_override_log_level() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_LOG_LEVEL", "trace") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_LOG_LEVEL") };

        assert_eq!(config.log_level, "trace");
    }

    #[test]
    fn test_env_var_override_timeout() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_TIMEOUT", "120") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_TIMEOUT") };

        assert_eq!(config.default_timeout, 120);
    }

    #[test]
    fn test_env_var_invalid_timeout_falls_back() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_TIMEOUT", "not_a_number") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_TIMEOUT") };

        assert_eq!(config.default_timeout, 30);
    }

    #[test]
    fn test_cli_overrides_take_priority() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let mut overrides = HashMap::new();
        overrides.insert("modules_dir".to_string(), "/cli/modules".to_string());
        overrides.insert("log_level".to_string(), "error".to_string());
        overrides.insert("scan_depth".to_string(), "5".to_string());

        let config = load_config(Some(config_path.as_path()), Some(&overrides)).unwrap();
        assert_eq!(config.modules_dir, PathBuf::from("/cli/modules"));
        assert_eq!(config.log_level, "error");
        assert_eq!(config.scan_depth, 5);
    }

    #[test]
    fn test_cli_overrides_beat_env_vars() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_LOG_LEVEL", "debug") };

        let mut overrides = HashMap::new();
        overrides.insert("log_level".to_string(), "warn".to_string());

        let config = load_config(Some(config_path.as_path()), Some(&overrides)).unwrap();
        unsafe { std::env::remove_var("APEXE_LOG_LEVEL") };

        assert_eq!(config.log_level, "warn");
    }

    #[test]
    fn test_ensure_dirs_creates_directories() {
        let tmp = TempDir::new().unwrap();
        let config = ApexeConfig {
            modules_dir: tmp.path().join("m"),
            cache_dir: tmp.path().join("c"),
            config_dir: tmp.path().join("cfg"),
            ..ApexeConfig::default()
        };

        assert!(!config.modules_dir.exists());
        assert!(!config.cache_dir.exists());
        assert!(!config.config_dir.exists());

        config.ensure_dirs().unwrap();

        assert!(config.modules_dir.exists());
        assert!(config.cache_dir.exists());
        assert!(config.config_dir.exists());
    }

    #[test]
    fn test_ensure_dirs_idempotent() {
        let tmp = TempDir::new().unwrap();
        let config = ApexeConfig {
            modules_dir: tmp.path().join("m"),
            cache_dir: tmp.path().join("c"),
            config_dir: tmp.path().join("cfg"),
            ..ApexeConfig::default()
        };

        config.ensure_dirs().unwrap();
        // Call again -- should not error
        config.ensure_dirs().unwrap();

        assert!(config.modules_dir.exists());
        assert!(config.cache_dir.exists());
        assert!(config.config_dir.exists());
    }
}
