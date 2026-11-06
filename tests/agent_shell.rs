#![cfg(unix)]

use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use predicates::str::contains;
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            home: TempDir::new().unwrap(),
        };
        fixture.add_binding("cli.apxepwd", "apxepwd", "/bin/pwd");
        fixture
    }

    fn bindings(&self) -> PathBuf {
        self.home.path().join(".apexe/bindings")
    }

    fn shims(&self) -> PathBuf {
        self.home.path().join(".apexe/bin")
    }

    fn entry_point(&self) -> PathBuf {
        self.shims().join("apexe-sh")
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("APEXE_OFFLINE", "true")
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env_remove("APEXE_REAL_SHELL")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(self.home.path());
        command
    }

    fn entry_command(&self) -> Command {
        let mut command = Command::new(self.entry_point());
        command
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env_remove("APEXE_REAL_SHELL")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(self.home.path());
        command
    }

    fn add_binding(&self, module_id: &str, name: &str, target: &str) {
        let mut module = ScannedModule::new(
            module_id.into(),
            "Agent shell test module".into(),
            json!({
                "type": "object",
                "x-apexe-end-of-options": true,
                "properties": {},
                "additionalProperties": false
            }),
            json!({"type": "object"}),
            vec!["pilot".into()],
            format!("exec://{target}"),
        );
        module.annotations = Some(ModuleAnnotations {
            readonly: true,
            ..Default::default()
        });
        module.metadata.insert("command_path".into(), json!([name]));
        module.metadata.insert("variant".into(), json!("unknown"));
        module.metadata.insert(
            "shim_contract".into(),
            json!({
                "authoritative": true,
                "variant": "unknown",
                "short_flag_bundling": true,
                "streaming_stdin_safe": false,
                "cross_flag_ordering": false
            }),
        );
        module.input_schema["x-apexe-streaming-stdin-safe"] = json!(false);
        module.input_schema["x-apexe-short-flag-bundling"] = json!(true);
        module.input_schema["x-apexe-cross-flag-ordering"] = json!(false);
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.bindings(), false)
            .unwrap();
    }

    fn add_unverified_binding(&self, module_id: &str, name: &str, target: &str) {
        let mut module = ScannedModule::new(
            module_id.into(),
            "Agent shell unverified module".into(),
            json!({"type": "object", "properties": {}}),
            json!({"type": "object"}),
            vec!["pilot".into()],
            format!("exec://{target}"),
        );
        module.annotations = Some(ModuleAnnotations {
            readonly: true,
            ..Default::default()
        });
        module.metadata.insert("command_path".into(), json!([name]));
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.bindings(), false)
            .unwrap();
    }

    fn add_scanned_binding(
        &self,
        module_id: &str,
        name: &str,
        target: &str,
        schema: Value,
        annotations: ModuleAnnotations,
    ) {
        let mut module = ScannedModule::new(
            module_id.into(),
            "Agent shell scanned module".into(),
            schema,
            json!({"type": "object"}),
            vec!["pilot".into()],
            format!("exec://{target}"),
        );
        module.annotations = Some(annotations);
        module.metadata.insert("command_path".into(), json!([name]));
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.bindings(), false)
            .unwrap();
    }

    fn add_scanned_script(
        &self,
        module_id: &str,
        name: &str,
        body: &str,
        schema: Value,
        annotations: ModuleAnnotations,
    ) {
        use std::os::unix::fs::PermissionsExt;

        let path = self.home.path().join(format!("{name}.sh"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.add_scanned_binding(module_id, name, path.to_str().unwrap(), schema, annotations);
    }

    fn add_script(&self, module_id: &str, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        let path = self.home.path().join(format!("{name}.sh"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.add_binding(module_id, name, path.to_str().unwrap());
    }

    fn enable(&self) {
        self.command().args(["env", "--enable"]).assert().success();
    }

    fn write_acl(&self) {
        std::fs::write(
            self.home.path().join(".apexe/acl.yaml"),
            "rules: []\ndefault_effect: deny\n",
        )
        .unwrap();
    }

    fn check_command(&self) -> Command {
        let mut command = self.command();
        command
            .args(["env", "--check"])
            .env(
                "PATH",
                std::env::join_paths([
                    self.shims(),
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/bin"),
                ])
                .unwrap(),
            )
            .env("APEXE_SHIM_DIR", self.shims());
        command
    }
}

#[test]
fn test_env_enable_installs_and_disable_removes_entry_point() {
    let fixture = Fixture::new();
    fixture.enable();
    assert!(fixture.entry_point().exists());
    fixture.write_acl();

    fixture
        .check_command()
        .assert()
        .success()
        .stdout(contains("[PASS] LINKS apexe-sh"));

    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert!(!fixture.entry_point().exists());
}

#[test]
fn test_sh_check_reports_entry_manifest_and_commands() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture
        .command()
        .args(["sh", "--check", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(report["real_shell"], "/bin/sh");
    assert_eq!(report["real_shell_source"], "environment_shell");
    assert_eq!(report["shims"]["entry_installed"], true);
    assert_eq!(report["shims"]["manifest_present"], true);
    assert!(report["shims"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|command| command == "apxepwd"));
    assert!(report["shims"]["acl"].is_null());
    assert_eq!(report["local_env"]["policy"], "inherit");
    assert!(report["local_env"]["denylist"]
        .as_array()
        .unwrap()
        .iter()
        .any(|name| name == "LD_PRELOAD"));
    assert!(report["host_config"]["opencode"]
        .as_str()
        .unwrap()
        .contains("apexe-sh"));
    let codex = report["host_config"]["codex"].as_str().unwrap();
    assert!(codex.contains("env --reload --shell zsh --shim-dir"));
    assert!(codex.contains(fixture.shims().to_str().unwrap()));
    assert!(codex.contains("Restart Codex"));
    assert!(report["host_config"].get("environment").is_none());
}

#[test]
fn test_sh_check_guides_claude_code_through_the_launch_environment_path() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture
        .command()
        .args(["sh", "--check", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    let claude = report["host_config"]["claude_code"].as_str().unwrap();

    assert!(claude.contains("SHELL=apexe-sh"));
    assert!(claude.contains("CLAUDE_CODE_SHELL=apexe-sh"));
    assert!(claude.contains("falling back to /bin/zsh"));
    assert!(claude.contains(fixture.shims().to_str().unwrap()));
    assert!(claude.contains("PATH="));
    assert!(claude.contains("shell snapshot"));
    assert!(claude.contains("startup-file edits"));
}

#[test]
fn test_sh_check_fails_without_an_installed_manifest() {
    let fixture = Fixture::new();

    let assert = fixture
        .command()
        .args(["sh", "--check", "--format", "json"])
        .assert()
        .failure();
    let output = assert.get_output();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["shims"]["manifest_present"], false);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("agent shell is not ready"), "{stderr}");
}

#[test]
fn test_sh_routes_a_simple_command_through_shims_and_preserves_cwd() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture
        .command()
        .args(["sh", "-c", "apxepwd"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let printed = String::from_utf8(output).unwrap();
    assert_eq!(
        printed.trim(),
        std::fs::canonicalize(fixture.home.path())
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn test_sh_preserves_exit_code_and_stdout_without_noise() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["sh", "-c", "exit 7"])
        .assert()
        .code(7);

    fixture
        .command()
        .args(["sh", "-c", "printf ok"])
        .assert()
        .success()
        .stdout("ok")
        .stderr("");
}

#[test]
fn test_entry_point_executes_the_real_shell_without_clap() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .entry_command()
        .args(["-c", "exit 5"])
        .assert()
        .code(5);
}

#[test]
fn test_entry_point_ignores_shell_pointing_at_itself() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .entry_command()
        .env("SHELL", fixture.entry_point())
        .args(["-c", "exit 6"])
        .assert()
        .code(6);
}

#[test]
fn test_sh_check_reports_self_reference_as_fallback() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture
        .command()
        .env("SHELL", fixture.entry_point())
        .args(["sh", "--check", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["real_shell_source"], "fallback");
    assert!(["/bin/bash", "/bin/sh"].contains(&report["real_shell"].as_str().unwrap()));
}

#[test]
fn test_sh_accepts_an_explicit_real_shell() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture
        .command()
        .args(["sh", "--check", "--format", "json", "--shell", "/bin/sh"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["real_shell"], "/bin/sh");
    assert_eq!(report["real_shell_source"], "explicit");
}

#[test]
fn test_sh_inherits_local_environment_for_shim_commands() {
    let fixture = Fixture::new();
    fixture.add_script(
        "cli.apxeenv",
        "apxeenv",
        "printf '%s' \"$APEXE_TEST_SECRET\"",
    );
    fixture.enable();

    fixture
        .command()
        .env("APEXE_TEST_SECRET", "visible")
        .args(["sh", "-c", "apxeenv"])
        .assert()
        .success()
        .stdout("visible");
}

#[test]
fn test_module_json_execution_keeps_the_scrubbed_environment() {
    let fixture = Fixture::new();
    fixture.add_script(
        "cli.apxeenv",
        "apxeenv",
        "printf '%s' \"$APEXE_TEST_SECRET\"",
    );
    fixture.enable();

    let output = fixture
        .command()
        .env("APEXE_TEST_SECRET", "must-not-leak")
        .args([
            "exec",
            "cli.apxeenv",
            "--input",
            "{}",
            "--bindings-dir",
            fixture.bindings().to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rendered = String::from_utf8(output).unwrap();
    assert!(!rendered.contains("must-not-leak"), "{rendered}");
}

#[test]
fn test_descendant_commands_keep_shim_routing() {
    let fixture = Fixture::new();
    fixture.add_script("cli.apxechild", "apxechild", "apxepwd");
    fixture.enable();

    let output = fixture
        .command()
        .args(["sh", "-c", "apxechild"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let printed = String::from_utf8(output).unwrap();
    assert_eq!(
        printed.trim(),
        std::fs::canonicalize(fixture.home.path())
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn test_locked_mode_restricts_path_to_shims_and_allowed_directories() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["sh", "--locked", "-c", "apxepwd"])
        .assert()
        .success();

    fixture
        .command()
        .args(["sh", "--locked", "-c", "ls"])
        .assert()
        .code(127);

    fixture
        .command()
        .args(["sh", "--locked", "--allow-path", "/bin", "-c", "ls"])
        .assert()
        .success();

    let output = fixture
        .command()
        .args(["sh", "--check", "--locked", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["local_env"]["restricted_search_path"], true);
}

#[test]
fn test_locked_mode_can_be_enabled_through_the_entry_point_environment() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .entry_command()
        .env("APEXE_SHELL_LOCKED", "1")
        .args(["-c", "apxepwd"])
        .assert()
        .success();

    fixture
        .entry_command()
        .env("APEXE_SHELL_LOCKED", "1")
        .args(["-c", "ls"])
        .assert()
        .code(127);
}

#[test]
fn test_sh_check_lists_modules_that_cannot_be_used_as_shims() {
    let fixture = Fixture::new();
    fixture.add_unverified_binding("cli.unverified", "unverified", "/bin/echo");
    fixture.enable();

    let output = fixture
        .command()
        .args(["sh", "--check", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert!(report["shims"]["unverified_modules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|module| module["module_id"] == "cli.unverified"));

    fixture
        .command()
        .args(["sh", "--check"])
        .assert()
        .success()
        .stdout(contains("[WARN] COVERAGE"));
}

fn reordering_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "flag": {"type": "boolean", "x-apexe-flag": "-n"},
            "file": {"type": "array", "items": {"type": "string"},
                     "x-apexe-positional": 0}
        },
        "additionalProperties": false
    })
}

fn readonly_annotations() -> ModuleAnnotations {
    ModuleAnnotations {
        readonly: true,
        open_world: false,
        ..Default::default()
    }
}

#[test]
fn test_scanned_route_executes_the_original_argv_without_reordering() {
    let fixture = Fixture::new();
    fixture.add_scanned_script(
        "cli.scanned",
        "scanned",
        "printf '%s\\n' \"$*\"",
        reordering_schema(),
        readonly_annotations(),
    );
    fixture
        .command()
        .args(["env", "--enable", "--scanned", "readonly"])
        .assert()
        .success();

    let output = fixture
        .command()
        .args(["sh", "-c", "scanned file -n"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(output).unwrap().trim(), "file -n");

    fixture.write_acl();
    fixture.check_command().assert().success();

    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("\"tier\":\"scanned\""), "{audit}");
}

#[test]
fn test_scanned_route_is_excluded_when_policy_is_off() {
    let fixture = Fixture::new();
    fixture.add_scanned_script(
        "cli.scanned",
        "scanned",
        "printf ok",
        reordering_schema(),
        readonly_annotations(),
    );
    fixture.enable();

    assert!(!fixture.shims().join("scanned").exists());
}

#[test]
fn test_scanned_readonly_policy_rejects_destructive_modules() {
    let fixture = Fixture::new();
    fixture.add_scanned_binding(
        "cli.scanned",
        "scanned",
        "/bin/echo",
        json!({"type": "object", "properties": {}}),
        ModuleAnnotations {
            destructive: true,
            ..Default::default()
        },
    );
    fixture
        .command()
        .args(["env", "--enable", "--scanned", "readonly"])
        .assert()
        .success();

    assert!(!fixture.shims().join("scanned").exists());
}

#[test]
fn test_scanned_all_policy_admits_destructive_modules() {
    let fixture = Fixture::new();
    fixture.add_scanned_binding(
        "cli.scanned",
        "scanned",
        "/bin/echo",
        json!({"type": "object", "properties": {}}),
        ModuleAnnotations {
            destructive: true,
            ..Default::default()
        },
    );
    fixture
        .command()
        .args(["env", "--enable", "--scanned", "all"])
        .assert()
        .success();

    assert!(fixture.shims().join("scanned").exists());
}

#[test]
fn test_sh_check_reports_scanned_routes_separately() {
    let fixture = Fixture::new();
    fixture.add_scanned_script(
        "cli.scanned",
        "scanned",
        "printf ok",
        reordering_schema(),
        readonly_annotations(),
    );
    fixture
        .command()
        .args(["env", "--enable", "--scanned", "readonly"])
        .assert()
        .success();

    let output = fixture
        .command()
        .args(["sh", "--check", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert!(report["shims"]["scanned_commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|name| name == "scanned"));
}

fn hook_event(command: &str) -> String {
    json!({
        "tool_name": "Bash",
        "tool_input": {"command": command, "description": "test command"}
    })
    .to_string()
}

#[test]
fn test_claude_hook_rewrites_a_simple_external_command() {
    let fixture = Fixture::new();
    fixture.enable();

    let output = fixture
        .command()
        .args(["hook", "claude"])
        .write_stdin(hook_event("ls -la"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let response: Value = serde_json::from_slice(&output).unwrap();
    let hook = &response["hookSpecificOutput"];
    assert_eq!(hook["permissionDecision"], "allow");
    let command = hook["updatedInput"]["command"].as_str().unwrap();
    assert!(command.contains(" sh -c "), "{command}");
    assert!(command.contains("ls -la"), "{command}");
    assert_eq!(hook["updatedInput"]["description"], "test command");
}

#[test]
fn test_claude_hook_leaves_stateful_and_compound_commands_alone() {
    let fixture = Fixture::new();
    fixture.enable();

    for command in ["cd /tmp", "ls | head", "export FOO=bar"] {
        let output = fixture
            .command()
            .args(["hook", "claude"])
            .write_stdin(hook_event(command))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(
            output.is_empty(),
            "{command}: {}",
            String::from_utf8_lossy(&output)
        );
    }
}

#[test]
fn test_claude_hook_does_not_rewrite_non_bash_or_already_routed_calls() {
    let fixture = Fixture::new();
    fixture.enable();

    let read_event = json!({
        "tool_name": "Read",
        "tool_input": {"file_path": "/tmp/x"}
    })
    .to_string();
    let output = fixture
        .command()
        .args(["hook", "claude"])
        .write_stdin(read_event)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(output.is_empty());

    for command in ["apexe sh -c 'ls'", "apexe exec -- ls", "apexe-sh -c 'ls'"] {
        let output = fixture
            .command()
            .args(["hook", "claude"])
            .write_stdin(hook_event(command))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(output.is_empty(), "{command}");
    }
}

#[test]
fn test_claude_hook_denies_when_shims_are_not_installed() {
    let fixture = Fixture::new();

    let output = fixture
        .command()
        .args(["hook", "claude"])
        .write_stdin(hook_event("ls -la"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let response: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(response["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(response["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .contains("apexe env --enable"));
}

#[test]
fn test_agent_install_prints_each_host_configuration() {
    let fixture = Fixture::new();
    fixture.enable();

    for (host, marker) in [
        ("opencode", "apexe-sh"),
        ("pi", "shellPath"),
        ("codex", "Codex"),
        ("claude", "Claude Code"),
    ] {
        fixture
            .command()
            .args(["agent", host])
            .assert()
            .success()
            .stdout(contains(marker));
    }
}

#[test]
fn test_agent_check_detects_opencode_shell_configuration() {
    let fixture = Fixture::new();
    fixture.enable();
    std::fs::write(
        fixture.home.path().join("opencode.json"),
        json!({"shell": fixture.shims().join("apexe-sh")}).to_string(),
    )
    .unwrap();

    let output = fixture
        .command()
        .args(["agent", "--check", "opencode", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["host"]["configured"], true);
    assert_eq!(report["host"]["host"], "opencode");
}

#[test]
fn test_agent_check_warns_but_does_not_fail_without_host_config() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["agent", "--check", "pi"])
        .assert()
        .success()
        .stdout(contains("[WARN] CONFIG"));
}

#[test]
fn test_agent_write_preserves_existing_opencode_keys() {
    let fixture = Fixture::new();
    fixture.enable();
    let path = fixture.home.path().join("opencode.json");
    std::fs::write(&path, r#"{"theme":"dark","shell":"old"}"#).unwrap();

    fixture
        .command()
        .args(["agent", "opencode", "--write"])
        .assert()
        .success();

    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["theme"], "dark");
    assert!(value["shell"].as_str().unwrap().ends_with("apexe-sh"));
}

#[test]
fn test_agent_write_refuses_when_launcher_is_not_installed() {
    let fixture = Fixture::new();
    let settings = fixture.home.path().join(".pi/agent/settings.json");

    fixture
        .command()
        .args(["agent", "pi", "--write", "--scope", "user"])
        .assert()
        .failure()
        .stderr(contains("apexe env --enable"));

    assert!(
        !settings.exists(),
        "a host that starts every command through apexe-sh must not be pointed at a launcher that is not installed"
    );
}

#[test]
fn test_agent_write_refuses_when_entry_point_is_not_the_launcher() {
    let fixture = Fixture::new();
    fixture.enable();
    // The incident this guards: the entry point went missing, and it was filled
    // in by hand with a shell script instead of by `apexe env --enable`. The file
    // is executable and delegates to a real shell, so nothing looks broken until
    // every command runs outside apexe.
    std::fs::remove_file(fixture.entry_point()).unwrap();
    std::fs::write(fixture.entry_point(), "#!/bin/sh\nexec /bin/zsh \"$@\"\n").unwrap();
    let settings = fixture.home.path().join(".pi/agent/settings.json");

    fixture
        .command()
        .args(["agent", "pi", "--write", "--scope", "user"])
        .assert()
        .failure()
        .stderr(contains("apexe env --enable"));

    assert!(!settings.exists());
}

#[test]
fn test_agent_write_configures_path_based_host_without_launcher() {
    // The refusal is scoped to hosts that select a shell by path. Claude Code
    // keeps the system shell and only gains hook commands, so a missing entry
    // point must not block writing its configuration.
    let fixture = Fixture::new();

    fixture
        .command()
        .args(["agent", "claude", "--write"])
        .assert()
        .success()
        .stdout(contains("Wrote"));
}

#[test]
fn test_env_disable_warns_when_host_still_selects_launcher() {
    // Disabling removes apexe-sh. A host already configured to it cannot start a
    // command afterwards, and nothing else in the output says so.
    let fixture = Fixture::new();
    fixture.enable();
    fixture
        .command()
        .args(["agent", "pi", "--write", "--scope", "user"])
        .assert()
        .success();

    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success()
        .stderr(contains("still selects"))
        .stderr(contains("apexe env --enable"));

    assert!(!fixture.entry_point().exists());
}

#[test]
fn test_agent_write_refuses_invalid_json() {
    let fixture = Fixture::new();
    fixture.enable();
    let path = fixture.home.path().join("opencode.json");
    std::fs::write(&path, "{not json").unwrap();

    fixture
        .command()
        .args(["agent", "opencode", "--write"])
        .assert()
        .failure();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
}

#[test]
fn test_agent_write_claude_adds_a_pretooluse_hook() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["agent", "claude", "--write"])
        .assert()
        .success();

    let path = fixture.home.path().join(".claude/settings.json");
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let command = value["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains("hook claude"), "{command}");
}

#[test]
fn test_agent_write_pi_uses_user_scope() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["agent", "pi", "--write"])
        .assert()
        .success();

    let path = fixture.home.path().join(".pi/agent/settings.json");
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(value["shellPath"].as_str().unwrap().ends_with("apexe-sh"));
}

#[test]
fn test_agent_mcp_only_writes_opencode_mcp_and_disables_bash() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["agent", "opencode", "--mcp-only", "--write"])
        .assert()
        .success();

    let path = fixture.home.path().join("opencode.json");
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["tools"]["bash"], false);
    let command = value["mcp"]["apexe"]["command"].as_array().unwrap();
    assert!(command[0].as_str().unwrap().ends_with("apexe"));
    assert_eq!(command[1], "serve");
}

#[test]
fn test_agent_mcp_only_writes_claude_mcp_and_denies_bash() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["agent", "claude", "--mcp-only", "--write"])
        .assert()
        .success();

    let mcp: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.home.path().join(".mcp.json")).unwrap(),
    )
    .unwrap();
    assert!(mcp["mcpServers"]["apexe"]["command"]
        .as_str()
        .unwrap()
        .ends_with("apexe"));

    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.home.path().join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(settings["permissions"]["deny"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| rule == "Bash"));
}

#[test]
fn test_agent_mcp_only_prints_codex_and_pi_guidance() {
    let fixture = Fixture::new();
    fixture.enable();

    fixture
        .command()
        .args(["agent", "codex", "--mcp-only"])
        .assert()
        .success()
        .stdout(contains("[mcp_servers.apexe]"));

    fixture
        .command()
        .args(["agent", "pi", "--mcp-only"])
        .assert()
        .success()
        .stdout(contains("no stable MCP-only profile"));
}

#[test]
fn test_claude_persistent_hook_routes_compound_commands_to_a_session() {
    let fixture = Fixture::new();
    fixture.enable();
    let event = json!({
        "tool_name": "Bash",
        "tool_input": {"command": "cd /tmp && pwd", "description": "compound"},
        "session_id": "session-1",
        "cwd": fixture.home.path()
    });

    let output = fixture
        .command()
        .args(["hook", "claude", "--persistent"])
        .write_stdin(event.to_string())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let response: Value = serde_json::from_slice(&output).unwrap();
    let command = response["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains("APEXE_SESSION_SOCKET"), "{command}");
    assert!(command.contains("cd /tmp && pwd"), "{command}");
}
