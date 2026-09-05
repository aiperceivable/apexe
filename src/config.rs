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

    /// Overlay directories to read *in addition to* `<config_dir>/overlays`.
    ///
    /// Additive by construction, like [`Self::additional_denied_paths`]: the
    /// personal directory is always read and there is no setting that removes
    /// it. This exists so a corpus someone else maintains — a team policy
    /// repository, a plugin that ships overlays, a checked-out data set — can
    /// be consumed without copying files into a directory apexe also treats as
    /// the operator's own scratch space.
    ///
    /// **Order is load order, and later wins.** Entries are read in the order
    /// listed, then `<config_dir>/overlays` is read last, so a hand-written
    /// local file shadows a distributed corpus that describes the same
    /// (command, variant). The rule follows the one already governing
    /// built-in vs user overlays: the source closer to the operator ranks
    /// higher. It does not override `--overlay`, which still wins outright.
    ///
    /// A listed directory that does not exist is a warning rather than an
    /// error — a corpus may legitimately not be installed yet — but it is not
    /// silent, because a typo here is otherwise indistinguishable from an
    /// empty directory.
    pub overlay_dirs: Vec<PathBuf>,

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
            overlay_dirs: Vec::new(),
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

/// The config keys whose silent loss widens what the process will permit.
///
/// Every unrecognised key is reported; these two additionally say what was
/// lost, because dropping them is fail-open: an operator who misspells
/// `additional_denied_paths` believes a location is blocked when only the
/// path-guard baseline is in force.
const PATH_GUARD_KEYS: &[&str] = &["additional_denied_paths", "allowed_paths"];

/// The top-level `config.yaml` keys `ApexeConfig` actually reads.
///
/// Derived from the struct by serializing its default rather than hand-listed,
/// so the two cannot drift: a field added to `ApexeConfig` is recognised here
/// the moment it exists, and a `#[serde(skip)]` field (`core_config`) is
/// correctly absent because it is not a config key.
fn known_config_keys() -> Vec<String> {
    match serde_yaml::to_value(ApexeConfig::default()) {
        Ok(serde_yaml::Value::Mapping(map)) => map
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        // INVARIANT: `ApexeConfig` is a plain struct of serializable fields, so
        // it always serializes to a mapping. Returning an empty set degrades to
        // "recognise nothing", which suppresses the warning rather than
        // producing a false one.
        _ => Vec::new(),
    }
}

/// Fold the config-key spellings that differ only in case or separator.
fn normalize_config_key(key: &str) -> String {
    key.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect()
}

/// The known key `unknown` was probably meant to be, if any.
///
/// Deliberately not a general edit distance, for the reason
/// `governance::acl::suggest_similar` gives: the spellings operators actually
/// reach for here are a separator or case change (`allowed-paths`) and a
/// singular/truncated form (`allowed_path`). Normalising separators and testing
/// for equality or a prefix relation names exactly those, without inventing a
/// match between two unrelated keys.
fn suggest_config_key(unknown: &str, known: &[String]) -> Option<String> {
    let needle = normalize_config_key(unknown);
    if needle.is_empty() {
        return None;
    }
    known.iter().find_map(|candidate| {
        let normalized = normalize_config_key(candidate);
        (normalized == needle || normalized.starts_with(&needle) || needle.starts_with(&normalized))
            .then(|| candidate.clone())
    })
}

/// Every top-level key in `contents` that `ApexeConfig` does not read, paired
/// with the key it was probably meant to be.
///
/// Split out from the warning so the detection is assertable without capturing
/// a tracing subscriber.
fn unrecognized_config_keys(contents: &str) -> Vec<(String, Option<String>)> {
    let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str::<serde_yaml::Value>(contents)
    else {
        // Not a mapping at all: `serde_yaml::from_str::<ApexeConfig>` below
        // reports that on its own, and guessing at keys here would double the
        // diagnostics for one fault.
        return Vec::new();
    };
    let known = known_config_keys();
    map.keys()
        .filter_map(|k| k.as_str())
        .filter(|k| !known.iter().any(|known_key| known_key == k))
        .map(|k| (k.to_string(), suggest_config_key(k, &known)))
        .collect()
}

/// Merge `file_path`'s YAML over `config`, field by field. A missing or
/// unreadable file is a no-op; a malformed file warns and leaves `config`
/// untouched rather than failing the whole load.
///
/// A key `ApexeConfig` does not read is reported before the merge. serde drops
/// it in silence — `ApexeConfig` is `#[serde(default)]` with no
/// `deny_unknown_fields` — and for `additional_denied_paths` that silence is
/// fail-open: an operator who misspells it believes a location is blocked when
/// only the path-guard baseline is in force. `deny_unknown_fields` is
/// deliberately NOT the fix, because a parse failure here degrades to
/// `ApexeConfig::default()`, so one stray key would discard the operator's
/// whole file — strictly worse than dropping the one key.
fn apply_file_config(config: &mut ApexeConfig, file_path: &Path) -> anyhow::Result<()> {
    if !file_path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(file_path)?;
    for (key, suggestion) in unrecognized_config_keys(&contents) {
        // The access clause is attached only where it is true. Appending it to
        // every unknown key would make it boilerplate an operator skims past,
        // on the one line where it has to land.
        let consequence = match suggestion.as_deref() {
            Some(known) if PATH_GUARD_KEYS.contains(&known) => {
                " That path-guard setting is NOT in force."
            }
            _ => "",
        };
        match suggestion {
            Some(known) => warn!(
                path = %file_path.display(),
                "Ignoring unrecognised config key '{key}' -- did you mean \
                 '{known}'? It has no effect.{consequence}"
            ),
            None => warn!(
                path = %file_path.display(),
                "Ignoring unrecognised config key '{key}'. It has no effect."
            ),
        }
    }
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
    if let Ok(val) = std::env::var("APEXE_OVERLAY_DIRS") {
        // `:`-separated like PATH. apexe does not support Windows (see the
        // platform table in README.md), so there is no `;` variant to handle.
        // Empty segments are dropped rather than resolving to the working
        // directory, which is what a stray leading or trailing `:` would
        // otherwise mean.
        config.overlay_dirs = val
            .split(':')
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .collect();
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

    /// The recognised key set is the struct's own, so a field cannot be added
    /// to `ApexeConfig` and then be reported as unrecognised.
    #[test]
    fn test_known_config_keys_match_the_struct_fields() {
        let keys = known_config_keys();
        for expected in [
            "modules_dir",
            "cache_dir",
            "config_dir",
            "audit_log",
            "log_level",
            "default_timeout",
            "scan_depth",
            "json_output_preference",
            "additional_denied_paths",
            "allowed_paths",
        ] {
            assert!(keys.iter().any(|k| k == expected), "missing {expected}");
        }
        assert!(
            !keys.iter().any(|k| k == "core_config"),
            "`core_config` is #[serde(skip)] and is not a config key"
        );
    }

    /// The reported case: a misspelled `additional_denied_paths` is dropped by
    /// serde in silence, and the drop is fail-open -- the operator believes a
    /// location is blocked when only the baseline is in force.
    #[test]
    fn test_a_misspelled_denied_paths_key_is_reported_not_dropped_silently() {
        let found = unrecognized_config_keys(
            "log_level: info\n\
             additional_denied_path:\n\
             \x20 - /srv/production-data\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "additional_denied_path");
        assert_eq!(
            found[0].1.as_deref(),
            Some("additional_denied_paths"),
            "the singular form must name the plural it was meant to be"
        );
    }

    /// A separator or case change is the other spelling operators reach for.
    #[test]
    fn test_a_separator_variant_is_matched_to_its_real_key() {
        let found = unrecognized_config_keys("allowed-paths:\n\x20 - /etc/nginx\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1.as_deref(), Some("allowed_paths"));
    }

    /// The path-guard consequence is stated only where it is true, so the
    /// sentence stays a signal rather than boilerplate on every line.
    #[test]
    fn test_path_guard_keys_are_the_two_that_widen_permission() {
        for key in PATH_GUARD_KEYS {
            assert!(
                known_config_keys().iter().any(|k| k == key),
                "{key} must be a real config key"
            );
        }
        assert_eq!(PATH_GUARD_KEYS.len(), 2);
    }

    /// A key with no plausible relative is still reported -- it just cannot be
    /// paired with a suggestion. Inventing one would be worse than none.
    #[test]
    fn test_an_unrelated_key_is_reported_without_a_suggestion() {
        let found = unrecognized_config_keys("telemetry_endpoint: https://example.test\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "telemetry_endpoint");
        assert_eq!(found[0].1, None);
    }

    /// A correct file must produce no diagnostics at all, or the warning
    /// becomes noise an operator learns to ignore.
    #[test]
    fn test_a_fully_valid_config_reports_nothing() {
        let found = unrecognized_config_keys(
            "log_level: debug\n\
             default_timeout: 30\n\
             allowed_paths:\n\
             \x20 - /etc/nginx\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    /// A non-mapping document is `apply_file_config`'s existing "malformed"
    /// path; reporting phantom keys here would double the diagnostics.
    #[test]
    fn test_a_non_mapping_document_reports_no_keys() {
        assert!(unrecognized_config_keys("- just\n- a\n- list\n").is_empty());
        assert!(unrecognized_config_keys("42\n").is_empty());
    }

    /// The suggestion must not pair two keys that merely share a prefix
    /// boundary by accident.
    #[test]
    fn test_suggestion_does_not_invent_a_match_between_unrelated_keys() {
        let known = known_config_keys();
        assert_eq!(suggest_config_key("zzz_nothing_like_it", &known), None);
    }

    /// Both path-guard lists must survive a real `config.yaml`, and a file that
    /// mentions neither must still get empty ones rather than failing to parse.
    /// `allowed_paths` in particular is the one setting that widens what the
    /// process will permit, so a silent deserialization failure there would
    /// look exactly like a carve-out that does nothing.
    /// `overlay_dirs` decides which corpora a scan can see at all, so a silent
    /// deserialization failure here looks exactly like a corpus that is
    /// installed but ignored.
    #[test]
    fn test_overlay_dirs_round_trip_through_config_yaml() {
        let configured: ApexeConfig = serde_yaml::from_str(
            "log_level: info\n\
             overlay_dirs:\n\
             \x20 - /opt/cli-facts/overlays\n\
             \x20 - ~/team-policy/overlays\n",
        )
        .expect("config with overlay_dirs must parse");

        assert_eq!(
            configured.overlay_dirs,
            vec![
                PathBuf::from("/opt/cli-facts/overlays"),
                PathBuf::from("~/team-policy/overlays"),
            ],
            "order is load order and later wins, so it must survive the round trip"
        );
    }

    /// The field is additive, and a config file that omits it must not turn
    /// into "no overlay directories at all".
    #[test]
    fn test_overlay_dirs_defaults_to_empty_when_absent() {
        let configured: ApexeConfig =
            serde_yaml::from_str("log_level: info\n").expect("must parse");
        assert!(configured.overlay_dirs.is_empty());
    }

    #[test]
    fn test_overlay_dirs_is_a_recognised_config_key() {
        assert!(
            known_config_keys().iter().any(|k| k == "overlay_dirs"),
            "an unrecognised key is warned about as a typo, which would be wrong here"
        );
    }

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
