use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;

fn fixture() -> TempDir {
    let home = TempDir::new().unwrap();
    let mut module = ScannedModule::new(
        "cli.echo".into(),
        "Echo a positional message".into(),
        json!({"type":"object", "properties": {
            "message":{"type":"string", "x-apexe-positional":0},
            "file":{"type":"string", "x-apexe-positional":1, "x-apexe-path":true}
        }, "required":["message"], "additionalProperties":false}),
        json!({"type":"object"}),
        vec!["cli".into()],
        "exec:///bin/echo".into(),
    );
    let annotations = ModuleAnnotations {
        readonly: true,
        ..Default::default()
    };
    module.annotations = Some(annotations);
    apexe::output::YamlOutput::without_verification()
        .write(&[module], &home.path().join(".apexe/bindings"), false)
        .unwrap();
    home
}

fn command(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("apexe").unwrap();
    cmd.env("HOME", home.path())
        .env_remove("APEXE_BINDINGS_DIR")
        .env_remove("APCORE_CONFIG")
        .env("RUST_LOG", "info")
        .current_dir(home.path());
    cmd
}

fn write_acl(home: &TempDir, content: &str) {
    std::fs::write(home.path().join(".apexe/acl.yaml"), content).unwrap();
}

#[test]
fn test_run_returns_captured_json_and_warns_without_acl() {
    let home = fixture();
    let output = command(&home)
        .args(["run", "cli.echo", "--input", r#"{"message":"hello"}"#])
        .assert()
        .success()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stdout"], "hello\n");
    assert_eq!(result["exit_code"], 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no ACL"));
    let audit = std::fs::read_to_string(home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("execution"));
    assert!(audit.contains("local"));
}

#[test]
fn test_run_rejects_unknown_module_and_invalid_schema() {
    let home = fixture();
    command(&home)
        .args(["run", "cli.missing"])
        .assert()
        .failure();
    command(&home)
        .args(["run", "cli.echo", "--input", r#"{"message":5}"#])
        .assert()
        .failure();
    command(&home)
        .args(["run", "cli.echo", "--input", "[]"])
        .assert()
        .failure();
}

#[test]
fn test_run_dry_run_reports_argv_without_execution() {
    let home = fixture();
    let output = command(&home)
        .args([
            "run",
            "cli.echo",
            "--dry-run",
            "--input",
            r#"{"message":"hello"}"#,
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["argv"], json!(["hello"]));
    assert_eq!(result["acl_decision"], "no_acl_loaded");
    assert_eq!(result["valid"], true);
    let audit = home.path().join(".apexe/audit.jsonl");
    assert!(
        !audit.exists()
            || !std::fs::read_to_string(audit)
                .unwrap()
                .contains("\"execution\"")
    );
}

#[test]
fn test_run_loads_default_acl_and_simulates_caller() {
    let home = fixture();
    write_acl(&home, "version: '1.0'\ndefault_effect: deny\nrules:\n  - callers: [local]\n    targets: [cli.echo]\n    effect: allow\n");
    command(&home)
        .args(["run", "cli.echo", "--input", r#"{"message":"ok"}"#])
        .assert()
        .success();
    let output = command(&home)
        .args([
            "run",
            "cli.echo",
            "--dry-run",
            "--as",
            "ci",
            "--input",
            r#"{"message":"ok"}"#,
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["acl_decision"], "denied");
    assert_eq!(result["caller_asserted"], true);
    command(&home)
        .args(["run", "cli.echo", "--as", "ci"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_run_fails_closed_on_malformed_or_missing_requested_acl() {
    let home = fixture();
    write_acl(&home, "rules: [broken");
    command(&home)
        .args(["run", "cli.echo", "--input", r#"{"message":"ok"}"#])
        .assert()
        .failure();
    command(&home)
        .args(["run", "cli.echo", "--acl", "/missing/acl.yaml"])
        .assert()
        .failure();
}

#[test]
fn test_run_dry_run_reports_path_guard_verdict() {
    let home = fixture();
    let input = json!({"message":"ok", "file": home.path().join(".ssh/id_rsa")}).to_string();
    let output = command(&home)
        .args(["run", "cli.echo", "--dry-run", "--input", &input])
        .assert()
        .failure()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["path_guard"][0]["allowed"], false);
    assert_eq!(result["valid"], false);
    command(&home)
        .args(["run", "cli.echo", "--input", &input])
        .assert()
        .failure();
}

#[test]
fn test_run_dry_run_does_not_start_a_side_effecting_process() {
    let home = fixture();
    let path = home.path().join("created");
    let module = ScannedModule::new(
        "cli.touch".into(),
        "Create a test file".into(),
        json!({"type":"object","properties":{"file":{"type":"string","x-apexe-positional":0,"x-apexe-path":true}},"required":["file"]}),
        json!({"type":"object"}),
        vec![],
        "exec:///usr/bin/touch".into(),
    );
    apexe::output::YamlOutput::without_verification()
        .write(&[module], &home.path().join(".apexe/bindings"), false)
        .unwrap();
    let input = json!({"file":path}).to_string();
    command(&home)
        .args(["run", "cli.touch", "--dry-run", "--input", &input])
        .assert()
        .success();
    assert!(!path.exists());
    command(&home)
        .args(["run", "cli.touch", "--input", &input])
        .assert()
        .success();
    assert!(path.exists());
}

#[test]
fn test_run_requires_approval_without_an_interactive_client() {
    let home = fixture();
    let mut module = ScannedModule::new(
        "cli.approval".into(),
        "Requires approval".into(),
        json!({"type":"object"}),
        json!({"type":"object"}),
        vec![],
        "exec:///bin/echo".into(),
    );
    let annotations = ModuleAnnotations {
        requires_approval: true,
        ..Default::default()
    };
    module.annotations = Some(annotations);
    apexe::output::YamlOutput::without_verification()
        .write(&[module], &home.path().join(".apexe/bindings"), false)
        .unwrap();
    let output = command(&home)
        .args(["run", "cli.approval", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["approval_required"], true);
    command(&home)
        .args(["run", "cli.approval"])
        .assert()
        .failure();
}

#[test]
fn test_run_reports_a_child_failure_as_nonzero() {
    let home = fixture();
    let module = ScannedModule::new(
        "cli.false".into(),
        "Fail".into(),
        json!({"type":"object"}),
        json!({"type":"object"}),
        vec![],
        "exec:///usr/bin/false".into(),
    );
    apexe::output::YamlOutput::without_verification()
        .write(&[module], &home.path().join(".apexe/bindings"), false)
        .unwrap();
    let output = command(&home)
        .args(["run", "cli.false"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"], 1);
}

#[test]
fn test_run_warns_about_bindings_with_no_scan_format() {
    let home = fixture();
    let bindings = home.path().join(".apexe/bindings");
    for entry in std::fs::read_dir(bindings).unwrap() {
        let path = entry.unwrap().path();
        let mut document: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        document["bindings"][0]["metadata"]
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::String("apexe_scan_format".into()));
        std::fs::write(path, serde_yaml::to_string(&document).unwrap()).unwrap();
    }
    command(&home)
        .args(["run", "cli.echo", "--input", r#"{"message":"ok"}"#])
        .assert()
        .success()
        .stderr(predicates::str::contains("governance markers may be stale"));
}

#[cfg(unix)]
#[test]
fn test_run_fails_closed_on_a_dangling_default_acl_symlink() {
    let home = fixture();
    std::os::unix::fs::symlink("missing.yaml", home.path().join(".apexe/acl.yaml")).unwrap();
    command(&home)
        .args(["run", "cli.echo", "--input", r#"{"message":"ok"}"#])
        .assert()
        .failure();
}

#[test]
fn test_run_denied_preflight_omits_command_details() {
    let home = fixture();
    write_acl(&home, "version: '1.0'\ndefault_effect: deny\nrules: []\n");
    let output = command(&home)
        .args([
            "run",
            "cli.echo",
            "--dry-run",
            "--input",
            r#"{"message":"secret"}"#,
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["acl_decision"], "denied");
    assert!(result.get("argv").is_none());
    assert!(result.get("binary_path").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
}

#[test]
fn test_run_unknown_module_does_not_report_an_acl_allow() {
    let home = fixture();
    write_acl(&home, "version: '1.0'\ndefault_effect: allow\nrules: []\n");
    let output = command(&home)
        .args(["run", "cli.missing", "--dry-run"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["acl_decision"], "not_evaluated");
}
