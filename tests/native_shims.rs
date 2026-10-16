#![cfg(unix)]
use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
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
    fn shims(&self) -> PathBuf {
        self.home.path().join(".apexe/bin")
    }
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("APEXE_OFFLINE", "true")
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "info")
            .current_dir(self.home.path());
        command
    }
    fn write(&self, id: &str, name: &str, binary: &str, schema: Value, streaming: bool) {
        let mut module = ScannedModule::new(
            id.into(),
            "Native shim test".into(),
            schema,
            json!({"type":"object"}),
            vec!["pilot".into()],
            format!("exec://{binary}"),
        );
        module.annotations = Some(ModuleAnnotations {
            readonly: true,
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
    fn enable(&self) {
        self.command().args(["env", "--enable"]).assert().success();
    }
    fn shim(&self, name: &str) -> Command {
        let mut command = Command::new(self.shims().join(name));
        command
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "info")
            .current_dir(self.home.path());
        command
    }
    fn script(&self, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.home.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{contents}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

fn file_schema() -> Value {
    json!({"type":"object","x-apexe-end-of-options":true,"properties":{
        "file":{"type":"array","items":{"type":"string","x-apexe-path":true},"x-apexe-positional":0},
        "n":{"type":"boolean","x-apexe-flag":"-n"}, "s":{"type":"boolean","x-apexe-flag":"-s"}
    }, "additionalProperties":false})
}

#[test]
fn test_activation_script_sources_current_shell_idempotently_without_rehash() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    #[cfg(target_os = "macos")]
    let shells = ["/bin/sh", "/bin/bash", "/bin/zsh"];
    #[cfg(not(target_os = "macos"))]
    let shells = ["/bin/sh", "/bin/bash"];
    for shell in shells {
        Command::new(shell).env("HOME", fixture.home.path())
            // Zsh reads startup files from ZDOTDIR even when HOME is overridden.
            .env("ZDOTDIR", fixture.home.path())
            .env("PATH", "/opt/added-at-runtime:/usr/bin:/bin")
            .env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
            .args(["-c", "cat /dev/null; . \"$1\" || exit; . \"$1\" || exit; command -v cat; printf '%s\\n' \"$PATH\"; printf 'sourced\\n' | cat", "shell"])
            .arg(fixture.shims().join("activate")).assert().success().stderr("")
            .stdout(format!("{}/cat\n{}:/opt/added-at-runtime:/usr/bin:/bin\nsourced\n", fixture.shims().display(), fixture.shims().display()));
    }
}

#[test]
fn test_activation_script_refuses_invalid_manifest_without_changing_path() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    std::fs::write(fixture.shims().join(".apexe-shims.json"), "invalid").unwrap();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", "if . \"$1\"; then exit 99; fi; test \"$PATH\" = /usr/bin:/bin && test -z \"$APEXE_SHIM_DIR\"", "shell"])
        .arg(fixture.shims().join("activate")).assert().success();
}

#[test]
fn test_activation_script_installation_refuses_unowned_file_without_mutation() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    std::fs::create_dir_all(fixture.shims()).unwrap();
    std::fs::write(fixture.shims().join("activate"), "keep user script").unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("activation script"));
    assert_eq!(
        std::fs::read_to_string(fixture.shims().join("activate")).unwrap(),
        "keep user script"
    );
    assert!(!fixture.shims().join("cat").exists());
    assert!(!fixture.home.path().join(".profile").exists());
}

#[test]
fn test_activation_script_changes_prevent_reload_and_uninstall() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let startup = std::fs::read(fixture.home.path().join(".profile")).unwrap();
    std::fs::write(fixture.shims().join("activate"), "modified script").unwrap();
    for operation in ["--enable", "--reload", "--disable"] {
        fixture
            .command()
            .args(["env", operation])
            .assert()
            .failure()
            .stderr(predicates::str::contains("activation script"));
        assert!(fixture.shims().join("cat").exists());
        assert_eq!(
            std::fs::read(fixture.home.path().join(".profile")).unwrap(),
            startup
        );
    }
}

#[test]
fn test_activation_script_symlinks_are_never_replaced() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let script = fixture.shims().join("activate");
    let target = fixture.home.path().join("keep-script");
    std::fs::rename(&script, &target).unwrap();
    let original = std::fs::read(&target).unwrap();
    std::os::unix::fs::symlink(&target, &script).unwrap();
    for operation in ["--reload", "--disable"] {
        fixture
            .command()
            .args(["env", operation])
            .assert()
            .failure();
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert_eq!(std::fs::read_link(&script).unwrap(), target);
    }
}

#[test]
fn test_activation_script_reload_upgrades_legacy_manifest_and_disable_cleans_up() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let manifest_path = fixture.shims().join(".apexe-shims.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest
        .as_object_mut()
        .unwrap()
        .remove("activation_script");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::remove_file(fixture.shims().join("activate")).unwrap();
    fixture
        .command()
        .args(["env", "--reload"])
        .assert()
        .success();
    assert!(fixture.shims().join("activate").is_file());
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert!(!fixture.shims().join("activate").exists());
}

#[test]
fn test_activation_script_name_is_reserved_from_shim_routes() {
    for name in [
        "activate",
        "deactivate",
        "apexe-activate.sh",
        "apexe-deactivate.sh",
        ".apexe-source-scripts.json",
    ] {
        let fixture = Fixture::new();
        fixture.write("cli.reserved", name, "/bin/cat", file_schema(), true);
        fixture
            .command()
            .args(["env", "--enable"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("reserved"));
        assert!(!fixture.shims().join(".apexe-shims.json").exists());
    }
}

#[test]
fn test_deactivation_script_disables_shims_and_restores_original_environment() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("PATH", "/usr/bin:/bin")
        .env("APEXE_SHIM_DIR", "previous-value").current_dir(fixture.home.path())
        .args(["-c", ". \"$1\" || exit; . \"$1\" || exit; . \"$2\" || exit; test \"$PATH\" = /usr/bin:/bin && test \"$APEXE_SHIM_DIR\" = previous-value && test -z \"${_APEXE_ACTIVE_SHIM_DIR+x}\"", "shell"])
        .arg(fixture.shims().join("activate")).arg(fixture.shims().join("deactivate"))
        .assert().success();
    assert!(!fixture.shims().join("cat").exists());
    assert!(!fixture.shims().join("activate").exists());
    assert!(!fixture.shims().join("deactivate").exists());
}

#[test]
fn test_deactivation_restores_unset_and_empty_environment_variables() {
    for original in [
        "unset PATH APEXE_SHIM_DIR",
        "export PATH='' APEXE_SHIM_DIR=''",
    ] {
        let fixture = Fixture::new();
        fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
        fixture.enable();
        let expected = if original.starts_with("unset") {
            "test -z \"${PATH+x}\" && test -z \"${APEXE_SHIM_DIR+x}\""
        } else {
            "test \"${PATH+x}\" = x && test -z \"$PATH\" && test \"${APEXE_SHIM_DIR+x}\" = x && test -z \"$APEXE_SHIM_DIR\""
        };
        Command::new("/bin/sh")
            .env("HOME", fixture.home.path())
            .current_dir(fixture.home.path())
            .args([
                "-c",
                &format!("{original}; . \"$1\" || exit; . \"$2\" || exit; {expected}"),
                "shell",
            ])
            .arg(fixture.shims().join("activate"))
            .arg(fixture.shims().join("deactivate"))
            .assert()
            .success();
    }
}

#[test]
fn test_deactivation_handles_automatic_startup_activation() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", ". \"$1\" || exit; test \"$_APEXE_ACTIVE_SHIM_DIR\" = \"$APEXE_SHIM_DIR\" || exit 99; . \"$2\" || exit; test \"$PATH\" = /usr/bin:/bin && test -z \"${APEXE_SHIM_DIR+x}\"", "shell"])
        .arg(fixture.home.path().join(".profile")).arg(fixture.shims().join("deactivate"))
        .assert().success();
}

#[test]
fn test_deactivation_keeps_current_environment_when_uninstall_is_refused() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let startup = fixture.home.path().join(".profile");
    let text = std::fs::read_to_string(&startup)
        .unwrap()
        .replace("# >>> apexe shims", "# >>> changed");
    std::fs::write(startup, text).unwrap();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", ". \"$1\" || exit; if . \"$2\"; then exit 99; fi; test \"$APEXE_SHIM_DIR\" = \"$3\" && test \"$_APEXE_ACTIVE_SHIM_DIR\" = \"$3\"", "shell"])
        .arg(fixture.shims().join("activate")).arg(fixture.shims().join("deactivate")).arg(fixture.shims())
        .assert().success();
    assert!(fixture.shims().join("cat").exists());
}

#[test]
fn test_activation_bootstrap_enables_disables_and_reenables_from_source() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let executable = assert_cmd::cargo::cargo_bin("apexe");
    let path = std::env::join_paths([
        executable.parent().unwrap(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .unwrap();
    let startup = fixture.home.path().join(".profile");
    std::fs::write(&startup, "# Keep startup text\n").unwrap();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("SHELL", "/bin/sh")
        .env("PATH", &path).env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", "original=$PATH; . \"$1\" || exit; . \"$1\" || exit; printf 'bootstrapped\\n' | cat; . \"$2\" || exit; . \"$2\" || exit; test \"$PATH\" = \"$original\" && test -z \"${APEXE_SHIM_DIR+x}\" || exit 99; . \"$1\" || exit; . \"$2\"", "shell"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/activate.sh"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/deactivate.sh"))
        .assert().success().stdout("bootstrapped\n");
    assert!(!fixture.shims().join(".apexe-shims.json").exists());
    assert_eq!(
        std::fs::read_to_string(startup).unwrap(),
        "# Keep startup text\n"
    );
}

#[test]
fn test_deactivation_bootstrap_restores_environment_after_manual_disable() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let executable = assert_cmd::cargo::cargo_bin("apexe");
    let path = std::env::join_paths([
        executable.parent().unwrap(),
        Path::new("/usr/bin"),
        Path::new("/bin"),
    ])
    .unwrap();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("SHELL", "/bin/sh")
        .env("PATH", &path).env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", "original=$PATH; . \"$1\" || exit; command apexe env --disable || exit; . \"$2\" || exit; test \"$PATH\" = \"$original\" && test -z \"${APEXE_SHIM_DIR+x}\" && test -z \"${_APEXE_ACTIVE_SHIM_DIR+x}\"", "shell"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/activate.sh"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/deactivate.sh"))
        .assert().success();
}

#[test]
fn test_persistent_source_scripts_reenable_after_uninstall_without_checkout() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let directory = fixture.home.path().join(".apexe/bin");
    let activate = directory.join("apexe-activate.sh");
    let deactivate = directory.join("apexe-deactivate.sh");
    assert!(activate.is_file());
    assert!(deactivate.is_file());
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("SHELL", "/bin/sh")
        .env("PATH", "/usr/bin:/bin").env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", "original=$PATH; . \"$1\" || exit; . \"$2\" || exit; test \"$PATH\" = \"$original\" || exit 99; . \"$1\" || exit; printf 'persistent\\n' | cat; . \"$2\" || exit; . \"$2\" || exit; test \"$PATH\" = \"$original\" && test -z \"${APEXE_SHIM_DIR+x}\"", "shell"])
        .arg(&activate).arg(&deactivate).assert().success().stdout("persistent\n");
    assert!(activate.is_file());
    assert!(deactivate.is_file());
    assert!(directory.join(".apexe-source-scripts.json").is_file());
    assert!(!fixture.shims().join(".apexe-shims.json").exists());
}

#[test]
fn test_persistent_source_scripts_preserve_python_venv_function_and_environment() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    Command::new("/bin/sh").env("HOME", fixture.home.path()).env("SHELL", "/bin/sh")
        .env("PATH", "/venv/bin:/usr/bin:/bin").env("VIRTUAL_ENV", "/venv")
        .env_remove("APEXE_SHIM_DIR").current_dir(fixture.home.path())
        .args(["-c", "deactivate() { printf 'venv function\\n'; }; . \"$1\" || exit; . \"$2\" || exit; test \"$VIRTUAL_ENV\" = /venv && test \"$PATH\" = /venv/bin:/usr/bin:/bin || exit 99; deactivate", "shell"])
        .arg(fixture.home.path().join(".apexe/bin/apexe-activate.sh"))
        .arg(fixture.home.path().join(".apexe/bin/apexe-deactivate.sh"))
        .assert().success().stdout("venv function\n");
}

#[test]
fn test_persistent_source_script_changes_refuse_reload_before_mutating_shims() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let manifest = std::fs::read(fixture.shims().join(".apexe-shims.json")).unwrap();
    let script = fixture.home.path().join(".apexe/bin/apexe-activate.sh");
    std::fs::write(script, "modified").unwrap();
    fixture
        .command()
        .args(["env", "--reload"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("activation script"));
    assert_eq!(
        std::fs::read(fixture.shims().join(".apexe-shims.json")).unwrap(),
        manifest
    );
}

#[test]
fn test_persistent_source_script_installation_preserves_unowned_files() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let directory = fixture.home.path().join(".apexe/bin");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("apexe-activate.sh"), "user file").unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure();
    assert_eq!(
        std::fs::read_to_string(directory.join("apexe-activate.sh")).unwrap(),
        "user file"
    );
    assert!(!fixture.shims().join("cat").exists());
    assert!(!fixture.home.path().join(".profile").exists());
}

#[test]
fn test_persistent_source_scripts_reload_upgrades_old_helper_directory() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let legacy = fixture.home.path().join("legacy-source-scripts");
    std::fs::create_dir(&legacy).unwrap();
    let names = [
        "apexe-activate.sh",
        "apexe-deactivate.sh",
        ".apexe-source-scripts.json",
    ];
    for name in names {
        std::fs::rename(fixture.shims().join(name), legacy.join(name)).unwrap();
    }
    let path = fixture.shims().join(".apexe-shims.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["source_scripts_dir"] = json!(legacy);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    fixture
        .command()
        .args(["env", "--reload"])
        .assert()
        .success();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(manifest["source_scripts_dir"], json!(fixture.shims()));
    for name in names {
        assert_eq!(
            std::fs::read(fixture.shims().join(name)).unwrap(),
            std::fs::read(legacy.join(name)).unwrap()
        );
    }
}

#[test]
fn test_persistent_source_scripts_isolate_custom_shim_installations() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let original = std::fs::read(fixture.home.path().join(".apexe/bin/apexe-activate.sh")).unwrap();
    let custom = fixture.home.path().join("custom-shims");
    fixture
        .command()
        .args(["env", "--enable", "--shim-dir"])
        .arg(&custom)
        .assert()
        .success();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(custom.join(".apexe-shims.json")).unwrap()).unwrap();
    let scripts = PathBuf::from(manifest["source_scripts_dir"].as_str().unwrap());
    assert_ne!(scripts, fixture.home.path().join(".apexe/bin"));
    assert_eq!(scripts, custom);
    assert_eq!(
        std::fs::read(fixture.home.path().join(".apexe/bin/apexe-activate.sh")).unwrap(),
        original
    );
    Command::new("/bin/sh")
        .env("HOME", fixture.home.path())
        .env("SHELL", "/bin/sh")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR")
        .current_dir(fixture.home.path())
        .args([
            "-c",
            ". \"$1\" || exit; test \"$APEXE_SHIM_DIR\" = \"$3\" || exit 99; . \"$2\"",
            "shell",
        ])
        .arg(scripts.join("apexe-activate.sh"))
        .arg(scripts.join("apexe-deactivate.sh"))
        .arg(&custom)
        .assert()
        .success();
    assert!(fixture.shims().join("cat").exists());
    assert!(!custom.join("cat").exists());
}

#[test]
fn test_activation_script_quotes_custom_directory_without_executing_path_text() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let directory = fixture
        .home
        .path()
        .join("shims' $(touch injected)`touch injected` folder");
    fixture
        .command()
        .args(["env", "--enable", "--shim-dir"])
        .arg(&directory)
        .assert()
        .success();
    Command::new("/bin/sh")
        .env("HOME", fixture.home.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR")
        .current_dir(fixture.home.path())
        .args(["-c", ". \"$1\" || exit; printf 'safe\\n' | cat", "shell"])
        .arg(directory.join("activate"))
        .assert()
        .success()
        .stdout("safe\n");
    assert!(!fixture.home.path().join("injected").exists());
}

#[test]
fn test_shim_streams_raw_stdin_without_json_or_buffering() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    fixture
        .shim("cat")
        .write_stdin(vec![0, 128, 255, b'\n'])
        .assert()
        .success()
        .stdout(vec![0, 128, 255, b'\n']);
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("execution"));
    assert!(audit.contains("local"));
}

#[test]
fn test_shim_null_stdin_without_explicit_capability() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), false);
    fixture.enable();
    fixture
        .shim("cat")
        .write_stdin("must not arrive")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn test_shim_bundling_and_declared_end_of_options() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    std::fs::write(fixture.home.path().join("-file"), "hello\n").unwrap();
    fixture
        .shim("cat")
        .args(["-ns", "--", "-file"])
        .assert()
        .success()
        .stdout(predicates::str::contains("hello"));
    fixture
        .shim("cat")
        .arg("--unknown")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Undeclared flag"));
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("refusal"));
    assert!(!audit.contains("--unknown"));
}

#[test]
fn test_shim_rejects_protected_paths_and_default_acl_denial() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    fixture
        .shim("cat")
        .arg(fixture.home.path().join(".ssh/id_rsa"))
        .assert()
        .failure();
    std::fs::write(
        fixture.home.path().join(".apexe/acl.yaml"),
        "version: '1.0'\ndefault_effect: deny\nrules: []\n",
    )
    .unwrap();
    fixture
        .shim("cat")
        .write_stdin("blocked")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Access denied"));
}

#[test]
fn test_shim_propagates_exact_child_exit_status() {
    let fixture = Fixture::new();
    let script = fixture.script("failure-bin", "exit 7");
    fixture.write(
        "cli.failure",
        "failure",
        script.to_str().unwrap(),
        json!({"type":"object","properties":{}}),
        false,
    );
    fixture.enable();
    fixture
        .shim("failure")
        .assert()
        .failure()
        .code(7)
        .stdout("");
}

#[test]
fn test_shim_restore_sigpipe_exits_quietly_on_closed_reader() {
    let fixture = Fixture::new();
    fixture.write(
        "cli.yes",
        "yes",
        "/usr/bin/yes",
        json!({"type":"object","properties":{}}),
        false,
    );
    fixture.enable();
    let mut child = std::process::Command::new(fixture.shims().join("yes"))
        .env("HOME", fixture.home.path())
        .current_dir(fixture.home.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut bytes = [0; 32];
    stdout.read_exact(&mut bytes).unwrap();
    drop(stdout);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(141));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Broken pipe"));
}

#[test]
fn test_shim_missing_module_fails_without_native_fallback() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    for entry in std::fs::read_dir(fixture.bindings()).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    fixture
        .shim("cat")
        .write_stdin("must not escape")
        .assert()
        .failure()
        .stdout("")
        .stderr(predicates::str::contains("no longer registered"));
}

#[test]
fn test_enable_refuses_unowned_files_and_ambiguous_names() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    std::fs::create_dir_all(fixture.shims()).unwrap();
    std::fs::write(fixture.shims().join("cat"), "owned by someone else").unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure();
    assert_eq!(
        std::fs::read_to_string(fixture.shims().join("cat")).unwrap(),
        "owned by someone else"
    );
    std::fs::remove_file(fixture.shims().join("cat")).unwrap();
    fixture.write("other.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("multiple modules"));
}

#[test]
fn test_enable_activation_is_idempotent_and_disable_is_owned_only() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    fixture.enable();
    let path = std::env::join_paths([
        fixture.shims(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();
    let output = fixture
        .command()
        .env("PATH", &path)
        .arg("env")
        .assert()
        .success()
        .get_output()
        .clone();
    let activation = String::from_utf8(output.stdout).unwrap();
    let exported_path = activation
        .lines()
        .find(|line| line.starts_with("export PATH="))
        .unwrap();
    assert_eq!(
        exported_path
            .matches(fixture.shims().to_str().unwrap())
            .count(),
        1
    );
    std::fs::write(fixture.shims().join("unrelated"), "keep").unwrap();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert!(!fixture.shims().join("cat").exists());
    assert!(fixture.shims().join("unrelated").exists());
}

#[test]
fn test_enable_new_bash_terminal_loads_shims_and_keeps_runtime_path() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    std::fs::write(fixture.home.path().join("sample.txt"), "new terminal\n").unwrap();
    let startup = fixture.home.path().join(".bashrc");
    let output = Command::new("/bin/bash")
        .env("HOME", fixture.home.path())
        .env("PATH", "/opt/runtime-added:/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR")
        .current_dir(fixture.home.path())
        .args(["--noprofile", "--rcfile"])
        .arg(&startup)
        .args([
            "-i",
            "-c",
            "source \"$1\"; source \"$1\"; cat sample.txt; printf 'PATH=%s\\n' \"$PATH\"",
            "bash",
        ])
        .arg(&startup)
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("new terminal\nPATH="));
    assert!(stdout.contains("/opt/runtime-added:/usr/bin:/bin"));
    assert_eq!(stdout.matches(fixture.shims().to_str().unwrap()).count(), 1);
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("cli.cat"));
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert!(!std::fs::read_to_string(startup)
        .unwrap()
        .contains("# >>> apexe shims"));
    assert!(!fixture.shims().join("cat").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn test_enable_new_zsh_terminal_automatically_loads_shims() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .env_remove("ZDOTDIR")
        .args(["env", "--enable", "--shell", "zsh"])
        .assert()
        .success()
        .stdout("");
    std::fs::write(fixture.home.path().join("sample.txt"), "zsh terminal\n").unwrap();
    Command::new("/bin/zsh")
        .env("HOME", fixture.home.path())
        .env("ZDOTDIR", fixture.home.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR")
        .current_dir(fixture.home.path())
        .args(["-i", "-c", "cat sample.txt"])
        .assert()
        .success()
        .stdout("zsh terminal\n");
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("cli.cat"));
}

#[test]
fn test_enable_disable_refuses_changed_startup_before_removing_shims() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "sh"])
        .assert()
        .success();
    let startup = fixture.home.path().join(".profile");
    let modified = std::fs::read_to_string(&startup)
        .unwrap()
        .replace("--shell sh", "--shell fish");
    std::fs::write(&startup, &modified).unwrap();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .failure();
    assert!(fixture.shims().join("cat").exists());
    assert!(fixture.shims().join(".apexe-shims.json").exists());
    assert_eq!(std::fs::read_to_string(startup).unwrap(), modified);
}

#[test]
fn test_enable_shell_quotes_installed_custom_directory() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let directory = fixture
        .home
        .path()
        .join("shims' $(touch injected)`touch injected` folder");
    fixture
        .command()
        .args(["env", "--enable", "--shell", "sh", "--shim-dir"])
        .arg(&directory)
        .assert()
        .success();
    std::fs::write(fixture.home.path().join("sample.txt"), "quoted directory\n").unwrap();
    Command::new("/bin/sh")
        .env("HOME", fixture.home.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(fixture.home.path())
        .arg("-c")
        .arg(". \"$1\"; cat sample.txt")
        .arg("sh")
        .arg(fixture.home.path().join(".profile"))
        .assert()
        .success()
        .stdout("quoted directory\n");
    assert!(!fixture.home.path().join("injected").exists());
}

#[test]
fn test_reload_adds_commands_without_evaluating_activation() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    let helper = fixture.script("new-command-bin", "printf 'daily update\\n'");
    fixture.write(
        "cli.newcmd",
        "newcmd",
        helper.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    Command::new("/bin/bash")
        .env("HOME", fixture.home.path())
        .env("SHELL", "/bin/bash")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR")
        .current_dir(fixture.home.path())
        .args(["--noprofile", "--rcfile"])
        .arg(fixture.home.path().join(".bashrc"))
        .args(["-i", "-c", "\"$1\" env --reload && newcmd", "bash"])
        .arg(env!("CARGO_BIN_EXE_apexe"))
        .assert()
        .success()
        .stdout("daily update\n");
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("cli.newcmd"));
}

#[test]
fn test_enable_is_idempotent_and_disable_preserves_other_configuration() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let startup = fixture.home.path().join("custom.rc");
    std::fs::write(&startup, "export CUSTOM=preserved").unwrap();
    for _ in 0..2 {
        fixture
            .command()
            .args(["env", "--enable", "--shell", "bash", "--rc-file"])
            .arg(&startup)
            .assert()
            .success()
            .stdout("");
    }
    let content = std::fs::read_to_string(&startup).unwrap();
    assert_eq!(content.matches("# >>> apexe shims").count(), 1);
    assert!(fixture.shims().join("cat").exists());
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert!(!fixture.shims().join("cat").exists());
    assert!(!fixture.shims().join(".apexe-shims.json").exists());
    assert!(!fixture.shims().join(".apexe-shell-init.json").exists());
    assert_eq!(
        std::fs::read_to_string(startup).unwrap(),
        "export CUSTOM=preserved"
    );
}

#[test]
fn test_enable_validates_startup_ownership_before_installing_shims() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    // Retain only startup state to verify validation happens before fresh link creation.
    std::fs::remove_file(fixture.shims().join("cat")).unwrap();
    std::fs::remove_file(fixture.shims().join(".apexe-shims.json")).unwrap();
    let startup = fixture.home.path().join(".bashrc");
    let modified = std::fs::read_to_string(&startup)
        .unwrap()
        .replace("--shell sh", "--shell fish");
    std::fs::write(&startup, &modified).unwrap();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .failure();
    assert!(!fixture.shims().join("cat").exists());
    assert!(!fixture.shims().join(".apexe-shims.json").exists());
    assert_eq!(std::fs::read_to_string(startup).unwrap(), modified);
}

#[test]
fn test_enable_install_failure_preserves_startup_and_unowned_files() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let startup = fixture.home.path().join(".bashrc");
    std::fs::write(&startup, "export CUSTOM=retained\n").unwrap();
    std::fs::create_dir_all(fixture.shims()).unwrap();
    std::fs::write(fixture.shims().join("cat"), "unowned command").unwrap();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .failure();
    assert_eq!(
        std::fs::read_to_string(startup).unwrap(),
        "export CUSTOM=retained\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.shims().join("cat")).unwrap(),
        "unowned command"
    );
    assert!(!fixture.shims().join(".apexe-shell-init.json").exists());
}

#[test]
fn test_enable_unknown_shell_never_installs_command_links() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .env("SHELL", "/bin/unknown")
        .args(["env", "--enable"])
        .assert()
        .failure();
    assert!(!fixture.shims().join("cat").exists());
    assert!(!fixture.shims().join(".apexe-shims.json").exists());
}

#[test]
fn test_enable_and_disable_repeat_without_duplicate_startup_blocks() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let startup = fixture.home.path().join("custom.rc");
    for _ in 0..2 {
        fixture
            .command()
            .args(["env", "--enable", "--shell", "bash", "--rc-file"])
            .arg(&startup)
            .assert()
            .success()
            .stdout("");
    }
    assert!(fixture.shims().join("cat").exists());
    assert_eq!(
        std::fs::read_to_string(&startup)
            .unwrap()
            .matches("# >>> apexe shims")
            .count(),
        1
    );
    fixture
        .command()
        .args(["env", "--enable", "--disable"])
        .assert()
        .failure();
    fixture
        .command()
        .args(["env", "--disable", "--enable"])
        .assert()
        .failure();
    for _ in 0..2 {
        fixture
            .command()
            .args(["env", "--disable"])
            .assert()
            .success();
    }
    assert!(!fixture.shims().join("cat").exists());
    assert_eq!(std::fs::read_to_string(startup).unwrap(), "");
}

#[test]
fn test_reload_automatically_enables_before_install_and_after_disable() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let startup = fixture.home.path().join("custom.rc");
    std::fs::write(&startup, "export CUSTOM=preserved\n").unwrap();
    for _ in 0..2 {
        fixture
            .command()
            .args(["env", "--reload", "--shell", "bash", "--rc-file"])
            .arg(&startup)
            .assert()
            .success()
            .stdout("");
        assert!(fixture.shims().join("cat").exists());
        assert_eq!(
            std::fs::read_to_string(&startup)
                .unwrap()
                .matches("# >>> apexe shims")
                .count(),
            1
        );
        fixture
            .command()
            .args(["env", "--disable"])
            .assert()
            .success();
        assert_eq!(
            std::fs::read_to_string(&startup).unwrap(),
            "export CUSTOM=preserved\n"
        );
    }
}

#[test]
fn test_reload_preserves_bindings_acl_filters_and_pattern_without_reinitializing() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.write("ignored.extra", "extra", "/bin/cat", file_schema(), true);
    let bindings = fixture.home.path().join("custom-bindings");
    std::fs::rename(fixture.bindings(), &bindings).unwrap();
    let acl = fixture.home.path().join("custom-acl.yaml");
    std::fs::write(&acl, "version: '1.0'\ndefault_effect: allow\nrules: []\n").unwrap();
    let config = fixture.home.path().join(".apexe/config.yaml");
    std::fs::write(&config, "bindings_pattern: '*.binding.yaml'\n").unwrap();
    enable_filtered_bindings(&fixture, &bindings, &acl);
    let manifest_path = fixture.shims().join(".apexe-shims.json");
    let previous: Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let startup = fixture.home.path().join(".bashrc");
    let original_startup = std::fs::read_to_string(&startup).unwrap();
    let helper = fixture.script("new-command-bin", "printf 'reloaded\\n'");
    fixture.write(
        "cli.newcmd",
        "newcmd",
        helper.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    std::fs::rename(
        fixture.bindings().join("cli.newcmd.binding.yaml"),
        bindings.join("cli.newcmd.binding.yaml"),
    )
    .unwrap();
    std::fs::write(config, "bindings_pattern: 'missing-*.binding.yaml'\n").unwrap();
    fixture
        .command()
        .env("SHELL", "/bin/unknown")
        .args(["env", "--reload"])
        .assert()
        .success()
        .stdout("");
    let updated: Value = serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    for field in ["bindings_dir", "bindings_pattern", "acl", "filter"] {
        assert_eq!(updated[field], previous[field]);
    }
    assert!(updated["commands"].get("newcmd").is_some());
    assert!(updated["commands"].get("extra").is_none());
    assert_eq!(std::fs::read_to_string(startup).unwrap(), original_startup);
    fixture
        .shim("newcmd")
        .assert()
        .success()
        .stdout("reloaded\n");
}

fn enable_filtered_bindings(fixture: &Fixture, bindings: &Path, acl: &Path) {
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash", "--bindings-dir"])
        .arg(bindings)
        .arg("--acl")
        .arg(acl)
        .args(["--prefix", "cli", "--tags", "pilot"])
        .assert()
        .success();
}

#[test]
fn test_reload_explicit_filter_override_updates_admission_and_keeps_tombstones() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    let helper = fixture.script("other-command", "printf 'other scope\\n'");
    fixture.write(
        "other.newcmd",
        "newcmd",
        helper.to_str().unwrap(),
        json!({"type":"object","properties":{},"additionalProperties":false}),
        false,
    );
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash", "--prefix", "cli"])
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--reload", "--prefix", "other"])
        .assert()
        .success();
    fixture
        .shim("newcmd")
        .assert()
        .success()
        .stdout("other scope\n");
    fixture
        .shim("cat")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no longer registered"));
    assert!(fixture.shims().join("cat").exists());
}

#[test]
fn test_reload_failure_preserves_existing_installation_and_startup() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    let manifest_path = fixture.shims().join(".apexe-shims.json");
    let previous_manifest = std::fs::read(&manifest_path).unwrap();
    let startup = fixture.home.path().join(".bashrc");
    let previous_startup = std::fs::read_to_string(&startup).unwrap();
    fixture
        .command()
        .args(["env", "--reload", "--bindings-dir", "missing-bindings"])
        .assert()
        .failure();
    assert_eq!(std::fs::read(manifest_path).unwrap(), previous_manifest);
    assert_eq!(std::fs::read_to_string(startup).unwrap(), previous_startup);
    fixture
        .shim("cat")
        .write_stdin("still enabled\n")
        .assert()
        .success()
        .stdout("still enabled\n");
}

#[test]
fn test_reload_explicit_startup_override_configures_and_cleans_another_shell() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    let original = std::fs::read_to_string(fixture.home.path().join(".bashrc")).unwrap();
    let startup = fixture.home.path().join("fish.rc");
    fixture
        .command()
        .args(["env", "--reload", "--shell", "fish", "--rc-file"])
        .arg(&startup)
        .assert()
        .success();
    assert!(std::fs::read_to_string(&startup)
        .unwrap()
        .contains("--shell fish | source"));
    assert_eq!(
        std::fs::read_to_string(fixture.home.path().join(".bashrc")).unwrap(),
        original
    );
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(startup).unwrap(), "");
    assert_eq!(
        std::fs::read_to_string(fixture.home.path().join(".bashrc")).unwrap(),
        ""
    );
}

#[test]
fn test_reload_corrupt_manifest_is_not_treated_as_disabled() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    let manifest = fixture.shims().join(".apexe-shims.json");
    std::fs::write(&manifest, "{invalid json}").unwrap();
    let startup = fixture.home.path().join(".bashrc");
    let original = std::fs::read_to_string(&startup).unwrap();
    fixture
        .command()
        .args(["env", "--reload"])
        .assert()
        .failure();
    assert_eq!(std::fs::read_to_string(manifest).unwrap(), "{invalid json}");
    assert_eq!(std::fs::read_to_string(startup).unwrap(), original);
    assert!(fixture.shims().join("cat").exists());
}

#[test]
fn test_enable_disable_keeps_startup_when_shim_ownership_fails() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    let startup = fixture.home.path().join(".bashrc");
    let original = std::fs::read_to_string(&startup).unwrap();
    std::fs::remove_file(fixture.shims().join("cat")).unwrap();
    std::fs::write(fixture.shims().join("cat"), "unowned replacement").unwrap();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .failure();
    assert_eq!(std::fs::read_to_string(startup).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(fixture.shims().join("cat")).unwrap(),
        "unowned replacement"
    );
}

#[test]
fn test_child_path_strips_shims_and_preserves_absolute_binary() {
    let fixture = Fixture::new();
    let helper = fixture.script("path-bin", "printf '%s' \"$PATH\"");
    fixture.write(
        "cli.inspect",
        "inspect",
        helper.to_str().unwrap(),
        json!({"type":"object","properties":{}}),
        false,
    );
    fixture.enable();
    let path = std::env::join_paths([
        fixture.shims(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();
    let output = fixture
        .shim("inspect")
        .env("PATH", path)
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(fixture.shims().to_str().unwrap()));
}

#[test]
fn test_scan_ignores_a_managed_shim_directory() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let path = std::env::join_paths([
        fixture.shims(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();
    let output = fixture
        .command()
        .env("PATH", path)
        .args(["scan", "cat", "--no-cache", "--dry-run", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!report["tools"][0]["binary_path"]
        .as_str()
        .unwrap()
        .contains(fixture.shims().to_str().unwrap()));
}

#[test]
fn test_shim_is_a_real_shell_pipeline_stage() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    let path = std::env::join_paths([
        fixture.shims(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();
    let mut command = Command::new("/bin/sh");
    command
        .env("HOME", fixture.home.path())
        .env("PATH", path)
        .current_dir(fixture.home.path())
        .args(["-c", "printf 'one\ntwo\n' | cat | /usr/bin/head -n 1"])
        .assert()
        .success()
        .stdout("one\n");
}

#[tokio::test]
async fn test_passthrough_result_has_no_captured_streams() {
    use apcore::Module;
    use apexe::module::{CliModule, ExecutionIo, StdinMode};
    let scanned = ScannedModule::new(
        "cli.true".into(),
        "No output".into(),
        json!({"type":"object"}),
        json!({"type":"object"}),
        vec![],
        "exec:///usr/bin/true".into(),
    );
    let module = CliModule::from_scanned(&scanned, 5000)
        .unwrap()
        .with_execution_io(ExecutionIo::Passthrough {
            stdin: StdinMode::Null,
        });
    let result = module
        .execute(json!({}), &apcore::Context::anonymous())
        .await
        .unwrap();
    assert_eq!(result["io"], "passthrough");
    assert_eq!(result["exit_code"], 0);
    assert!(result.get("stdout").is_none());
    assert!(result.get("stderr").is_none());
}

#[tokio::test]
async fn test_stdin_inheritance_requires_explicit_module_capability() {
    use apcore::Module;
    use apexe::module::{CliModule, ExecutionIo, StdinMode};
    let scanned = ScannedModule::new(
        "cli.cat".into(),
        "No stdin capability".into(),
        json!({"type":"object"}),
        json!({"type":"object"}),
        vec![],
        "exec:///bin/cat".into(),
    );
    let module = CliModule::from_scanned(&scanned, 5000)
        .unwrap()
        .with_execution_io(ExecutionIo::Passthrough {
            stdin: StdinMode::Inherit,
        });
    let error = module
        .execute(json!({}), &apcore::Context::anonymous())
        .await
        .unwrap_err();
    assert!(error.message.contains("streaming_stdin_safe"));
}

#[test]
fn test_enable_excludes_unasserted_or_order_sensitive_grammar() {
    let fixture = Fixture::new();
    let mut schema = file_schema();
    schema["x-apexe-cross-flag-ordering"] = json!(true);
    fixture.write("cli.cat", "cat", "/bin/cat", schema, true);
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0].metadata.get_mut("shim_contract").unwrap()["cross_flag_ordering"] = Value::Null;
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure();
    assert!(!fixture.shims().join("cat").exists());
}

#[test]
fn test_shim_subcommand_refusal_never_falls_back_to_root() {
    let fixture = Fixture::new();
    let binary = fixture.script("dispatch-bin", "printf '%s' \"$*\"");
    fixture.write("cli.tool", "tool", binary.to_str().unwrap(), json!({"type":"object","properties":{"args":{"type":"array","items":{"type":"string"},"x-apexe-positional":0}}}), false);
    fixture.write(
        "cli.tool.danger",
        "tool",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{"count":{"type":"integer","x-apexe-positional":0}}}),
        false,
    );
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    let leaf = modules
        .iter_mut()
        .find(|module| module.module_id == "cli.tool.danger")
        .unwrap();
    leaf.target.push_str(" danger");
    leaf.metadata
        .insert("command_path".into(), json!(["tool", "danger"]));
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture.enable();
    fixture
        .shim("tool")
        .args(["danger", "not-an-integer"])
        .assert()
        .failure()
        .stdout("");
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    let refusal: Value = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
    assert_eq!(refusal["module_id"], "cli.tool.danger");
    assert_eq!(refusal["event"], "refusal");

    fixture
        .shim("tool")
        .args(["danger", "5"])
        .assert()
        .success()
        .stdout("danger 5");
}

fn routing_fixture() -> Fixture {
    let fixture = Fixture::new();
    let binary = fixture.script("dispatch-bin", "printf '%s' \"$*\"");
    fixture.write(
        "cli.tool",
        "tool",
        binary.to_str().unwrap(),
        json!({"type":"object","x-apexe-end-of-options":true,"properties":{
            "args":{"type":"array","items":{"type":"string"},"x-apexe-positional":0},
            "help":{"type":"boolean","x-apexe-flag":"--help"},
            "setting":{"type":"string","x-apexe-flag":"--setting"}
        },"additionalProperties":false}),
        false,
    );
    fixture.write(
        "cli.tool.danger", "tool", binary.to_str().unwrap(),
        json!({"type":"object","properties":{
            "count":{"type":"integer","x-apexe-positional":0},
            "directory":{"type":"string","x-apexe-flag":"-C","x-apexe-flag-position":"before-subcommand"}
        },"additionalProperties":false}), false,
    );
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    let leaf = modules
        .iter_mut()
        .find(|module| module.module_id == "cli.tool.danger")
        .unwrap();
    leaf.target.push_str(" danger");
    leaf.metadata
        .insert("command_path".into(), json!(["tool", "danger"]));
    leaf.tags.push("restricted".into());
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    std::fs::write(fixture.home.path().join(".apexe/acl.yaml"),
        "default_effect: deny\nrules:\n  - callers: ['*']\n    targets: [cli.tool]\n    effect: allow\n  - callers: ['*']\n    targets: [cli.tool.danger]\n    effect: deny\n").unwrap();
    fixture.enable();
    fixture
}

#[test]
fn test_reload_retains_removed_subcommand_tombstone() {
    let fixture = routing_fixture();
    fixture
        .shim("tool")
        .args(["danger", "5"])
        .assert()
        .failure()
        .stdout("");
    std::fs::remove_file(fixture.bindings().join("cli.tool.danger.binding.yaml")).unwrap();
    for _ in 0..2 {
        fixture
            .command()
            .args(["env", "--reload"])
            .assert()
            .success();
        fixture
            .shim("tool")
            .args(["danger", "5"])
            .assert()
            .failure()
            .stdout("")
            .stderr(predicates::str::contains("no longer registered"));
        fixture
            .shim("tool")
            .arg("--help")
            .assert()
            .success()
            .stdout("--help");
    }
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(fixture.shims().join(".apexe-shims.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["commands"]["tool"].as_array().unwrap().len(), 2);
}

#[test]
fn test_reload_retains_filtered_subcommand_tombstone() {
    let fixture = routing_fixture();
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules
        .iter_mut()
        .find(|module| module.module_id == "cli.tool")
        .unwrap()
        .tags
        .push("root-only".into());
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture
        .command()
        .args(["env", "--reload", "--tags", "root-only"])
        .assert()
        .success();
    fixture
        .shim("tool")
        .args(["danger", "5"])
        .assert()
        .failure()
        .stdout("")
        .stderr(predicates::str::contains("no longer registered"));
    fixture
        .shim("tool")
        .arg("--help")
        .assert()
        .success()
        .stdout("--help");
}

#[test]
fn test_shim_root_flags_are_not_validated_against_unrelated_subcommands() {
    let fixture = routing_fixture();
    fixture
        .shim("tool")
        .arg("--help")
        .assert()
        .success()
        .stdout("--help");
    fixture
        .shim("tool")
        .args(["--setting", "danger"])
        .assert()
        .success()
        .stdout("--setting danger");
    fixture
        .shim("tool")
        .args(["--setting=danger"])
        .assert()
        .success()
        .stdout("--setting danger");
}

#[test]
fn test_shim_root_flags_cannot_hide_a_selected_subcommand() {
    let fixture = routing_fixture();
    for arguments in [
        vec!["--help", "danger", "5"],
        vec!["--setting", "value", "danger", "5"],
        vec!["--setting=value", "danger", "5"],
        vec!["--", "danger", "5"],
    ] {
        fixture
            .shim("tool")
            .args(arguments)
            .assert()
            .failure()
            .stdout("");
    }
}

#[test]
fn test_shim_global_flag_values_do_not_count_as_subcommands() {
    let fixture = routing_fixture();
    fixture
        .shim("tool")
        .args(["-C", "danger", "danger", "5"])
        .assert()
        .failure()
        .stdout("")
        .stderr(predicates::str::contains("Access denied"));
    fixture
        .shim("tool")
        .args(["--unknown", "danger", "5"])
        .assert()
        .failure()
        .stdout("")
        .stderr(predicates::str::contains("Undeclared flag"));
}

#[test]
fn test_shim_preserves_child_stderr_and_never_appends_json_flag() {
    let fixture = Fixture::new();
    let binary = fixture.script(
        "raw-bin",
        "test \"$#\" -eq 0 || exit 9; printf raw; printf diagnostic >&2",
    );
    fixture.write(
        "cli.raw",
        "raw.exe",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{}}),
        false,
    );
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0]
        .metadata
        .insert("json_flag".into(), json!("--json"));
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture.enable();
    fixture
        .shim("raw.exe")
        .env("RUST_LOG", "error")
        .assert()
        .success()
        .stdout("raw")
        .stderr("diagnostic");
}

/// Run the timed-out shim until the child has announced its pid, and return it.
///
/// The child writes its pid only once the machine has scheduled it that far, and
/// with many CLI integration processes running concurrently the timeout can fire
/// first. That outcome says nothing about the timeout itself, so the scenario is
/// retried within the same fixture. The elapsed time is checked on every attempt
/// so a timeout that fires far too early -- the defect this test exists to catch
/// -- still fails instead of being retried away.
fn timed_out_child_pid(fixture: &Fixture, pid_file: &Path) -> i32 {
    let mut missing = None;
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let assertion = fixture
            .shim("slow")
            .env("APEXE_TIMEOUT", "5")
            .timeout(std::time::Duration::from_secs(15))
            .assert()
            .failure()
            .stderr(predicates::str::contains("timed out"));
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(4),
            "the timeout must run its full length; output: {:?}",
            assertion.get_output()
        );
        match std::fs::read_to_string(pid_file) {
            Ok(pid) => return pid.trim().parse().unwrap(),
            Err(error) => missing = Some(error),
        }
    }
    panic!("the child never announced its pid in {pid_file:?}: {missing:?}");
}

#[test]
fn test_shim_timeout_kills_the_direct_child_and_audits_error() {
    let fixture = Fixture::new();
    let pid_file = fixture.home.path().join("child.pid");
    let binary = fixture.script(
        "slow-bin",
        &format!("echo $$ > {}; exec /bin/sleep 30", pid_file.display()),
    );
    fixture.write(
        "cli.slow",
        "slow",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{}}),
        false,
    );
    fixture.enable();
    let pid = timed_out_child_pid(&fixture, &pid_file);
    // SAFETY: kill with signal zero probes existence and does not signal the process.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    let entry: Value = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
    assert_eq!(entry["event"], "execution");
    assert_eq!(entry["status"], "error");
    assert_eq!(entry["exit_code"], -1);
    assert_eq!(entry["caller_id"], "local");
}

#[test]
fn test_shim_approval_required_is_refused_without_spawning() {
    let fixture = Fixture::new();
    let marker = fixture.home.path().join("spawned");
    let binary = fixture.script("approval-bin", &format!("touch {}", marker.display()));
    fixture.write(
        "cli.approval",
        "approval",
        binary.to_str().unwrap(),
        json!({"type":"object","properties":{}}),
        false,
    );
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0].annotations.as_mut().unwrap().requires_approval = true;
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    fixture.enable();
    fixture
        .shim("approval")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no MCP elicitation support"));
    assert!(!marker.exists());
}

#[test]
fn test_plain_shell_command_prefix_still_invokes_the_path_shim() {
    let fixture = Fixture::new();
    fixture.write("cli.cat", "cat", "/bin/cat", file_schema(), true);
    fixture.enable();
    std::fs::write(fixture.home.path().join("sample.txt"), "governed\n").unwrap();
    let path = std::env::join_paths([
        fixture.shims(),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/bin"),
    ])
    .unwrap();
    Command::new("/bin/sh")
        .env("HOME", fixture.home.path())
        .env("PATH", path)
        .current_dir(fixture.home.path())
        .args(["-c", "command cat sample.txt"])
        .assert()
        .success()
        .stdout("governed\n");
    let audit = std::fs::read_to_string(fixture.home.path().join(".apexe/audit.jsonl")).unwrap();
    assert!(audit.contains("execution"));
    assert!(audit.contains("cli.cat"));
}
