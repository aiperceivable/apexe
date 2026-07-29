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
        .stdout(predicate::str::contains("a2a"))
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
        .stdout(predicate::str::contains("--explorer"));
}

#[test]
fn test_a2a_help_shows_expected_flags() {
    // Regression for the WARNING finding: `apexe a2a` had no CLI integration
    // test at all, unlike `apexe serve`. Mirrors
    // test_serve_help_shows_expected_flags for the a2a subcommand's flags.
    Command::cargo_bin("apexe")
        .unwrap()
        .args(["a2a", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--url"))
        .stdout(predicate::str::contains("--modules-dir"))
        .stdout(predicate::str::contains("--acl"))
        .stdout(predicate::str::contains("--explorer"))
        // A2A has no interactive elicitation, so --enable-approval is not offered
        // (it is a library-only feature for A2A).
        .stdout(predicate::str::contains("--enable-approval").not());
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

/// Regression: one unscannable name used to abort the whole batch.
///
/// `apexe scan` propagated the first error, so a single bad name discarded
/// every tool already scanned *and* every tool not yet reached — the command
/// wrote no bindings at all and its message did not say which name failed.
#[test]
fn test_scan_writes_bindings_for_the_tools_that_succeeded() {
    let out = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("apexe")
        .unwrap()
        .args(["scan", "echo", "zzz_no_such_tool_xyz", "ls"])
        .args(["--no-cache", "--output-dir"])
        .arg(out.path())
        .assert()
        // Partial success still exits non-zero: a pipeline must not read a
        // short surface as the whole surface.
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("zzz_no_such_tool_xyz"),
        "the failing tool must be named: {stderr}"
    );
    assert!(
        stderr.contains("Scanned 2 of 3 tools"),
        "the message must state both halves: {stderr}"
    );

    // The successes are on disk — including `ls`, which comes *after* the
    // failure and so proves the batch continued.
    let written: Vec<String> = std::fs::read_dir(out.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert!(
        written.iter().any(|n| n.contains("echo")),
        "bindings written: {written:?}"
    );
    assert!(
        written.iter().any(|n| n.contains("ls")),
        "the tool after the failure must still be scanned: {written:?}"
    );
}

/// When nothing scans there is no deliverable, so it is a plain failure.
#[test]
fn test_scan_fails_outright_when_no_tool_can_be_scanned() {
    let out = tempfile::tempdir().unwrap();

    Command::cargo_bin("apexe")
        .unwrap()
        .args(["scan", "zzz_no_such_tool_xyz", "zzz_also_missing_xyz"])
        .args(["--no-cache", "--output-dir"])
        .arg(out.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No tool could be scanned"));
}
