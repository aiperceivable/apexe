# F7: Config Integration -- Integrate with apcore Config

| Field | Value |
|---|---|
| **Feature ID** | F7 |
| **Tech Design Section** | 5.8 |
| **Priority** | P2 (Polish) |
| **Dependencies** | F4 (MCP Server), F5 (Governance) |
| **Depended On By** | None (terminal feature) |
| **Modified Files** | `src/config.rs`, `src/cli/mod.rs` |
| **Deleted Files** | None |
| **Estimated LOC** | +50 (modifications) |
| **Estimated Tests** | ~10 (modify existing + add new) |

---

## 1. Purpose

Integrate apcore's `Config` system into `ApexeConfig` so that ecosystem-shared settings (log level, timeout, registry configuration) use the standard apcore configuration mechanism while apexe-specific settings (scan depth, cache directory, modules directory) remain on the apexe config struct. Also adopt apcore-cli's 4-tier `ConfigResolver` precedence: CLI flags > env vars > config file > defaults.

---

## 2. Current State (v0.1.x)

```rust
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
```

Resolution: defaults -> YAML file -> env vars -> CLI overrides (3-tier, manually implemented).

---

## 3. New Design

### 3.1 Updated ApexeConfig

```rust
// src/config.rs
use std::path::PathBuf;
use apcore::Config as CoreConfig;
use serde::{Deserialize, Serialize};

/// Global apexe configuration.
///
/// Resolution priority: CLI flags > env vars > config file > defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApexeConfig {
    // === apexe-specific settings ===

    /// Directory for binding YAML files.
    pub modules_dir: PathBuf,
    /// Directory for scanner cache files.
    pub cache_dir: PathBuf,
    /// Directory for configuration files.
    pub config_dir: PathBuf,
    /// Path to the audit log file.
    pub audit_log: PathBuf,
    /// Maximum subcommand recursion depth for scanning.
    pub scan_depth: u32,
    /// Prefer JSON output from CLI tools when available.
    pub json_output_preference: bool,

    // === Delegated to apcore Config ===

    /// Log level (trace, debug, info, warn, error).
    /// Synced with core_config on load.
    pub log_level: String,
    /// Default timeout for CLI subprocess execution (seconds).
    /// Synced with core_config on load.
    pub default_timeout: u64,

    // === apcore ecosystem config ===

    /// apcore core configuration for ecosystem integration.
    /// Contains registry settings, middleware config, etc.
    #[serde(skip)]
    pub core_config: Option<CoreConfig>,
}
```

### 3.2 Why `core_config` is `Option` and `#[serde(skip)]`

- `Option`: Allows graceful fallback when apcore Config cannot be loaded (e.g., missing apcore config file).
- `#[serde(skip)]`: The `CoreConfig` is loaded separately from the apcore config file, not serialized into apexe's config.yaml. This avoids coupling the two config formats.

### 3.3 Updated load_config()

```rust
pub fn load_config(
    config_path: Option<&Path>,
    cli_overrides: Option<&std::collections::HashMap<String, String>>,
) -> Result<ApexeConfig, ModuleError> {
    let mut config = ApexeConfig::default();

    // Step 1: Load from apexe config file (YAML)
    let file_path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| config.config_dir.join("config.yaml"));

    if file_path.exists() {
        let contents = std::fs::read_to_string(&file_path)
            .map_err(|e| ApexeError::Io(e))?;
        match serde_yaml::from_str::<ApexeConfig>(&contents) {
            Ok(file_config) => config = file_config,
            Err(e) => tracing::warn!(
                path = %file_path.display(),
                "Malformed config file, using defaults: {e}"
            ),
        }
    }

    // Step 2: Override from env vars (APEXE_* prefix)
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
            Err(_) => tracing::warn!("Invalid APEXE_TIMEOUT value: {val}, using default"),
        }
    }
    if let Ok(val) = std::env::var("APEXE_SCAN_DEPTH") {
        match val.parse::<u32>() {
            Ok(d) if (1..=5).contains(&d) => config.scan_depth = d,
            _ => tracing::warn!("Invalid APEXE_SCAN_DEPTH, using default"),
        }
    }

    // Step 3: Apply CLI overrides (highest priority)
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
        if let Some(val) = overrides.get("timeout") {
            if let Ok(t) = val.parse::<u64>() {
                config.default_timeout = t;
            }
        }
    }

    // Step 4: Load apcore CoreConfig (optional)
    let core_config_path = config.config_dir.join("apcore.yaml");
    if core_config_path.exists() {
        match CoreConfig::load(&core_config_path) {
            Ok(cc) => {
                // Sync shared settings: apexe settings take priority
                config.core_config = Some(cc);
            }
            Err(e) => tracing::warn!(
                path = %core_config_path.display(),
                "Failed to load apcore config: {e}"
            ),
        }
    }

    Ok(config)
}
```

### 3.4 Core Config Access

```rust
impl ApexeConfig {
    /// Get the apcore CoreConfig, creating a default if not loaded.
    pub fn core_config(&self) -> CoreConfig {
        self.core_config.clone().unwrap_or_default()
    }

    /// Create all required directories if they do not exist.
    pub fn ensure_dirs(&self) -> Result<(), ModuleError> {
        std::fs::create_dir_all(&self.modules_dir)
            .map_err(|e| ApexeError::Io(e))?;
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| ApexeError::Io(e))?;
        std::fs::create_dir_all(&self.config_dir)
            .map_err(|e| ApexeError::Io(e))?;
        Ok(())
    }
}
```

---

## 4. New CLI Flags

### 4.1 ScanArgs additions

```rust
#[derive(Debug, clap::Args)]
pub struct ScanArgs {
    // ... existing fields ...

    /// Verify output files after writing (run YAMLVerifier + SyntaxVerifier).
    #[arg(long)]
    pub verify: bool,

    /// Preview output without writing files.
    #[arg(long)]
    pub dry_run: bool,
}
```

### 4.2 Global flag additions

```rust
#[derive(Debug, Parser)]
#[command(name = "apexe", version, about)]
pub struct Cli {
    /// Log level (trace, debug, info, warn, error)
    #[arg(long, global = true, default_value = "info")]
    pub log_level: String,

    /// Override default timeout (seconds)
    #[arg(long, global = true)]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Commands,
}
```

---

## 5. Config File Locations

| File | Purpose | Created By |
|---|---|---|
| `~/.apexe/config.yaml` | apexe-specific settings | `apexe config --init` |
| `~/.apexe/apcore.yaml` | apcore ecosystem settings (optional) | Manual or `apcore config` |
| `~/.apexe/acl.yaml` | ACL rules | `apexe scan` (auto-generated) |
| `~/.apexe/audit.jsonl` | Audit log | `apexe serve` (auto-appended) |
| `~/.apexe/modules/*.binding.yaml` | Binding files | `apexe scan` |
| `~/.apexe/cache/` | Scanner cache files | `apexe scan` |

---

## 6. Environment Variable Mapping

| Variable | Config Field | Type | Default |
|---|---|---|---|
| `APEXE_MODULES_DIR` | `modules_dir` | PathBuf | `~/.apexe/modules` |
| `APEXE_CACHE_DIR` | `cache_dir` | PathBuf | `~/.apexe/cache` |
| `APEXE_LOG_LEVEL` | `log_level` | String | `"info"` |
| `APEXE_TIMEOUT` | `default_timeout` | u64 | `30` |
| `APEXE_SCAN_DEPTH` | `scan_depth` | u32 | `2` |

---

## 7. Test Scenarios

### 7.1 Existing Tests (Modified)

The 14 existing config tests are modified to account for the new `core_config` field:

| Test Name | Change |
|---|---|
| `test_default_modules_dir_ends_with_apexe_modules` | No change |
| `test_default_log_level_is_info` | No change |
| `test_default_timeout_is_30` | No change |
| `test_default_scan_depth_is_2` | No change |
| `test_default_json_output_preference_is_true` | No change |
| `test_load_config_no_file_returns_defaults` | Verify core_config is None |
| `test_load_config_valid_yaml` | No change (core_config is skip) |
| `test_load_config_malformed_yaml_returns_defaults` | No change |
| All env var tests | No change |
| `test_cli_overrides_take_priority` | Add timeout override test |
| `test_ensure_dirs_creates_directories` | Returns ModuleError now |

### 7.2 New Tests

| Test Name | Scenario | Expected |
|---|---|---|
| `test_core_config_loaded_when_file_exists` | apcore.yaml present | core_config is Some |
| `test_core_config_none_when_file_missing` | No apcore.yaml | core_config is None |
| `test_core_config_accessor_returns_default` | No apcore.yaml | core_config() returns default CoreConfig |
| `test_env_var_scan_depth_override` | APEXE_SCAN_DEPTH=3 | scan_depth = 3 |
| `test_env_var_scan_depth_invalid_range` | APEXE_SCAN_DEPTH=10 | scan_depth = 2 (default) |
| `test_cli_timeout_override` | timeout=60 in overrides | default_timeout = 60 |
| `test_scan_args_verify_flag` | --verify | args.verify = true |
| `test_scan_args_dry_run_flag` | --dry-run | args.dry_run = true |
| `test_global_timeout_flag` | --timeout 120 | cli.timeout = Some(120) |
| `test_ensure_dirs_returns_module_error` | Invalid path | Err(ModuleError) |

---

## 8. Migration Notes

### Backward Compatibility

- Existing `~/.apexe/config.yaml` files are fully backward compatible. The `core_config` field is `#[serde(skip)]` so it is never read from or written to the apexe config file.
- All existing environment variables continue to work.
- The `apexe config --show` output adds no new fields (core_config is skipped).
- The `apexe config --init` output is unchanged.

### New Capability

- `APEXE_SCAN_DEPTH` environment variable (was only available via CLI override before).
- `--timeout` global flag (was only available via env var before).
- `--verify` and `--dry-run` flags on `apexe scan`.
- Optional `~/.apexe/apcore.yaml` for ecosystem-wide settings.

### Return Type Change

`load_config()` return type changes from `anyhow::Result<ApexeConfig>` to `Result<ApexeConfig, ModuleError>`. This requires F6 (Error Migration) to be complete so that `ApexeError::Io` and `ApexeError::Yaml` can convert to `ModuleError`.

`ensure_dirs()` return type changes from `std::io::Result<()>` to `Result<(), ModuleError>`.
