#![cfg(unix)]
use assert_cmd::Command;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    repository: PathBuf,
    home: PathBuf,
    binary: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("rules");
        let home = directory.path().join("home");
        let binary = directory.path().join("toy");
        std::fs::create_dir_all(repository.join("overlays")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        executable(&binary, "case \"$1\" in --version) echo 'toy 1.0';; --ready) echo ready;; *) printf 'Usage: toy [OPTIONS]\\nOptions:\\n  --ready  Test operation\\n';; esac");
        let fixture = Self {
            directory,
            repository,
            home,
            binary,
        };
        fixture.git(&["init", "--quiet"]);
        fixture.overlay("toy", "unknown");
        fixture.overlay("missing-apexe-init-test", "unknown");
        fixture.commit();
        fixture
    }
    fn command(&self) -> Command {
        Command::from(self.process())
    }
    fn process(&self) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_apexe"));
        command
            .env("HOME", &self.home)
            .env("RUST_LOG", "error")
            .env("APEXE_DEFAULT_OVERLAY_REPOSITORY", &self.repository)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.directory.path().display()),
            )
            .env_remove("APEXE_OFFLINE")
            .env_remove("APEXE_OVERLAY_DIRS")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env_remove("APEXE_SHIM_DIR")
            .current_dir(&self.home);
        command
    }
    fn git(&self, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repository)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "-m", "Test rules"]);
    }
    fn overlay(&self, command: &str, variant: &str) {
        let overlay = json!({"schema_version":"1.1","command":command,"variant":variant,
            "mode":"authoritative","confidence":"low","cross_flag_ordering":false,
            "short_flag_bundling":false,"streaming_stdin_safe":false,
            "flags":[{"long":"--ready","type":"boolean","description":"Test operation"}],
            "annotations":{"readonly":true,"destructive":false,"idempotent":true,"requires_approval":false,"open_world":false}});
        std::fs::write(
            self.repository.join(format!("overlays/{command}.json")),
            serde_json::to_vec(&overlay).unwrap(),
        )
        .unwrap();
    }
    fn init(&self, args: &[&str]) -> Value {
        let output = self
            .command()
            .args(["init", "--format", "json"])
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }
    fn binding(&self) -> PathBuf {
        self.home.join(".apexe/bindings/cli.toy.binding.yaml")
    }
}
fn executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{contents}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn test_init_table_shows_ordered_progress_without_enabling_logs() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["init", "--log-level", "off"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    let stages = [
        "[1/3] Preparing CLI rules...",
        "[2/3] Scanning 2 candidate tool(s)...",
        "[1/2] Checking missing-apexe-init-test...",
        "[2/2] Checking toy...",
        "[3/3] Saving bindings and ACL...",
        "Initialized 1 binding(s)",
    ];
    let mut remaining = output.as_str();
    for stage in stages {
        let position = remaining
            .find(stage)
            .unwrap_or_else(|| panic!("Missing {stage}: {output}"));
        remaining = &remaining[position + stage.len()..];
    }
}

#[test]
fn test_init_json_suppresses_progress_and_keeps_a_single_report() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["init", "--format", "json", "--log-level", "off"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["written"], json!(["cli.toy"]));
    assert!(!String::from_utf8(output).unwrap().contains("[1/3]"));
}

#[tokio::test]
async fn test_init_flushes_progress_before_a_blocked_tool_scan_completes() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::time::{timeout, Duration};
    let fixture = Fixture::new();
    executable(&fixture.binary, "case \"$1\" in --version) echo 'toy 1.0';; *) while [ ! -f scan-release ]; do /bin/sleep 0.01; done; printf 'Usage: toy [OPTIONS]\\nOptions:\\n  --ready  Test operation\\n';; esac");
    let mut command = tokio::process::Command::from(fixture.process());
    let mut child = command
        .args(["init", "--log-level", "off"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first, "[1/3] Preparing CLI rules...");
    wait_for_tool_progress(&mut lines).await;
    assert!(child.try_wait().unwrap().is_none());
    std::fs::write(fixture.home.join("scan-release"), "").unwrap();
    let mut completed = false;
    timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            completed |= line.contains("Initialized 1 binding(s)");
        }
        assert!(child.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(completed);
}

async fn wait_for_tool_progress(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.contains("Checking toy...") {
                return;
            }
        }
        panic!("Tool progress was not emitted before scanning");
    })
    .await
    .unwrap();
}

#[test]
fn test_init_generates_bindings_acl_and_a_working_exec_without_scan() {
    let fixture = Fixture::new();
    let report = fixture.init(&[]);
    assert_eq!(report["degraded"], false);
    assert_eq!(report["written"], json!(["cli.toy"]));
    assert!(fixture.binding().exists());
    fixture
        .command()
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("cli.toy"));
    fixture
        .command()
        .args(["exec", "--", "toy", "--ready"])
        .assert()
        .success()
        .stdout("ready\n");
    assert!(!fixture.home.join(".profile").exists());
    assert!(!fixture.home.join(".apexe/bin/.apexe-shims.json").exists());
}

#[test]
fn test_init_is_idempotent_offline_and_preserves_custom_binding_and_policy() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    let mut modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    modules[0].metadata.remove("apexe_init_source");
    modules[0].description = "User-maintained binding".into();
    apexe::output::YamlOutput::without_verification()
        .write(&modules, fixture.binding().parent().unwrap(), false)
        .unwrap();
    let acl = fixture.home.join(".apexe/acl.yaml");
    std::fs::write(&acl, "rules:\n- callers: ['*']\n  targets: ['cli.toy']\n  effect: deny\n  description: Custom policy\ndefault_effect: allow\n").unwrap();
    let previous = std::fs::read(fixture.binding()).unwrap();
    std::fs::remove_dir_all(&fixture.repository).unwrap();
    let report = fixture.init(&["--offline"]);
    assert_eq!(report["written"], json!([]));
    assert_eq!(std::fs::read(fixture.binding()).unwrap(), previous);
    let policy = apexe::governance::AclManager::from_config(&acl).unwrap();
    assert_eq!(policy.explain("cli.toy").effect, "deny");
    assert_eq!(policy.explain("cli.other").effect, "allow");
}

#[test]
fn test_init_reports_absent_tools_and_variant_mismatches() {
    let fixture = Fixture::new();
    fixture.overlay("toy", "gnu");
    fixture.commit();
    let output = fixture
        .command()
        .args(["init", "--format", "json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert!(report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["status"] == "not_installed"));
    assert!(report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["status"] == "no_matching_overlay"));
    assert!(!fixture.binding().exists());
}

#[test]
fn test_init_explicit_source_and_pin_use_managed_committed_snapshot() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("APEXE_DEFAULT_OVERLAY_REPOSITORY", "/missing/default")
        .args(["init", "--overlays"])
        .arg(&fixture.repository)
        .args(["--ref", "HEAD", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert!(report["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| url::Url::parse(item["repository"].as_str().unwrap())
            .unwrap()
            .to_file_path()
            .unwrap()
            == fixture.repository.canonicalize().unwrap()));
    assert!(fixture.binding().exists());
}

#[test]
fn test_init_refresh_updates_managed_bindings_only() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    let mut overlay: Value = serde_json::from_slice(
        &std::fs::read(fixture.repository.join("overlays/toy.json")).unwrap(),
    )
    .unwrap();
    overlay["flags"][0]["long"] = json!("--changed");
    std::fs::write(
        fixture.repository.join("overlays/toy.json"),
        serde_json::to_vec(&overlay).unwrap(),
    )
    .unwrap();
    fixture.commit();
    fixture.init(&[]);
    let modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    assert!(modules[0].input_schema["properties"].get("ready").is_some());
    let refreshed = fixture.init(&["--refresh"]);
    assert_eq!(refreshed["written"], json!(["cli.toy"]), "{refreshed}");
    let modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    assert!(modules[0].input_schema["properties"]
        .get("changed")
        .is_some());
}

#[test]
fn test_init_refresh_preserves_unowned_bindings_and_configuration() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    let mut modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    modules[0].metadata.remove("apexe_init_source");
    apexe::output::YamlOutput::without_verification()
        .write(&modules, fixture.binding().parent().unwrap(), false)
        .unwrap();
    let previous = std::fs::read(fixture.binding()).unwrap();
    let config = fixture.home.join(".apexe/config.yaml");
    std::fs::write(&config, "log_level: warn\n").unwrap();
    fixture.init(&["--refresh"]);
    assert_eq!(std::fs::read(fixture.binding()).unwrap(), previous);
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "log_level: warn\n"
    );
}

#[test]
fn test_init_explicit_invalid_source_fails_without_fallback() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["init", "--overlays", "/missing/rules"])
        .assert()
        .failure();
    assert!(!fixture.binding().exists());
    fixture
        .command()
        .args(["init", "--overlays", "ssh://git@example.invalid/rules"])
        .assert()
        .failure();
}

#[test]
fn test_init_default_source_failure_reports_conservative_fallback() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("APEXE_DEFAULT_OVERLAY_REPOSITORY", "/missing/rules")
        .args(["init", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["degraded"], true);
    assert!(report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning.as_str().unwrap().contains("fallback")));
}

#[test]
fn test_init_offline_uses_configured_overlay_directory() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("APEXE_OVERLAY_DIRS", fixture.repository.join("overlays"))
        .args(["init", "--offline", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["degraded"], false);
    assert!(fixture.binding().exists());
    assert!(!fixture.home.join(".apexe/corpora").exists());
}

#[test]
fn test_init_bad_acl_fails_before_writing_bindings() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.home.join(".apexe")).unwrap();
    std::fs::write(fixture.home.join(".apexe/acl.yaml"), "invalid: [").unwrap();
    fixture.command().args(["init"]).assert().failure();
    assert!(!fixture.binding().exists());
}

#[test]
fn test_init_uses_clean_path_while_shims_are_active() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "sh"])
        .assert()
        .success();
    let shims = fixture.home.join(".apexe/bin");
    fixture
        .command()
        .env("APEXE_SHIM_DIR", &shims)
        .env(
            "PATH",
            format!(
                "{}:{}:/usr/bin:/bin",
                shims.display(),
                fixture.directory.path().display()
            ),
        )
        .args(["init", "--refresh"])
        .assert()
        .success();
    let modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    assert_eq!(
        modules[0].target,
        format!("exec://{}", fixture.binary.display())
    );
}

#[test]
fn test_init_replaces_overlays_command_and_rejects_blank_sources() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["overlays", "list"])
        .assert()
        .code(2);
    fixture
        .command()
        .args(["init", "--overlays", " "])
        .assert()
        .code(2);
}

#[test]
fn test_init_refresh_preserves_an_edited_binding_with_ownership_marker() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    let mut modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    modules[0].description = "Edited description".into();
    apexe::output::YamlOutput::without_verification()
        .write(&modules, fixture.binding().parent().unwrap(), false)
        .unwrap();
    let previous = std::fs::read(fixture.binding()).unwrap();
    let report = fixture.init(&["--refresh"]);
    assert_eq!(report["written"], json!([]));
    assert_eq!(std::fs::read(fixture.binding()).unwrap(), previous);
}

#[test]
fn test_init_recreates_missing_acl_without_rewriting_bindings() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    let previous = std::fs::read(fixture.binding()).unwrap();
    let acl = fixture.home.join(".apexe/acl.yaml");
    std::fs::remove_file(&acl).unwrap();
    fixture.init(&[]);
    assert_eq!(std::fs::read(fixture.binding()).unwrap(), previous);
    let policy = apexe::governance::AclManager::from_config(&acl).unwrap();
    assert_eq!(policy.explain("cli.toy").effect, "allow");
}

#[test]
fn test_init_failed_refresh_retains_snapshot_bindings_and_acl() {
    let fixture = Fixture::new();
    let initial = fixture.init(&[]);
    let binding = std::fs::read(fixture.binding()).unwrap();
    let acl = std::fs::read(fixture.home.join(".apexe/acl.yaml")).unwrap();
    std::fs::write(fixture.repository.join("overlays/toy.json"), "invalid").unwrap();
    fixture.commit();
    fixture
        .command()
        .args(["init", "--refresh"])
        .assert()
        .failure();
    assert_eq!(std::fs::read(fixture.binding()).unwrap(), binding);
    assert_eq!(
        std::fs::read(fixture.home.join(".apexe/acl.yaml")).unwrap(),
        acl
    );
    let after = fixture.init(&[]);
    assert_eq!(
        after["corpora"][0]["commit"],
        initial["corpora"][0]["commit"]
    );
}

#[test]
fn test_init_offline_default_falls_back_and_explicit_remote_fails() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env(
            "APEXE_DEFAULT_OVERLAY_REPOSITORY",
            "https://example.invalid/rules",
        )
        .args(["init", "--offline", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["degraded"], true);
    assert_eq!(report["native_ready"], json!([]));
    assert!(!fixture.home.join(".apexe/corpora").exists());
    fixture
        .command()
        .args([
            "init",
            "--offline",
            "--overlays",
            "https://example.invalid/rules",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("disabled"));
}

#[test]
fn test_init_reports_scan_failure_and_nonzero_exit() {
    let fixture = Fixture::new();
    executable(
        &fixture.binary,
        "case \"$1\" in --version) echo 'toy 1.0';; *) exec /bin/sleep 5;; esac",
    );
    let output = fixture
        .command()
        .args(["init", "--timeout", "1", "--format", "json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert!(report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["status"] == "failed"));
    assert!(!fixture.binding().exists());
}

#[test]
fn test_init_changed_default_source_does_not_reuse_old_named_corpus() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repository.join("manifest.json"),
        r#"{"schema_version":"1.0","name":"cli-permissions","description":"Test source","version":"0.1.0","overlay_directory":"overlays","license":"Apache-2.0"}"#).unwrap();
    fixture.commit();
    fixture.init(&[]);
    let report = fixture
        .command()
        .env(
            "APEXE_DEFAULT_OVERLAY_REPOSITORY",
            "https://example.invalid/new-rules",
        )
        .args(["init", "--offline", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&report).unwrap();
    assert_eq!(report["degraded"], true);
    assert!(report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning.as_str().unwrap().contains("disabled")));
    fixture
        .command()
        .env(
            "APEXE_DEFAULT_OVERLAY_REPOSITORY",
            "https://example.invalid/new-rules",
        )
        .args(["init", "--offline", "--refresh"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("disabled"));
}

#[test]
fn test_init_pattern_excluded_custom_binding_is_preserved() {
    let fixture = Fixture::new();
    fixture.init(&[]);
    let mut modules =
        apexe::output::load_modules_from_dir(fixture.binding().parent().unwrap(), None).unwrap();
    modules[0].metadata.remove("apexe_init_source");
    modules[0].description = "Custom binding outside the selected pattern".into();
    apexe::output::YamlOutput::without_verification()
        .write(&modules, fixture.binding().parent().unwrap(), false)
        .unwrap();
    let previous = std::fs::read(fixture.binding()).unwrap();
    std::fs::write(
        fixture.home.join(".apexe/apcore.yaml"),
        "version: \"1.0\"\nproject:\n  name: apexe-test\nbindings:\n  pattern: '*.custom.yaml'\n",
    )
    .unwrap();
    let report = fixture.init(&[]);
    assert_eq!(std::fs::read(fixture.binding()).unwrap(), previous);
    assert_eq!(report["preserved"], json!(["cli.toy"]));
    assert_eq!(report["degraded"], true);
    assert_eq!(report["native_ready"], json!([]));
}

#[test]
fn test_init_refuses_new_bindings_excluded_by_configured_pattern() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.home.join(".apexe")).unwrap();
    std::fs::write(
        fixture.home.join(".apexe/apcore.yaml"),
        "version: \"1.0\"\nproject:\n  name: apexe-test\nbindings:\n  pattern: '*.custom.yaml'\n",
    )
    .unwrap();
    fixture
        .command()
        .args(["init"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("bindings.pattern"));
    assert!(!fixture.binding().exists());
    assert!(!fixture.home.join(".apexe/acl.yaml").exists());
}
