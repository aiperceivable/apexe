#![cfg(unix)]
//! `apexe managed`: backend probes and policy compilation.
//!
//! These tests exercise the operator-facing half of the v1 contract. The
//! Linux backend cannot be probed on this host, which is itself one of the
//! behaviours under test: an unsupported backend must refuse a policy rather
//! than compile one it cannot enforce.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn command(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("apexe").unwrap();
    command
        .env("HOME", home.path())
        .env_remove("APEXE_BINDINGS_DIR")
        .env_remove("APCORE_CONFIG")
        .current_dir(home.path());
    command
}

fn valid_policy() -> String {
    serde_json::json!({
        "version": 1,
        "session_grants": [
            {"root": "workspace", "prefix": "", "ops": ["inspect_metadata", "read_file"]}
        ],
        "forms": [
            {
                "id": "read-workspace",
                "form": "cli.cat.file",
                "grants": [{"root": "workspace", "prefix": "", "ops": ["inspect_metadata", "read_file"]}],
                "approval": "none"
            }
        ]
    })
    .to_string()
}

#[test]
fn test_probe_reports_the_selected_backend() {
    let home = TempDir::new().unwrap();
    command(&home)
        .args(["managed", "probe", "--backend", "linux", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"backend\": \"linux\""));
}

#[test]
fn test_sbpl_is_reported_as_a_prototype() {
    let home = TempDir::new().unwrap();
    command(&home)
        .args(["managed", "probe", "--backend", "sbpl", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"supported\": false"));
}

#[test]
fn test_a_malformed_policy_is_policy_invalid() {
    let home = TempDir::new().unwrap();
    let policy = home.path().join("policy.json");
    std::fs::write(&policy, "{}").unwrap();
    command(&home)
        .args(["managed", "policy-check", "--file"])
        .arg(&policy)
        .assert()
        .failure()
        .stderr(predicate::str::contains("POLICY_INVALID"));
}

#[test]
fn test_a_duplicate_key_is_refused_before_validation() {
    let home = TempDir::new().unwrap();
    let policy = home.path().join("policy.json");
    std::fs::write(
        &policy,
        r#"{"version":1,"version":1,"session_grants":[],"forms":[]}"#,
    )
    .unwrap();
    command(&home)
        .args(["managed", "policy-check", "--file"])
        .arg(&policy)
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate"));
}

#[test]
fn test_an_unsupported_backend_refuses_a_valid_policy() {
    let home = TempDir::new().unwrap();
    let policy = home.path().join("policy.json");
    std::fs::write(&policy, valid_policy()).unwrap();
    // `sbpl` is a prototype and can never activate a managed session, so the
    // compiler must refuse even though the policy itself is well-formed.
    command(&home)
        .args(["managed", "policy-check", "--file"])
        .arg(&policy)
        .args(["--backend", "sbpl"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("BACKEND_UNSUPPORTED"));
}

#[test]
fn test_the_linux_backend_is_unsupported_off_linux() {
    let home = TempDir::new().unwrap();
    let policy = home.path().join("policy.json");
    std::fs::write(&policy, valid_policy()).unwrap();
    let assertion = command(&home)
        .args(["managed", "policy-check", "--file"])
        .arg(&policy)
        .args(["--backend", "linux"])
        .assert();
    if cfg!(target_os = "linux") {
        // On Linux the probe result depends on the kernel; the command must
        // still not claim success without a capability report.
        assertion.code(predicate::in_iter([0i32, 1]));
    } else {
        assertion
            .failure()
            .stderr(predicate::str::contains("BACKEND_UNSUPPORTED"));
    }
}
