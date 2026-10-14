use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;

fn fixture() -> TempDir {
    let home = TempDir::new().unwrap();
    let module = ScannedModule::new(
        "cli.echo".into(),
        "Echo a positional message".into(),
        json!({"type":"object","properties":{
            "message":{"type":"string","x-apexe-positional":0}
        },"required":["message"],"additionalProperties":false}),
        json!({"type":"object"}),
        vec!["cli".into()],
        "exec:///bin/echo".into(),
    );
    apexe::output::YamlOutput::without_verification()
        .write(&[module], &home.path().join(".apexe/bindings"), false)
        .unwrap();
    std::fs::write(
        home.path().join(".apexe/acl.yaml"),
        "rules: []\ndefault_effect: allow\n",
    )
    .unwrap();
    home
}

fn command(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("apexe").unwrap();
    command.env("HOME", home.path()).current_dir(home.path());
    for name in [
        "RUST_LOG",
        "APEXE_LOG_LEVEL",
        "APEXE_BINDINGS_DIR",
        "APCORE_CONFIG",
    ] {
        command.env_remove(name);
    }
    command
}

fn failure(command: &mut Command, exit_code: i32) -> Value {
    let output = command.assert().code(exit_code).get_output().clone();
    assert!(output.stdout.is_empty());
    let result: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert!(result["error"]["message"].is_string());
    result
}

#[test]
fn test_file_and_stdin_inputs_match_inline_execution_and_preview() {
    let home = fixture();
    let input = r#"{"message":"hello from JSON"}"#;
    std::fs::write(home.path().join("input.json"), input).unwrap();
    for preview in [false, true] {
        let mut results = Vec::new();
        for source in ["inline", "file", "stdin"] {
            let mut cmd = command(&home);
            cmd.args(["exec", "cli.echo"]);
            match source {
                "inline" => {
                    cmd.args(["--input", input]);
                }
                "file" => {
                    cmd.args(["--input-file", "input.json"]);
                }
                _ => {
                    cmd.args(["--input-file", "-"]).write_stdin(input);
                }
            }
            if preview {
                cmd.arg("--dry-run");
            }
            let output = cmd.assert().success().get_output().stdout.clone();
            let result: Value = serde_json::from_slice(&output).unwrap();
            results.push(if preview {
                result["argv"].clone()
            } else {
                result["stdout"].clone()
            });
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[1], results[2]);
    }
}

#[test]
fn test_json_errors_cover_runtime_schema_and_argument_failures() {
    let home = fixture();
    for (args, code, expected) in [
        (vec!["exec", "cli.missing"], 1, "MODULE_NOT_FOUND"),
        (
            vec!["exec", "cli.echo", "--input", r#"{"message":5}"#],
            1,
            "SCHEMA_VALIDATION_ERROR",
        ),
        (vec!["exec"], 2, "GENERAL_INVALID_INPUT"),
        (
            vec!["exec", "cli.echo", "--input", "{}", "--input-file", "-"],
            2,
            "GENERAL_INVALID_INPUT",
        ),
    ] {
        let result = failure(
            command(&home).args(["--error-format", "json"]).args(args),
            code,
        );
        assert_eq!(result["error"]["code"], expected);
    }
}

#[test]
fn test_json_input_errors_are_bounded_and_do_not_echo_payloads() {
    let home = fixture();
    for input in ["", "[]", "null", "{\"secret\":\"PRIVATE-PAYLOAD\"", "{} {}"] {
        let result = failure(
            command(&home)
                .args([
                    "exec",
                    "cli.echo",
                    "--error-format=json",
                    "--input-file",
                    "-",
                ])
                .write_stdin(input),
            1,
        );
        assert_eq!(result["error"]["code"], "GENERAL_INVALID_INPUT");
        assert!(!result.to_string().contains("PRIVATE-PAYLOAD"));
    }
    let result = failure(
        command(&home)
            .args([
                "exec",
                "cli.echo",
                "--error-format=json",
                "--input-file",
                "-",
            ])
            .write_stdin(vec![b' '; 1_048_577]),
        1,
    );
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("1048576"));
}

#[test]
fn test_file_failures_and_native_input_conflicts_are_structured() {
    let home = fixture();
    let result = failure(
        command(&home).args([
            "exec",
            "cli.echo",
            "--error-format",
            "json",
            "--input-file",
            "missing.json",
        ]),
        1,
    );
    assert_eq!(result["error"]["code"], "GENERAL_INVALID_INPUT");
    failure(
        command(&home).args([
            "exec",
            "--error-format",
            "json",
            "--input-file",
            "-",
            "--",
            "echo",
            "hello",
        ]),
        2,
    );
}

#[test]
fn test_default_errors_help_and_child_exit_status_keep_legacy_behavior() {
    let home = fixture();
    let output = command(&home)
        .args(["exec", "cli.missing"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("Error:"));
    command(&home)
        .args(["--error-format", "json", "--help"])
        .assert()
        .success();
    let mut modules =
        apexe::output::load_modules_from_dir(&home.path().join(".apexe/bindings"), None).unwrap();
    modules[0].target = "exec:///usr/bin/false".into();
    modules[0].input_schema = json!({"type":"object"});
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &home.path().join(".apexe/bindings"), false)
        .unwrap();
    let output = command(&home)
        .args(["exec", "cli.echo", "--error-format", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let result: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    assert_eq!(result["exit_code"], 1);
    assert!(output.stderr.is_empty());
}
