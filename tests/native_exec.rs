#![cfg(unix)]
use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use serde_json::{json, Value};
use std::path::PathBuf;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
        }
    }
    fn bindings(&self) -> PathBuf {
        self.home.path().join(".apexe/bindings")
    }
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("HOME", self.home.path())
            .env("RUST_LOG", "info")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env_remove("APEXE_SHIM_DIR")
            .current_dir(self.home.path());
        command
    }
    fn module(&self, name: &str, binary: &str, schema: Value, streaming: bool) {
        let mut module = ScannedModule::new(
            format!("cli.{name}"),
            "Native execution test".into(),
            schema,
            json!({"type":"object"}),
            vec!["cli".into()],
            format!("exec://{binary}"),
        );
        module.annotations = Some(ModuleAnnotations {
            readonly: true,
            open_world: false,
            ..Default::default()
        });
        module.metadata.insert("command_path".into(), json!([name]));
        module.metadata.insert("variant".into(), json!("unknown"));
        module.metadata.insert("shim_contract".into(), json!({"authoritative":true,"variant":"unknown","short_flag_bundling":true,"streaming_stdin_safe":streaming,"cross_flag_ordering":false}));
        module.input_schema["x-apexe-streaming-stdin-safe"] = json!(streaming);
        module.input_schema["x-apexe-short-flag-bundling"] = json!(true);
        module.input_schema["x-apexe-cross-flag-ordering"] = json!(false);
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.bindings(), false)
            .unwrap();
    }
    fn script(&self, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.home.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{contents}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    fn acl(&self, policy: &str) {
        std::fs::create_dir_all(self.home.path().join(".apexe")).unwrap();
        std::fs::write(self.home.path().join(".apexe/acl.yaml"), policy).unwrap();
    }
    fn audit(&self) -> String {
        std::fs::read_to_string(self.home.path().join(".apexe/audit.jsonl")).unwrap()
    }
}

fn file_schema() -> Value {
    json!({"type":"object","x-apexe-end-of-options":true,"properties":{
        "file":{"type":"array","items":{"type":"string","x-apexe-path":true},"x-apexe-positional":0},
        "n":{"type":"boolean","x-apexe-flag":"-n"}
    },"additionalProperties":false})
}

#[test]
fn test_exec_replaces_run_without_alias() {
    let fixture = Fixture::new();
    fixture.command().args(["run", "cli.cat"]).assert().code(2);
    fixture
        .command()
        .args(["exec", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--unmanaged"));
}

#[test]
fn test_exec_native_without_shim_installation_streams_and_audits() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["exec", "--", "cat", "-n"])
        .write_stdin("hello\n")
        .assert()
        .success()
        .stdout(predicates::str::contains("hello\n"));
    assert!(fixture.audit().contains("\"module_id\":\"cli.cat\""));
    assert!(fixture.audit().contains("\"event\":\"execution\""));
    assert!(!fixture
        .home
        .path()
        .join(".apexe/bin/.apexe-shims.json")
        .exists());
}

#[test]
fn test_exec_native_dry_run_has_governance_and_no_process() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "touch started");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    let output = fixture
        .command()
        .args(["exec", "--dry-run", "--", binary.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(plan["module_id"], "cli.toy");
    assert_eq!(plan["mode"], "native");
    assert_eq!(plan["valid"], true);
    assert!(!fixture.home.path().join("started").exists());
}

#[test]
fn test_exec_native_options_after_separator_belong_to_child() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["exec", "--", "cat", "--dry-run"])
        .assert()
        .failure();
    assert!(fixture.audit().contains("refusal"));
}

#[test]
fn test_exec_native_refusal_never_uses_unmanaged_allow() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "touch started");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
            "--unknown",
        ])
        .assert()
        .failure();
    assert!(!fixture.home.path().join("started").exists());
    assert!(fixture.audit().contains("refusal"));
}

#[test]
fn test_exec_native_acl_denial_omits_secret_plan() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    fixture.acl("rules: []\ndefault_effect: deny\n");
    let output = fixture
        .command()
        .args(["exec", "--dry-run", "--", "cat", "secret-path"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(plan["acl_decision"], "denied");
    assert!(plan.get("argv").is_none());
    assert!(!fixture.audit().contains("secret-path"));
}

#[test]
fn test_exec_native_path_guard_refuses_credentials() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    let path = fixture.home.path().join(".ssh/id_rsa");
    fixture
        .command()
        .args(["exec", "--", "cat", path.to_str().unwrap()])
        .assert()
        .failure();
    let plan = fixture
        .command()
        .args(["exec", "--dry-run", "--", "cat", path.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&plan).unwrap();
    assert_eq!(plan["path_guard"][0]["allowed"], false);
}

#[test]
fn test_exec_native_missing_contract_cannot_be_raw() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "touch started");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0].metadata.remove("shim_contract");
    modules[0].metadata.remove("command_path");
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert!(!fixture.home.path().join("started").exists());
}

#[test]
fn test_exec_native_checks_binary_identity() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    let alternative = fixture.script("cat", "touch started");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            alternative.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert!(!fixture.home.path().join("started").exists());
    fixture
        .command()
        .args(["exec", "--", "/bin/cat"])
        .write_stdin("ok\n")
        .assert()
        .success()
        .stdout("ok\n");
}

#[test]
fn test_exec_native_preserves_exit_code_stderr_and_signal() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "printf 'out'; printf 'err' >&2; exit 7");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    fixture
        .command()
        .args(["exec", "--", binary.to_str().unwrap()])
        .assert()
        .code(7)
        .stdout("out")
        .stderr(predicates::str::contains("err"));
    std::fs::write(&binary, "#!/bin/sh\nkill -TERM $$\n").unwrap();
    fixture
        .command()
        .args(["exec", "--", binary.to_str().unwrap()])
        .assert()
        .code(143);
}

#[test]
fn test_exec_native_timeout_kills_child() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "exec sleep 5");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    fixture
        .command()
        .args(["exec", "--timeout", "1", "--", binary.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("timed out"));
    assert!(fixture.audit().contains("\"exit_code\":-1"));
}

#[test]
fn test_exec_rejects_missing_or_mixed_modes() {
    let fixture = Fixture::new();
    for args in [
        vec!["exec"],
        vec![
            "exec",
            "--",
            "cat",
            "--input",
            "{}",
            "--dry-run",
            "--as",
            "ci",
        ],
        vec!["exec", "cli.cat", "--", "cat"],
        vec!["exec", "--input", "{}", "--", "cat"],
        vec!["exec", "--as", "ci", "--", "cat"],
    ] {
        fixture.command().args(&args).assert().failure();
    }
}

#[test]
fn test_exec_unmanaged_defaults_to_deny_and_audits_without_arguments() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "touch started");
    fixture
        .command()
        .args(["exec", "--", binary.to_str().unwrap(), "secret"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unmanaged"));
    assert!(!fixture.home.path().join("started").exists());
    assert!(fixture.audit().contains("refusal"));
    assert!(!fixture.audit().contains("secret"));
}

#[test]
fn test_exec_unmanaged_allow_is_literal_argv_with_acl_and_audit() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "printf '%s\\n' \"$@\"");
    fixture.acl("rules:\n- callers: ['local']\n  targets: ['unmanaged.toy']\n  effect: allow\ndefault_effect: deny\n");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
            "--flag",
            "$(touch started);literal",
        ])
        .assert()
        .success()
        .stdout("--flag\n$(touch started);literal\n");
    assert!(!fixture.home.path().join("started").exists());
    assert!(fixture.audit().contains("unmanaged.toy"));
    assert!(!fixture.audit().contains("literal"));
    fixture.acl("rules: []\ndefault_effect: deny\n");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn test_exec_unmanaged_approval_is_reported_and_fails_closed() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "touch started");
    let output = fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "approval",
            "--dry-run",
            "--",
            binary.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(plan["approval_required"], true);
    assert_eq!(plan["mode"], "unmanaged");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "approval",
            "--",
            binary.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert!(!fixture.home.path().join("started").exists());
}

#[test]
fn test_exec_unmanaged_stdin_is_null() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "exec /bin/cat");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
        ])
        .write_stdin("never forwarded\n")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn test_exec_native_ignores_json_flag_in_execution_and_plan() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "printf '%s' \"$#\"");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0]
        .metadata
        .insert("json_flag".into(), json!("--json"));
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture
        .command()
        .args(["exec", "--", binary.to_str().unwrap()])
        .assert()
        .success()
        .stdout("0");
    let output = fixture
        .command()
        .args(["exec", "--dry-run", "--", binary.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(plan["argv"], json!([]));
    fixture
        .command()
        .args(["exec", "cli.toy"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"stdout\":\"1\""));
}

#[test]
fn test_exec_native_routes_subcommands_without_acl_fallback_to_root() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "touch started");
    fixture.module("toy", binary.to_str().unwrap(),
        json!({"type":"object","properties":{"args":{"type":"array","items":{"type":"string"},"x-apexe-positional":0}},"additionalProperties":false}), false);
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    let mut child = modules[0].clone();
    child.module_id = "cli.toy.show".into();
    child.target.push_str(" show");
    child
        .metadata
        .insert("command_path".into(), json!(["toy", "show"]));
    child.input_schema["properties"] = json!({});
    modules.push(child);
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture.acl("rules:\n- callers: ['local']\n  targets: ['cli.toy']\n  effect: allow\ndefault_effect: deny\n");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
            "show",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("denied"));
    assert!(!fixture.home.path().join("started").exists());
    assert!(fixture.audit().contains("cli.toy.show"));
}

#[test]
fn test_exec_native_installed_shim_path_is_skipped_and_removed_route_stays_refused() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "sh"])
        .assert()
        .success();
    let shims = fixture.home.path().join(".apexe/bin");
    fixture
        .command()
        .env("PATH", format!("{}:/usr/bin:/bin", shims.display()))
        .env("APEXE_SHIM_DIR", &shims)
        .args(["exec", "--", "cat"])
        .write_stdin("ok\n")
        .assert()
        .success()
        .stdout("ok\n");
    std::fs::remove_file(fixture.bindings().join("cli.cat.binding.yaml")).unwrap();
    fixture
        .command()
        .args(["exec", "--unmanaged", "allow", "--", "cat"])
        .assert()
        .failure();
}

#[test]
fn test_exec_native_alias_to_registered_binary_cannot_bypass_contract() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), true);
    let alias = fixture.home.path().join("alias-cat");
    std::os::unix::fs::symlink("/bin/cat", &alias).unwrap();
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            alias.to_str().unwrap(),
            "--unknown",
        ])
        .assert()
        .failure();
    assert!(fixture.audit().contains("cli.cat"));
}

#[test]
fn test_exec_unmanaged_flags_after_separator_are_never_apexe_options() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "printf '%s\\n' \"$@\"");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
            "--man",
            "--help",
            "--timeout",
            "8",
            "--input",
            "{}",
        ])
        .assert()
        .success()
        .stdout("--man\n--help\n--timeout\n8\n--input\n{}\n");
}

#[test]
fn test_exec_module_preserves_exact_failure_status() {
    let fixture = Fixture::new();
    let binary = fixture.script("toy", "exit 7");
    fixture.module(
        "toy",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    fixture
        .command()
        .args(["exec", "cli.toy"])
        .assert()
        .code(7)
        .stdout(predicates::str::contains("\"exit_code\":7"));
}

#[test]
fn test_exec_native_simulation_is_asserted_and_stdin_requires_review() {
    let fixture = Fixture::new();
    fixture.module("cat", "/bin/cat", file_schema(), false);
    fixture.acl("rules:\n- callers: ['ci']\n  targets: ['cli.cat']\n  effect: allow\ndefault_effect: deny\n");
    let output = fixture
        .command()
        .args(["exec", "--dry-run", "--as", "ci", "--", "cat"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(plan["caller_asserted"], true);
    assert_eq!(plan["stdin"], "null");
    fixture
        .command()
        .args(["exec", "--", "cat"])
        .assert()
        .failure();
    fixture.acl("rules: []\ndefault_effect: allow\n");
    fixture
        .command()
        .args(["exec", "--", "cat"])
        .write_stdin("hidden\n")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn test_exec_unmanaged_checks_missing_binary_and_control_characters() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            "/missing/apexe-test-binary",
        ])
        .assert()
        .failure();
    let binary = fixture.script("toy", "touch started");
    fixture
        .command()
        .args([
            "exec",
            "--unmanaged",
            "allow",
            "--",
            binary.to_str().unwrap(),
            "line\nbreak",
        ])
        .assert()
        .failure();
    assert!(!fixture.home.path().join("started").exists());
}
