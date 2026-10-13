use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new(settings: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".apexe")).unwrap();
        std::fs::write(home.path().join(".apexe/config.yaml"), settings).unwrap();
        Self { home }
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("HOME", self.home.path())
            .env("APEXE_OFFLINE", "true")
            .env_remove("RUST_LOG")
            .env_remove("APEXE_LOG_LEVEL")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .current_dir(self.home.path());
        command
    }

    fn log(&self) -> String {
        std::fs::read_dir(self.home.path().join(".apexe/log"))
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect()
    }
}

#[test]
fn test_commands_default_to_warn_file_logs_with_quiet_terminal() {
    let fixture = Fixture::new("unknown_logging_setting: true\n");
    for arguments in [
        vec!["config", "--show"],
        vec!["list", "--format", "json"],
        vec!["policy", "--format", "json"],
    ] {
        fixture
            .command()
            .args(arguments)
            .assert()
            .success()
            .stderr("");
    }
    let log = fixture.log();
    assert!(log.contains("WARN"));
    assert!(log.contains("unknown_logging_setting"));
    assert!(!log.contains("DEBUG"));
    assert!(!log.contains("INFO"));
}

#[test]
fn test_config_set_persists_level_and_keeps_other_settings() {
    let fixture = Fixture::new("default_timeout: 99\n");
    fixture
        .command()
        .args(["config", "set", "log.level", "debug"])
        .assert()
        .success()
        .stdout("")
        .stderr("");
    let output = fixture
        .command()
        .args(["config", "--show"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .clone();
    let settings: serde_yaml::Value = serde_yaml::from_slice(&output.stdout).unwrap();
    assert_eq!(settings["log_level"], "debug");
    assert_eq!(settings["default_timeout"], 99);
    assert!(fixture.log().contains("Installing path guard"));
}

#[test]
fn test_global_log_level_overrides_environment_before_or_after_command() {
    for arguments in [
        vec!["--log-level", "debug", "config", "--show"],
        vec!["config", "--show", "--log-level", "debug"],
    ] {
        let fixture = Fixture::new("log_level: warn\n");
        fixture
            .command()
            .env("RUST_LOG", "error")
            .env("APEXE_LOG_LEVEL", "error")
            .args(arguments)
            .assert()
            .success()
            .stderr("");
        assert!(fixture.log().contains("DEBUG"));
    }
    let fixture = Fixture::new("log_level: debug\nunknown_logging_setting: true\n");
    fixture
        .command()
        .env("RUST_LOG", "trace")
        .args(["config", "--show", "--log-level", "error"])
        .assert()
        .success()
        .stderr("");
    assert!(fixture.log().is_empty());
}

#[test]
fn test_logging_environment_level_overrides_config() {
    let fixture = Fixture::new("log_level: error\n");
    fixture
        .command()
        .env("APEXE_LOG_LEVEL", "debug")
        .args(["config", "--show"])
        .assert()
        .success()
        .stderr("");
    assert!(fixture.log().contains("Installing path guard"));
}

#[test]
fn test_log_stderr_mirrors_files_without_polluting_stdout() {
    let fixture = Fixture::new("unknown_logging_setting: true\n");
    let output = fixture
        .command()
        .args(["config", "--show", "--log-stderr"])
        .assert()
        .success()
        .stderr(contains("WARN"))
        .get_output()
        .clone();
    let settings: serde_yaml::Value = serde_yaml::from_slice(&output.stdout).unwrap();
    assert_eq!(settings["log_level"], "warn");
    assert!(fixture.log().contains("unknown_logging_setting"));
}

#[test]
fn test_invalid_log_levels_are_rejected_without_changing_config() {
    let fixture = Fixture::new("log_level: warn\n");
    for arguments in [
        vec!["--log-level", "verbose", "config", "--show"],
        vec!["config", "set", "log.level", "verbose"],
        vec!["config", "set", "unsupported", "debug"],
    ] {
        fixture.command().args(arguments).assert().code(2);
    }
    assert_eq!(
        std::fs::read_to_string(fixture.home.path().join(".apexe/config.yaml")).unwrap(),
        "log_level: warn\n"
    );
}

#[test]
fn test_command_errors_remain_visible_and_are_flushed_to_file() {
    let fixture = Fixture::new("");
    fixture
        .command()
        .args(["exec", "cli.missing"])
        .assert()
        .failure()
        .stderr(contains("Error:"));
    assert!(fixture.log().contains("Command failed"));
}

#[test]
fn test_log_level_off_disables_diagnostics() {
    let fixture = Fixture::new("unknown_logging_setting: true\nlog_level: off\n");
    fixture
        .command()
        .args(["config", "--show", "--log-stderr"])
        .assert()
        .success()
        .stderr("");
    assert!(fixture.log().is_empty());
}
