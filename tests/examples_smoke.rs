//! Execute the user-facing examples against the actual built CLI.
#![cfg(unix)]
use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

fn example(name: &str) -> Command {
    let python = which::which("python3").expect("example smoke tests require Python 3");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new(python);
    command
        .arg(root.join("examples").join(name).join("demo.py"))
        .args(["--apexe", env!("CARGO_BIN_EXE_apexe")])
        .current_dir(root)
        .timeout(Duration::from_secs(90));
    command
}

fn summary(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().clone();
    serde_json::from_slice(&output.stdout).expect("example must print its validated JSON summary")
}

#[test]
fn test_local_example_runs_captured_governance_and_policy_denial() {
    let result = summary(&mut example("local_execution"));
    assert_eq!(result["captured_stdout"], "one\ntwo\n");
    assert_eq!(result["preflight"], "allowed");
    assert_eq!(result["acl_denied"], true);
    assert_eq!(result["audited"], true);
    assert!(result["path_denied"].is_null() || result["path_denied"] == true);
    assert!(result["native"].is_null());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "Requires the reviewed BSD cat/head corpus; run with --ignored after installing it"]
fn test_local_example_runs_native_pipeline_and_owned_uninstall() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("cli-permissions/overlays");
    let result = summary(
        example("local_execution")
            .arg("--corpus")
            .arg(corpus)
            .arg("--shims"),
    );
    assert_eq!(result["path_denied"], true);
    assert_eq!(result["native"]["pipeline_stdout"], "one\n");
    assert_eq!(result["native"]["reloaded"], true);
    assert_eq!(result["native"]["startup_loaded"], true);
    assert_eq!(result["native"]["startup_removed"], true);
    assert_eq!(result["native"]["uninstalled"], true);
}

#[test]
fn test_openapi_example_imports_local_json_through_both_transports() {
    let result = summary(&mut example("openapi_import"));
    assert_eq!(result["upstream_calls"], 4);
    assert_eq!(result["spec_fetches"], 0);
    assert_eq!(result["surfaces"]["mcp"]["json_body"], true);
    assert_eq!(result["surfaces"]["a2a"]["authenticated"], true);
    assert_eq!(result["surfaces"]["mcp"]["delete_denied"], true);
    assert_eq!(result["surfaces"]["a2a"]["delete_denied"], true);
}

#[test]
fn test_openapi_example_keeps_remote_spec_credentials_separate() {
    let result = summary(example("openapi_import").arg("--remote-spec"));
    assert_eq!(result["spec_fetches"], 2);
    assert_eq!(result["upstream_calls"], 4);
    assert_eq!(result["spec_credentials_not_forwarded"], true);
    assert_eq!(result["surfaces"]["mcp"]["read"], true);
    assert_eq!(result["surfaces"]["a2a"]["read"], true);
}

#[test]
fn test_verification_example_checks_scoped_evidence_and_invalidation() {
    let result = summary(&mut example("verification"));
    assert_eq!(result["bindings_preserved"], true);
    if result["supported"] == false {
        assert!(result["reason"]
            .as_str()
            .unwrap()
            .contains("isolation unavailable"));
    } else {
        for field in [
            "dry_run",
            "isolated_effects",
            "case_file_invalidation",
            "binary_invalidation",
        ] {
            assert_eq!(result[field], true);
        }
        assert!(!result["backends"].as_array().unwrap().is_empty());
    }
}
