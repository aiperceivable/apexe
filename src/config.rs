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

    /// Filesystem locations to deny *in addition to* the compiled-in baseline.
    ///
    /// Additive by construction: [`crate::governance::PathGuard::new`] starts
    /// from `BASELINE_DENIED_PATHS` and appends this, and there is no field
    /// that subtracts. An operator can extend the boundary to cover a data
    /// directory this deployment cares about; nobody can edit a YAML file to
    /// expose `/etc`.
    ///
    /// Entries may be relative, in which case they resolve against the same
    /// working directory the guard resolves caller-supplied paths against.
    pub additional_denied_paths: Vec<PathBuf>,

    /// Carve-outs *out of* the compiled-in path-guard baselines.
    ///
    /// **Empty by default, and the only setting that relaxes the guard.**
    /// Everything else in this file can tighten the boundary and nothing else
    /// can loosen it; this can, which makes it the one entry an operator owns
    /// the consequences of.
    ///
    /// It exists because the alternative was worse. An agent that legitimately
    /// has to write `/etc/nginx/conf.d` previously had no option but to be
    /// handed `nginx`-adjacent tooling outside apexe altogether — which drops
    /// the audit trail and the ACL along with the path check. A narrow,
    /// declared carve-out keeps the call inside the governed path.
    ///
    /// Nothing validates that an entry is *wise*: naming `/etc`, or a
    /// credential directory, is honoured. What the guard does instead is log
    /// every carve-out at startup, and warn on the ones that open a whole
    /// system location or expose credentials. Prefer the narrowest subtree that
    /// does the job — `/etc/nginx/conf.d`, not `/etc`.
    ///
    /// `~/.config` needs no entry here; it is not guarded in the first place.
    pub allowed_paths: Vec<PathBuf>,

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
            additional_denied_paths: Vec::new(),
            allowed_paths: Vec::new(),
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

/// Load configuration with two-tier resolution.
///
/// 1. Start with defaults
/// 2. If config file exists, parse YAML and merge it over the defaults
///    field by field (a field the file omits keeps its default -- see
///    `ApexeConfig`'s `#[serde(default)]`)
/// 3. Check env vars (APEXE_MODULES_DIR, APEXE_CACHE_DIR, APEXE_LOG_LEVEL,
///    APEXE_TIMEOUT) and override matching fields
/// 4. Return ApexeConfig
///
/// CLI-flag overrides are applied separately, after this returns: `--timeout`
/// via `ApexeConfig::with_timeout_override` (called from `Cli::run`),
/// `--scan-depth`/`--log-level`/`--modules-dir` via clap's own typed flags on
/// each subcommand. There is no third, map-based override mechanism here --
/// there used to be one (`apply_cli_overrides`, taking a
/// `HashMap<String, String>`), but every production caller passed `None` for
/// it; the typed flags above were, and remain, the only wired path.
pub fn load_config(config_path: Option<&Path>) -> anyhow::Result<ApexeConfig> {
    let mut config = ApexeConfig::default();

    let file_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("config.yaml"));
    apply_file_config(&mut config, &file_path)?;

    apply_env_overrides(&mut config);
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

    /// Both path-guard lists must survive a real `config.yaml`, and a file that
    /// mentions neither must still get empty ones rather than failing to parse.
    /// `allowed_paths` in particular is the one setting that widens what the
    /// process will permit, so a silent deserialization failure there would
    /// look exactly like a carve-out that does nothing.
    #[test]
    fn test_path_guard_lists_round_trip_through_config_yaml() {
        let configured: ApexeConfig = serde_yaml::from_str(
            "log_level: info\n\
             additional_denied_paths:\n\
             \x20 - /srv/production-data\n\
             allowed_paths:\n\
             \x20 - /etc/nginx/conf.d\n",
        )
        .expect("config with both path lists must parse");

        assert_eq!(
            configured.additional_denied_paths,
            vec![PathBuf::from("/srv/production-data")]
        );
        assert_eq!(
            configured.allowed_paths,
            vec![PathBuf::from("/etc/nginx/conf.d")]
        );
        // A field-level merge, so the rest of the file keeps its defaults.
        assert_eq!(configured.log_level, "info");
        assert_eq!(configured.scan_depth, ApexeConfig::default().scan_depth);

        let silent: ApexeConfig =
            serde_yaml::from_str("log_level: debug\n").expect("config without them must parse");
        assert!(silent.additional_denied_paths.is_empty());
        assert!(
            silent.allowed_paths.is_empty(),
            "carve-outs must be empty unless an operator writes them"
        );
    }

    use super::*;
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
        let config = load_config(Some(config_path.as_path())).unwrap();
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

        let config = load_config(Some(config_path.as_path())).unwrap();
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

        let config = load_config(Some(config_path.as_path())).unwrap();
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

        let config = load_config(Some(config_path.as_path())).unwrap();
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
        let config = load_config(Some(config_path.as_path())).unwrap();
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
        let config = load_config(Some(config_path.as_path())).unwrap();
        unsafe { std::env::remove_var("APEXE_CACHE_DIR") };

        assert_eq!(config.cache_dir, PathBuf::from(unique_dir));
    }

    #[test]
    fn test_env_var_override_log_level() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_LOG_LEVEL", "trace") };
        let config = load_config(Some(config_path.as_path())).unwrap();
        unsafe { std::env::remove_var("APEXE_LOG_LEVEL") };

        assert_eq!(config.log_level, "trace");
    }

    #[test]
    fn test_env_var_override_timeout() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_TIMEOUT", "120") };
        let config = load_config(Some(config_path.as_path())).unwrap();
        unsafe { std::env::remove_var("APEXE_TIMEOUT") };

        assert_eq!(config.default_timeout, 120);
    }

    #[test]
    fn test_env_var_invalid_timeout_falls_back() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_TIMEOUT", "not_a_number") };
        let config = load_config(Some(config_path.as_path())).unwrap();
        unsafe { std::env::remove_var("APEXE_TIMEOUT") };

        assert_eq!(config.default_timeout, 30);
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
        let config = load_config(Some(config_path.as_path())).unwrap();
        unsafe { std::env::remove_var("APEXE_SCAN_DEPTH") };

        assert_eq!(config.scan_depth, 3);
    }

    #[test]
    fn test_env_var_scan_depth_invalid_range() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");

        unsafe { std::env::set_var("APEXE_SCAN_DEPTH", "10") };
        let config = load_config(Some(config_path.as_path())).unwrap();
        unsafe { std::env::remove_var("APEXE_SCAN_DEPTH") };

        assert_eq!(config.scan_depth, 2); // default
    }

    #[test]
    fn test_core_config_none_when_file_missing() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("nonexistent.yaml");
        let config = load_config(Some(config_path.as_path())).unwrap();
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
}
