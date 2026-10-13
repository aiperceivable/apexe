#![cfg(unix)]
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;

fn command(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("apexe").unwrap();
    command
        .env("APEXE_OFFLINE", "true")
        .env("HOME", home.path())
        .env_remove("APEXE_BINDINGS_DIR")
        .env_remove("APCORE_CONFIG")
        .env("APEXE_OVERLAY_DIRS", "")
        .env("RUST_LOG", "error")
        .current_dir(home.path());
    command
}

fn script(home: &TempDir, flag: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = home.path().join("drift-fixture");
    std::fs::write(&path, format!("#!/bin/sh\ncase \\\"$1\\\" in\n--help) printf 'Usage: drift-fixture [OPTIONS]\\nOptions:\\n  --{flag} TEXT  fixture value\\n';;\n--version) echo 'fixture 1.0';;\n*) exit 0;;\nesac\n").replace("\\\"", "\"")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn test_verify_detects_removed_source_flag_without_replacing_baseline() {
    let home = TempDir::new().unwrap();
    let binary = script(&home, "before");
    command(&home)
        .args(["scan", binary.to_str().unwrap(), "--no-cache"])
        .assert()
        .success();
    let binding = home
        .path()
        .join(".apexe/bindings/cli.drift_fixture.binding.yaml");
    let baseline = std::fs::read(&binding).unwrap();
    command(&home).args(["verify"]).assert().success();
    script(&home, "after");
    let output = command(&home)
        .args(["verify"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["drift"][0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|change| change["kind"] == "breaking"));
    assert_eq!(std::fs::read(binding).unwrap(), baseline);
}

#[test]
fn test_verify_missing_baseline_is_unverifiable_not_unchanged() {
    let home = TempDir::new().unwrap();
    let binary = script(&home, "before");
    command(&home)
        .args(["scan", binary.to_str().unwrap(), "--no-cache"])
        .assert()
        .success();
    let dir = home.path().join(".apexe/bindings");
    let mut modules = apexe::output::load_modules_from_dir(&dir, None).unwrap();
    modules[0].metadata.remove("source_contract");
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &dir, false)
        .unwrap();
    let output = command(&home)
        .arg("verify")
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["drift"][0]["status"], "unverifiable");
}

#[test]
fn test_verify_earns_case_result_and_refuses_networked_or_real_paths() {
    let home = TempDir::new().unwrap();
    command(&home)
        .args(["scan", "cat", "--no-cache"])
        .assert()
        .success();
    let dir = home.path().join(".apexe/bindings");
    let mut modules = apexe::output::load_modules_from_dir(&dir, None).unwrap();
    // The test asserts only the reviewed cat invocation, independent of platform parsers.
    modules[0].input_schema = json!({"type":"object","properties":{"file":{"type":"array","items":{"type":"string","x-apexe-path":true},"x-apexe-positional":0}}});
    modules[0].annotations.as_mut().unwrap().readonly = true;
    modules[0].annotations.as_mut().unwrap().idempotent = true;
    modules[0].annotations.as_mut().unwrap().destructive = false;
    modules[0].annotations.as_mut().unwrap().requires_approval = false;
    modules[0].annotations.as_mut().unwrap().open_world = false;
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &dir, false)
        .unwrap();
    let cases = home.path().join("cases.json");
    let mut data = json!({"allowlist":["cli.cat"],"cases":[{"module_id":"cli.cat","input":{"file":["$FIXTURE/input.txt"]},"fixtures":{"input.txt":"hello\n"},"expected_stdout":"hello\n"}]});
    std::fs::write(&cases, data.to_string()).unwrap();
    let output = command(&home)
        .args(["verify", "--cases", cases.to_str().unwrap()])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["behavioral"][0]["status"], "passed", "{report}");
    assert!(report["behavioral"][0]["case_digest"].is_string());
    assert!(report["behavioral"][0]["binary_digest"].is_string());
    data["cases"][0]["input"]["file"][0] = json!("/etc/passwd");
    std::fs::write(&cases, data.to_string()).unwrap();
    let output = command(&home)
        .args(["verify", "--cases", cases.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["behavioral"][0]["status"], "failed");
    modules[0].annotations.as_mut().unwrap().open_world = true;
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &dir, false)
        .unwrap();
    let output = command(&home)
        .args(["verify", "--cases", cases.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["behavioral"][0]["status"], "unverifiable");
}

#[test]
fn test_verify_authoritative_overlay_cannot_hide_source_drift() {
    let home = TempDir::new().unwrap();
    let binary = script(&home, "before");
    let overlay = home.path().join("overlay.json");
    std::fs::write(&overlay, json!({"schema_version":"1.1","command":"drift-fixture","variant":"unknown","mode":"authoritative","confidence":"high",
        "flags":[{"long":"--before","type":"string","description":"Fixture option"}]}).to_string()).unwrap();
    command(&home)
        .args([
            "scan",
            binary.to_str().unwrap(),
            "--no-cache",
            "--overlay",
            overlay.to_str().unwrap(),
        ])
        .assert()
        .success();
    script(&home, "after");
    let output = command(&home)
        .args(["verify", "--overlay", overlay.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let changes = report["drift"][0]["changes"].as_array().unwrap();
    assert!(changes
        .iter()
        .any(|change| change["path"].as_str().unwrap().starts_with("source.")));
    assert!(!changes
        .iter()
        .any(|change| change["path"].as_str().unwrap().starts_with("published.")));
}

#[test]
fn test_verify_refuses_unmarked_string_operands_and_fake_json_output_flags() {
    let home = TempDir::new().unwrap();
    command(&home)
        .args(["scan", "cat", "--no-cache"])
        .assert()
        .success();
    let dir = home.path().join(".apexe/bindings");
    let mut modules = apexe::output::load_modules_from_dir(&dir, None).unwrap();
    modules[0].input_schema = json!({"type":"object","properties":{
        "file":{"type":"array","items":{"type":"string","x-apexe-path":true},"x-apexe-positional":1},
        "unmarked":{"type":"string","x-apexe-positional":0}}});
    let annotation = modules[0].annotations.as_mut().unwrap();
    annotation.readonly = true;
    annotation.idempotent = true;
    annotation.destructive = false;
    annotation.requires_approval = false;
    annotation.open_world = false;
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &dir, false)
        .unwrap();
    let cases = home.path().join("cases.json");
    std::fs::write(&cases, json!({"allowlist":["cli.cat"],"cases":[{"module_id":"cli.cat","input":{"file":["$FIXTURE/a"],"unmarked":"/etc/passwd"},"fixtures":{"a":"safe"},"expected_stdout":"safe"}]}).to_string()).unwrap();
    let output = command(&home)
        .args(["verify", "--cases", cases.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["behavioral"][0]["status"], "failed");
    modules[0]
        .metadata
        .insert("json_flag".into(), json!("/etc/passwd"));
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &dir, false)
        .unwrap();
    let output = command(&home)
        .args(["verify", "--cases", cases.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["behavioral"][0]["status"], "unverifiable");
}

#[test]
fn test_verify_report_cannot_overwrite_a_binding_baseline() {
    let home = TempDir::new().unwrap();
    let binary = script(&home, "before");
    command(&home)
        .args(["scan", binary.to_str().unwrap(), "--no-cache"])
        .assert()
        .success();
    let binding = home
        .path()
        .join(".apexe/bindings/cli.drift_fixture.binding.yaml");
    let before = std::fs::read(&binding).unwrap();
    command(&home)
        .args(["verify", "--report", binding.to_str().unwrap()])
        .assert()
        .failure();
    assert_eq!(std::fs::read(binding).unwrap(), before);
}
