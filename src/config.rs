use std::path::{Path, PathBuf};

use apcore::{Config as CoreConfig, ModuleError};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Global apexe configuration.
///
/// Resolution priority: CLI flags > env vars > config file > defaults.
// `default` makes a field missing from config.yaml fall back to
// `ApexeConfig::default()` for that field instead of failing to parse the
// whole file -- required for the field-level merge `load_config` documents
// (a config.yaml that only sets e.g. `default_timeout` must not force every
// other field, like `modules_dir`, to also be spelled out).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ApexeConfig {
    pub modules_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    pub audit_log: PathBuf,
    pub log_level: String,
    pub default_timeout: u64,
    pub scan_depth: u32,
    pub json_output_preference: bool,

    /// apcore core configuration for ecosystem integration.
    #[serde(skip)]
    pub core_config: Option<CoreConfig>,
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
            core_config: None,
        }
    }
}

impl ApexeConfig {
    /// Get the apcore CoreConfig, creating a default if not loaded.
    pub fn core_config(&self) -> CoreConfig {
        self.core_config.clone().unwrap_or_default()
    }

    /// Apply a global `--timeout` override, if the operator gave one.
    ///
    /// A CLI flag outranks `config.yaml`, matching how `--log-level` resolves.
    pub fn with_timeout_override(mut self, timeout: Option<u64>) -> Self {
        if let Some(seconds) = timeout {
            self.default_timeout = seconds;
        }
        self
    }

    /// Create all required directories if they do not exist.
    ///
    /// Returns the crate-wide domain error rather than `std::io::Error`, so a
    /// failure here carries the same code, retryability and guidance every
    /// other apexe failure does — and so `Cli::run`, which is the only caller,
    /// reports it through `report_error` like anything else. `ApexeError::Io`
    /// keeps the underlying `ErrorKind` on the way through.
    #[allow(clippy::result_large_err)] // ModuleError is the crate-wide domain error
    pub fn ensure_dirs(&self) -> Result<(), ModuleError> {
        for dir in [&self.modules_dir, &self.cache_dir, &self.config_dir] {
            std::fs::create_dir_all(dir).map_err(|e| {
                let context = std::io::Error::new(
                    e.kind(),
                    format!("failed to create {}: {e}", dir.display()),
                );
                ModuleError::from(crate::errors::ApexeError::Io(context))
            })?;
        }
        Ok(())
    }
}

/// Load configuration with three-tier resolution.
///
/// 1. Start with defaults
/// 2. If config file exists, parse YAML and merge it over the defaults
///    field by field (a field the file omits keeps its default -- see
///    `ApexeConfig`'s `#[serde(default)]`)
/// 3. Check env vars (APEXE_MODULES_DIR, APEXE_CACHE_DIR, APEXE_LOG_LEVEL,
///    APEXE_TIMEOUT) and override matching fields
/// 4. Apply cli_overrides
/// 5. Return ApexeConfig
pub fn load_config(
    config_path: Option<&Path>,
    cli_overrides: Option<&std::collections::HashMap<String, String>>,
) -> anyhow::Result<ApexeConfig> {
    let mut config = ApexeConfig::default();

    let file_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("config.yaml"));
    apply_file_config(&mut config, &file_path)?;

    apply_env_overrides(&mut config);
    apply_cli_overrides(&mut config, cli_overrides);
    load_core_config(&mut config);

    Ok(config)
}

/// Merge `file_path`'s YAML over `config`, field by field. A missing or
/// unreadable file is a no-op; a malformed file warns and leaves `config`
/// untouched rather than failing the whole load.
fn apply_file_config(config: &mut ApexeConfig, file_path: &Path) -> anyhow::Result<()> {
    if !file_path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(file_path)?;
    match serde_yaml::from_str::<ApexeConfig>(&contents) {
        Ok(file_config) => *config = file_config,
        Err(e) => warn!(
            path = %file_path.display(),
            "Malformed config file, using defaults: {e}"
        ),
    }
    Ok(())
}

/// Override `config` from `APEXE_*` environment variables, when set and valid.
fn apply_env_overrides(config: &mut ApexeConfig) {
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
    if let Ok(val) = std::env::var("APEXE_SCAN_DEPTH") {
        match val.parse::<u32>() {
            Ok(d) if (1..=5).contains(&d) => config.scan_depth = d,
            _ => warn!("Invalid APEXE_SCAN_DEPTH value, using default"),
        }
    }
}

/// Override `config` from range-validated CLI flag overrides, when present.
fn apply_cli_overrides(
    config: &mut ApexeConfig,
    cli_overrides: Option<&std::collections::HashMap<String, String>>,
) {
    let Some(overrides) = cli_overrides else {
        return;
    };
    if let Some(val) = overrides.get("modules_dir") {
        config.modules_dir = PathBuf::from(val);
    }
    if let Some(val) = overrides.get("log_level") {
        config.log_level = val.clone();
    }
    if let Some(val) = overrides.get("scan_depth") {
        if let Ok(d) = val.parse::<u32>() {
            if (1..=5).contains(&d) {
                config.scan_depth = d;
            } else {
                warn!("Invalid scan_depth override: {d}, must be 1-5");
            }
        }
    }
    if let Some(val) = overrides.get("timeout") {
        if let Ok(t) = val.parse::<u64>() {
            if t > 0 {
                config.default_timeout = t;
            } else {
                warn!("Invalid timeout override: {t}, must be > 0");
            }
        }
    }
}

/// Load the optional apcore `CoreConfig` from `<config_dir>/apcore.yaml`.
fn load_core_config(config: &mut ApexeConfig) {
    let core_config_path = config.config_dir.join("apcore.yaml");
    if !core_config_path.exists() {
        return;
    }
    match CoreConfig::load(&core_config_path) {
        Ok(cc) => config.core_config = Some(cc),
        Err(e) => warn!(
            path = %core_config_path.display(),
            "Failed to load apcore config: {e}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Global lock for tests that modify environment variables.
    /// Prevents parallel test execution from causing race conditions
    /// on shared process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        assert_eq!(config.log_level, "info");
        assert_eq!(config.default_timeout, 30);
    }

    #[test]
    fn test_load_config_valid_yaml() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
            ..ApexeConfig::default()
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
    fn test_load_config_partial_yaml_merges_over_defaults() {
        // Regression for the WARNING finding: load_config's doc promises a
        // field-level merge, but ApexeConfig had no #[serde(default)], so a
        // config.yaml that only sets one field failed to deserialize
        // (missing required fields) and silently fell back to *pure*
        // defaults, discarding the one field the user did set.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.yaml");
        std::fs::write(&config_path, "default_timeout: 99\n").unwrap();

        let config = load_config(Some(config_path.as_path()), None).unwrap();
        assert_eq!(
            config.default_timeout, 99,
            "the field set in the partial config.yaml must take effect"
        );
        assert!(
            config.modules_dir.ends_with(".apexe/modules"),
            "a field omitted from the partial config.yaml must keep its default, got: {:?}",
            config.modules_dir
        );
    }

    #[test]
    fn test_load_config_malformed_yaml_returns_defaults() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let unique_dir = "/tmp/apexe_test_modules_dir_unique";
        unsafe { std::env::set_var("APEXE_MODULES_DIR", unique_dir) };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_MODULES_DIR") };

        assert_eq!(config.modules_dir, PathBuf::from(unique_dir));
    }

    #[test]
    fn test_env_var_override_cache_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_LOG_LEVEL", "trace") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_LOG_LEVEL") };

        assert_eq!(config.log_level, "trace");
    }

    #[test]
    fn test_env_var_override_timeout() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_TIMEOUT", "120") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_TIMEOUT") };

        assert_eq!(config.default_timeout, 120);
    }

    #[test]
    fn test_env_var_invalid_timeout_falls_back() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_TIMEOUT", "not_a_number") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_TIMEOUT") };

        assert_eq!(config.default_timeout, 30);
    }

    #[test]
    fn test_cli_overrides_take_priority() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn test_env_var_scan_depth_override() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_SCAN_DEPTH", "3") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_SCAN_DEPTH") };

        assert_eq!(config.scan_depth, 3);
    }

    #[test]
    fn test_env_var_scan_depth_invalid_range() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_SCAN_DEPTH", "10") };
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        unsafe { std::env::remove_var("APEXE_SCAN_DEPTH") };

        assert_eq!(config.scan_depth, 2); // default
    }

    #[test]
    fn test_cli_timeout_override() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let mut overrides = HashMap::new();
        overrides.insert("timeout".to_string(), "60".to_string());

        let config = load_config(Some(config_path.as_path()), Some(&overrides)).unwrap();
        assert_eq!(config.default_timeout, 60);
    }

    #[test]
    fn test_core_config_none_when_file_missing() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");
        let config = load_config(Some(config_path.as_path()), None).unwrap();
        assert!(config.core_config.is_none());
    }

    #[test]
    fn test_core_config_accessor_returns_default() {
        let config = ApexeConfig::default();
        let core = config.core_config();
        // CoreConfig::default() should have reasonable defaults
        assert!(core.executor.max_call_depth > 0);
    }

    #[test]
    fn test_ensure_dirs_reports_a_domain_error_not_a_bare_io_error() {
        // The return type is `Result<(), ModuleError>` so `Cli::run`'s failure
        // path renders this like any other apexe error — with a code and, where
        // one exists, guidance — rather than as a bare io::Error string. The
        // path is unusable because a *file* stands where a directory must go,
        // which `create_dir_all` cannot resolve.
        let tmp = tempfile::TempDir::new().unwrap();
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();

        let config = ApexeConfig {
            modules_dir: blocker.join("modules"),
            cache_dir: tmp.path().join("cache"),
            config_dir: tmp.path().join("config"),
            ..ApexeConfig::default()
        };

        let err = config
            .ensure_dirs()
            .expect_err("a file where a directory must go cannot be created");
        assert_eq!(err.code, apcore::ErrorCode::GeneralInternalError);
        assert!(
            err.message.contains("not-a-dir"),
            "the message must name the path that could not be created: {}",
            err.message
        );
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

    #[test]
    fn test_cli_scan_depth_override_invalid_range_ignored() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let mut overrides = HashMap::new();
        overrides.insert("scan_depth".to_string(), "10".to_string());

        let config = load_config(Some(config_path.as_path()), Some(&overrides)).unwrap();
        assert_eq!(config.scan_depth, 2); // default, override rejected
    }

    #[test]
    fn test_cli_scan_depth_override_zero_rejected() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let mut overrides = HashMap::new();
        overrides.insert("scan_depth".to_string(), "0".to_string());

        let config = load_config(Some(config_path.as_path()), Some(&overrides)).unwrap();
        assert_eq!(config.scan_depth, 2); // default
    }

    #[test]
    fn test_cli_timeout_override_zero_rejected() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        let mut overrides = HashMap::new();
        overrides.insert("timeout".to_string(), "0".to_string());

        let config = load_config(Some(config_path.as_path()), Some(&overrides)).unwrap();
        assert_eq!(config.default_timeout, 30); // default, override rejected
    }
}
