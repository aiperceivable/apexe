use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_help_shows_subcommands() {
    Command::cargo_bin("apexe")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("config"));
}

#[test]
fn test_version_flag() {
    Command::cargo_bin("apexe")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("apexe"));
}

#[test]
fn test_scan_help_shows_expected_flags() {
    Command::cargo_bin("apexe")
        .unwrap()
        .args(["scan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("TOOLS"))
        .stdout(predicate::str::contains("--output-dir"))
        .stdout(predicate::str::contains("--depth"))
        .stdout(predicate::str::contains("--no-cache"))
        .stdout(predicate::str::contains("--format"));
}

#[test]
fn test_serve_help_shows_expected_flags() {
    Command::cargo_bin("apexe")
        .unwrap()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--transport"))
        .stdout(predicate::str::contains("--host"))
        .stdout(predicate::str::contains("--port"))
        .stdout(predicate::str::contains("--a2a"))
        .stdout(predicate::str::contains("--explorer"));
}

#[test]
fn test_scan_no_args_fails() {
    Command::cargo_bin("apexe")
        .unwrap()
        .arg("scan")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_config_show_succeeds() {
    Command::cargo_bin("apexe")
        .unwrap()
        .args(["config", "--show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("modules_dir"))
        .stdout(predicate::str::contains("log_level"));
}

#[test]
fn test_config_no_flags_succeeds() {
    Command::cargo_bin("apexe")
        .unwrap()
        .arg("config")
        .assert()
        .success();
}
