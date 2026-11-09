#![cfg(unix)]
use assert_cmd::Command;
use predicates::str::contains;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[test]
fn test_env_rejects_removed_parameters() {
    use clap::Parser;
    for flag in ["--load", "--unload", "--install", "--init", "--uninstall"] {
        for operation in [None, Some("--enable"), Some("--reload"), Some("--disable")] {
            let mut arguments = vec!["apexe", "env", flag];
            if let Some(operation) = operation {
                arguments.push(operation);
            }
            let error = apexe::cli::Cli::try_parse_from(arguments).unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{flag}"
            );
        }
    }
}

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().unwrap(),
        };
        fixture.write_binding();
        fixture
    }

    fn write_binding(&self) {
        use serde_json::json;
        let mut module = apcore_toolkit::ScannedModule::new(
            "cli.cat".into(),
            "Shell activation fixture".into(),
            json!({"type":"object","properties":{},"additionalProperties":false,
                   "x-apexe-short-flag-bundling":false,"x-apexe-streaming-stdin-safe":false,
                   "x-apexe-cross-flag-ordering":false}),
            json!({"type":"object"}),
            vec![],
            "exec:///bin/cat".into(),
        );
        module.annotations = Some(apcore::ModuleAnnotations {
            readonly: true,
            ..Default::default()
        });
        module
            .metadata
            .insert("command_path".into(), json!(["cat"]));
        // A current binding carries the per-route grammar stamp; without it the
        // route is `Legacy` -- admitted like a scanned one, never reviewed (#47).
        module.metadata.insert("grammar".into(), json!("reviewed"));
        module.metadata.insert("variant".into(), json!("unknown"));
        module.metadata.insert(
            "shim_contract".into(),
            json!({"authoritative":true,"variant":"unknown",
            "short_flag_bundling":false,"streaming_stdin_safe":false,"cross_flag_ordering":false}),
        );
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.path(".apexe/bindings"), false)
            .unwrap();
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home.path().join(relative)
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/zsh")
            .env_remove("ZDOTDIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "info")
            .arg("--log-stderr")
            .current_dir(self.home.path());
        command
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path(relative)).unwrap()
    }
}

#[test]
fn test_enable_detects_zsh_and_is_idempotent() {
    let fixture = Fixture::new();
    fixture.write(".zshrc", "export CUSTOM_SETTING=retained");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success()
        .stdout("")
        .stderr(contains("Open a new terminal"));
    let first = fixture.read(".zshrc");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    assert_eq!(fixture.read(".zshrc"), first);
    assert!(first.starts_with("export CUSTOM_SETTING=retained\n"));
    assert_eq!(first.matches("# >>> apexe shims").count(), 1);
    assert!(fixture.path(".apexe/bin/.apexe-shims.json").exists());
    assert!(fixture.path(".apexe/bin/cat").exists());
    std::fs::remove_file(fixture.path(".apexe/bin/.apexe-shims.json")).unwrap();
    // A missing manifest makes the startup guard silent and preserves PATH.
    Command::new("/bin/sh")
        .env("PATH", "/usr/bin:/bin")
        .arg("-c")
        .arg(". \"$1\"; printf '%s' \"$PATH\"")
        .arg("sh")
        .arg(fixture.path(".zshrc"))
        .assert()
        .success()
        .stdout("/usr/bin:/bin")
        .stderr("");
}

#[test]
fn test_enable_zsh_respects_zdotdir() {
    let fixture = Fixture::new();
    let directory = fixture.path("zsh settings");
    fixture
        .command()
        .env("ZDOTDIR", &directory)
        .args(["env", "--enable"])
        .assert()
        .success();
    assert!(directory.join(".zshrc").exists());
    assert!(directory.join(".zprofile").exists());
    assert!(!fixture.path(".zshrc").exists());
    assert!(!fixture.path(".zprofile").exists());
}

#[test]
fn test_enable_zsh_login_profile_is_idempotent_and_restored_on_disable() {
    let fixture = Fixture::new();
    let original = "# Existing login settings\nexport PATH=/usr/bin:/bin";
    fixture.write(".zprofile", original);
    for _ in 0..2 {
        fixture
            .command()
            .args(["env", "--enable"])
            .assert()
            .success();
    }
    let installed = fixture.read(".zprofile");
    assert!(installed.starts_with(&format!("{original}\n")));
    assert_eq!(installed.matches("# >>> apexe shims").count(), 1);
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(fixture.read(".zprofile"), original);
}

#[test]
fn test_reload_zsh_upgrades_interactive_only_startup_and_preserves_policy() {
    let fixture = Fixture::new();
    fixture.write("policy.yaml", "rules: []\ndefault_effect: deny\n");
    fixture
        .command()
        .args(["env", "--enable", "--acl", "policy.yaml"])
        .assert()
        .success();
    let original_manifest: serde_json::Value =
        serde_json::from_str(&fixture.read(".apexe/bin/.apexe-shims.json")).unwrap();
    let interactive = fixture.read(".zshrc");
    let state_path = fixture.path(".apexe/bin/.apexe-shell-init.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    state["files"]
        .as_array_mut()
        .unwrap()
        .retain(|file| !file["path"].as_str().unwrap().ends_with("/.zprofile"));
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    fixture.write(".zprofile", "# Existing login profile\n");
    fixture
        .command()
        .args(["env", "--reload", "--shell", "zsh"])
        .assert()
        .success();
    assert!(fixture.read(".zprofile").contains("# >>> apexe shims"));
    assert_eq!(fixture.read(".zshrc"), interactive);
    let manifest: serde_json::Value =
        serde_json::from_str(&fixture.read(".apexe/bin/.apexe-shims.json")).unwrap();
    assert_eq!(manifest["acl"], original_manifest["acl"]);
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(fixture.read(".zprofile"), "# Existing login profile\n");
}

#[test]
fn test_disable_zsh_refuses_modified_login_block_before_removing_shims() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    let original = fixture.read(".zprofile");
    let modified = original.replace("--shell sh", "--shell fish");
    assert_ne!(modified, original);
    fixture.write(".zprofile", &modified);
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .failure()
        .stderr(contains("was modified"));
    assert!(fixture.path(".apexe/bin/cat").exists());
    assert!(fixture.read(".zshrc").contains("# >>> apexe shims"));
    assert_eq!(fixture.read(".zprofile"), modified);
}

#[cfg(target_os = "macos")]
#[test]
fn test_enable_zsh_noninteractive_login_routes_commands_after_profile_path_reset() {
    let fixture = Fixture::new();
    fixture.write(".zprofile", "export PATH=/usr/bin:/bin\n");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    Command::new("/bin/zsh")
        .env("HOME", fixture.home.path())
        .env("ZDOTDIR", fixture.home.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("APEXE_SHIM_DIR")
        .current_dir(fixture.home.path())
        .args(["-lc", "command -v cat; cat"])
        .assert()
        .success()
        .stdout(format!("{}/cat\n", fixture.path(".apexe/bin").display()))
        .stderr("");
    let audit = fixture.read(".apexe/audit.jsonl");
    assert!(audit.contains("\"module_id\":\"cli.cat\""));
    assert!(audit.contains("\"status\":\"success\""));
}

#[test]
fn test_enable_bash_configures_interactive_and_existing_login_startup() {
    let fixture = Fixture::new();
    fixture.write(".bash_login", "# Existing login configuration\n");
    fixture.write(".profile", "# Lower-priority login configuration\n");
    fixture
        .command()
        .env("SHELL", "/bin/bash")
        .args(["env", "--enable"])
        .assert()
        .success();
    assert!(fixture.read(".bashrc").contains("# >>> apexe shims"));
    assert!(fixture.read(".bash_login").contains("# >>> apexe shims"));
    assert!(!fixture.path(".bash_profile").exists());
    assert_eq!(
        fixture.read(".profile"),
        "# Lower-priority login configuration\n"
    );
}

#[test]
fn test_enable_bash_creates_login_profile_when_none_exists() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    assert!(fixture.path(".bashrc").is_file());
    assert!(fixture.path(".bash_profile").is_file());
}

#[test]
fn test_enable_fish_respects_xdg_config_home() {
    let fixture = Fixture::new();
    let directory = fixture.path("configuration");
    fixture
        .command()
        .env("XDG_CONFIG_HOME", &directory)
        .args(["env", "--enable", "--shell", "fish"])
        .assert()
        .success();
    let content = std::fs::read_to_string(directory.join("fish/config.fish")).unwrap();
    assert!(content.contains("--shell fish | source"));
    assert!(!fixture.path(".config/fish/config.fish").exists());
}

#[test]
fn test_enable_sh_uses_login_profile_and_explicit_rc_file_overrides_defaults() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "sh"])
        .assert()
        .success();
    assert!(fixture.path(".profile").exists());
    fixture
        .command()
        .args([
            "env",
            "--enable",
            "--shell",
            "bash",
            "--rc-file",
            "startup/custom.rc",
        ])
        .assert()
        .success();
    assert!(fixture.path("startup/custom.rc").exists());
    assert!(!fixture.path(".bashrc").exists());
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(fixture.read(".profile"), "");
    assert_eq!(fixture.read("startup/custom.rc"), "");
}

#[test]
fn test_enable_powershell_requires_explicit_host_profile_and_quotes_paths() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "powershell"])
        .assert()
        .failure()
        .stderr(contains("current $PROFILE"));
    fixture
        .command()
        .args([
            "env",
            "--enable",
            "--shell",
            "powershell",
            "--rc-file",
            "host-profile.ps1",
            "--shim-dir",
            "shims' folder",
        ])
        .assert()
        .success();
    let content = fixture.read("host-profile.ps1");
    assert!(content.contains("shims'' folder"));
    assert!(content.contains("Test-Path -LiteralPath"));
    assert!(content.contains("| Out-String | Invoke-Expression"));
}

#[test]
fn test_enable_unknown_shell_requires_explicit_choice() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env("SHELL", "/bin/unknown-shell")
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(contains("Cannot detect shell"));
    fixture
        .command()
        .env_remove("SHELL")
        .args(["env", "--enable", "--shell", "zsh"])
        .assert()
        .success();
}

#[test]
fn test_enable_cli_rejects_conflicting_operations_and_rc_without_startup_operation() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable", "--disable"])
        .assert()
        .failure();
    fixture
        .command()
        .args(["env", "--rc-file", "custom.rc"])
        .assert()
        .failure();
    assert!(!fixture.path("custom.rc").exists());
}

#[test]
fn test_enable_preserves_symlinks_permissions_and_unrelated_edits_on_uninstall() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    fixture.write(
        "dotfiles/zshrc",
        "# Existing configuration without final newline",
    );
    std::fs::set_permissions(
        fixture.path("dotfiles/zshrc"),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    symlink(fixture.path("dotfiles/zshrc"), fixture.path(".zshrc")).unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    assert!(std::fs::symlink_metadata(fixture.path(".zshrc"))
        .unwrap()
        .file_type()
        .is_symlink());
    let content = fixture.read(".zshrc");
    fixture.write(
        "dotfiles/zshrc",
        &format!("{content}\n# Added by the user\n"),
    );
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(
        fixture.read(".zshrc"),
        "# Existing configuration without final newline\n# Added by the user\n"
    );
    assert_eq!(
        std::fs::metadata(fixture.path("dotfiles/zshrc"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(!fixture.path(".apexe/bin/.apexe-shell-init.json").exists());
}

#[test]
fn test_enable_disable_restores_original_missing_final_newline() {
    let fixture = Fixture::new();
    fixture.write(".zshrc", "export PRESERVE=1");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(fixture.read(".zshrc"), "export PRESERVE=1");
}

#[test]
fn test_enable_refuses_modified_and_duplicate_managed_blocks() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    let original = fixture.read(".zshrc");
    let modified = original.replace("--shell sh", "--shell fish");
    fixture.write(".zshrc", &modified);
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(contains("was modified"));
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .failure();
    assert_eq!(fixture.read(".zshrc"), modified);
    fixture.write(".zshrc", &format!("{original}{original}"));
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure();
}

#[test]
fn test_enable_refuses_unowned_managed_block_without_changing_startup() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    let original = fixture.read(".zshrc");
    std::fs::remove_file(fixture.path(".apexe/bin/.apexe-shell-init.json")).unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(contains("unowned"));
    assert_eq!(fixture.read(".zshrc"), original);
}

#[test]
fn test_enable_replace_unowned_recovers_after_lost_installation_record() {
    // The incident: the installation record was gone and a hand-written script
    // occupied the entry point. Every repair path is keyed on that record, so
    // `--enable` could only ever say "unowned" — the environment was wedged with
    // no way out but deleting files by hand.
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    std::fs::remove_file(fixture.path(".apexe/bin/.apexe-shims.json")).unwrap();
    std::fs::remove_file(fixture.path(".apexe/bin/apexe-sh")).unwrap();
    fixture.write(".apexe/bin/apexe-sh", "#!/bin/sh\nexec /bin/zsh \"$@\"\n");

    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(contains("--replace-unowned"));

    fixture
        .command()
        .args(["env", "--enable", "--replace-unowned"])
        .assert()
        .success();

    let entry = fixture.path(".apexe/bin/apexe-sh");
    assert!(
        std::fs::symlink_metadata(&entry)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the recovered entry point must be the launcher link again"
    );
    assert!(fixture.path(".apexe/bin/.apexe-shims.json").exists());
    assert!(
        std::fs::symlink_metadata(fixture.path(".apexe/bin/cat"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "installed command shims are recovered too, not left unowned by the missing record"
    );
    // The record is functional again: `--reload` is keyed on it and cannot run
    // until the installation is whole.
    fixture
        .command()
        .args(["env", "--reload"])
        .assert()
        .success();
}

#[test]
fn test_enable_replace_unowned_never_replaces_command_shims() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    std::fs::remove_file(fixture.path(".apexe/bin/.apexe-shims.json")).unwrap();
    let shim = fixture.path(".apexe/bin/cat");
    assert!(shim.exists(), "the fixture installs a `cat` shim");
    std::fs::remove_file(&shim).unwrap();
    std::fs::write(&shim, "#!/bin/sh\nexec /bin/cat \"$@\"\n").unwrap();

    // The override is scoped to apexe's own reserved names. A file sitting on a
    // command name is the operator's, whatever else is missing.
    fixture
        .command()
        .args(["env", "--enable", "--replace-unowned"])
        .assert()
        .failure()
        .stderr(contains("unowned shim path"));
}

#[test]
fn test_enable_validates_all_startup_files_before_writing_any() {
    let fixture = Fixture::new();
    fixture.write(".bashrc", "# Keep me\n");
    std::fs::create_dir(fixture.path(".bash_profile")).unwrap();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .failure();
    assert_eq!(fixture.read(".bashrc"), "# Keep me\n");
    assert!(!fixture.path(".apexe/bin/.apexe-shell-init.json").exists());
}

#[test]
fn test_enable_rejects_non_utf8_files_and_unsafe_path_segments() {
    let fixture = Fixture::new();
    std::fs::write(fixture.path(".zshrc"), [0xff, 0xfe]).unwrap();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure();
    assert_eq!(std::fs::read(fixture.path(".zshrc")).unwrap(), [0xff, 0xfe]);
    fixture
        .command()
        .args(["env", "--enable", "--rc-file", "bad\nname"])
        .assert()
        .failure()
        .stderr(contains("line breaks"));
    fixture
        .command()
        .args([
            "env",
            "--enable",
            "--rc-file",
            "good.rc",
            "--shim-dir",
            "bad:path",
        ])
        .assert()
        .failure();
    assert!(!fixture.path("good.rc").exists());
}

#[test]
fn test_enable_disable_tolerates_manually_removed_block_or_file() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    fixture.write(".bashrc", "# User removed the managed block\n");
    std::fs::remove_file(fixture.path(".bash_profile")).unwrap();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    assert_eq!(
        fixture.read(".bashrc"),
        "# User removed the managed block\n"
    );
    assert!(!fixture.path(".bash_profile").exists());
}

#[test]
fn test_enable_custom_path_is_shell_quoted_and_never_executed() {
    let fixture = Fixture::new();
    let directory = fixture.path("shims' $(touch injected)`touch injected` folder");
    fixture
        .command()
        .args(["env", "--enable", "--shim-dir"])
        .arg(&directory)
        .assert()
        .success();
    Command::new("/bin/sh")
        .current_dir(fixture.home.path())
        .arg("-c")
        .arg(". \"$1\"")
        .arg("sh")
        .arg(fixture.path(".zshrc"))
        .assert()
        .success()
        .stdout("")
        .stderr("");
    assert!(!fixture.path("injected").exists());
    fixture
        .command()
        .args(["env", "--disable", "--shim-dir"])
        .arg(&directory)
        .assert()
        .success();
}

#[test]
fn test_enable_rejects_relative_shell_configuration_directory() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env("ZDOTDIR", Path::new("relative-zsh"))
        .args(["env", "--enable"])
        .assert()
        .failure()
        .stderr(contains("absolute directory"));
}

#[test]
fn test_enable_refuses_startup_files_inside_managed_directory() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args([
            "env",
            "--enable",
            "--rc-file",
            ".apexe/bin/.apexe-shims.json",
        ])
        .assert()
        .failure()
        .stderr(contains("outside the shim directory"));
    assert!(!fixture.path(".apexe/bin/.apexe-shims.json").exists());
}

#[test]
fn test_enable_reinstall_restores_user_removed_block_and_rejects_corrupt_state() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    fixture.write(".zshrc", "# User configuration\n");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    assert!(fixture.read(".zshrc").starts_with("# User configuration\n"));
    let original = fixture.read(".zshrc");
    fixture.write(".apexe/bin/.apexe-shell-init.json", "{not json}");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .failure();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .failure();
    assert_eq!(fixture.read(".zshrc"), original);
}

#[test]
fn test_enable_disable_refuses_startup_target_replaced_with_symlink() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    let content = fixture.read(".zshrc");
    fixture.write("replacement", &content);
    std::fs::remove_file(fixture.path(".zshrc")).unwrap();
    symlink(fixture.path("replacement"), fixture.path(".zshrc")).unwrap();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .failure()
        .stderr(contains("changed symlink"));
    assert_eq!(fixture.read("replacement"), content);
}

#[test]
fn test_enable_cli_rejects_conflicting_operations() {
    let fixture = Fixture::new();
    for arguments in [
        ["env", "--enable", "--disable"],
        ["env", "--enable", "--reload"],
        ["env", "--disable", "--enable"],
        ["env", "--disable", "--reload"],
        ["env", "--reload", "--enable"],
        ["env", "--reload", "--disable"],
    ] {
        fixture.command().args(arguments).assert().failure();
    }
    assert!(!fixture.path(".zshrc").exists());
}

#[test]
fn test_enable_without_eligible_modules_does_not_configure_shell() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(fixture.path(".apexe/bindings")).unwrap();
    for flag in ["--enable", "--reload"] {
        fixture
            .command()
            .args(["env", flag])
            .assert()
            .failure()
            .stderr(contains("No shim-eligible modules"));
    }
    assert!(!fixture.path(".zshrc").exists());
    assert!(!fixture.path(".apexe/bin/.apexe-shell-init.json").exists());
}

#[test]
fn test_disable_is_idempotent_and_removes_legacy_initialization() {
    let fixture = Fixture::new();
    fixture
        .command()
        .env_remove("SHELL")
        .args(["env", "--disable"])
        .assert()
        .success()
        .stdout("");
    fixture
        .command()
        .args(["env", "--enable"])
        .assert()
        .success();
    // Simulate historical startup-only state without accepting the removed CLI option.
    std::fs::remove_file(fixture.path(".apexe/bin/cat")).unwrap();
    std::fs::remove_file(fixture.path(".apexe/bin/.apexe-shims.json")).unwrap();
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
    assert_eq!(fixture.read(".zshrc"), "");
}

#[test]
fn test_env_help_prefers_enable_and_disable() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["env", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--enable"));
    assert!(help.contains("--disable"));
    assert!(help.contains("--reload"));
    assert!(help.contains("--rc-file"));
    for legacy in ["--load", "--unload", "--install", "--init", "--uninstall"] {
        assert!(!help.contains(legacy));
    }
}
