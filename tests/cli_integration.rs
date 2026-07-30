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

// --------------------------------------------------------------------------
// `serve --show-config` (issue #37.3)
// --------------------------------------------------------------------------

/// Run `apexe serve --show-config …` and parse stdout as JSON.
fn show_config(extra: &[&str]) -> serde_json::Value {
    let output = Command::cargo_bin("apexe")
        .unwrap()
        .args(["serve", "--show-config"])
        .args(extra)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout).to_string();
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"))
}

fn args_of(config: &serde_json::Value, name: &str) -> Vec<String> {
    config["mcpServers"][name]["args"]
        .as_array()
        .unwrap_or_else(|| panic!("no args in {config}"))
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Regression for issue #37.3: the stdio snippet emitted a fixed
/// `["serve","--transport","stdio"]` no matter what else was on the command
/// line, so a user who scanned into a non-default directory got a config that
/// launched a server with no tools at all.
#[test]
fn test_show_config_stdio_carries_every_surface_flag() {
    let config = show_config(&[
        "claude-desktop",
        "--modules-dir",
        "/srv/apexe/modules",
        "--prefix",
        "cli.git",
        "--tags",
        "readonly",
        "--acl",
        "/etc/apexe/acl.yaml",
        "--enable-approval",
    ]);
    let args = args_of(&config, "apexe");

    assert_eq!(config["mcpServers"]["apexe"]["command"], "apexe");
    for pair in [
        ["--modules-dir", "/srv/apexe/modules"],
        ["--prefix", "cli.git"],
        ["--tags", "readonly"],
        ["--acl", "/etc/apexe/acl.yaml"],
    ] {
        assert!(
            args.windows(2).any(|w| w == pair),
            "{pair:?} missing from {args:?}"
        );
    }
    assert!(args.iter().any(|a| a == "--enable-approval"));
}

/// `("cursor", _)` ignored `--transport` entirely, so `--transport http`
/// still emitted a stdio command block that launched a second server.
#[test]
fn test_show_config_cursor_honours_http_transport() {
    let config = show_config(&["cursor", "--transport", "http", "--port", "9111"]);
    assert_eq!(
        config["mcpServers"]["apexe"]["url"],
        "http://127.0.0.1:9111/mcp"
    );
    assert!(config["mcpServers"]["apexe"]["command"].is_null());
}

/// An unknown target used to print `Unknown config format: vscode` to stdout
/// and exit 0, so `--show-config vscode > mcp.json` wrote that sentence as the
/// config file's body.
#[test]
fn test_show_config_unknown_format_fails_without_writing_stdout() {
    let assert = Command::cargo_bin("apexe")
        .unwrap()
        .args(["serve", "--show-config", "vscode"])
        .assert()
        .failure();
    let output = assert.get_output();
    assert!(
        output.stdout.is_empty(),
        "nothing may reach stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("vscode"), "stderr: {stderr}");
    assert!(stderr.contains("claude-desktop"), "stderr: {stderr}");
}

/// A snippet is pasted into a config file that is shared and often committed,
/// so no credential may appear in one.
#[test]
fn test_show_config_never_echoes_credentials() {
    for extra in [
        vec![
            "claude-desktop",
            "--transport",
            "http",
            "--auth",
            "token",
            "--auth-token",
            "s3cret-bearer-value",
        ],
        vec![
            "cursor",
            "--auth-token",
            "s3cret-bearer-value",
            "--jwt-secret",
            "s3cret-signing-key",
        ],
    ] {
        let rendered = show_config(&extra).to_string();
        for forbidden in ["s3cret-bearer-value", "s3cret-signing-key", "--auth-token"] {
            assert!(
                !rendered.contains(forbidden),
                "{extra:?} leaked {forbidden}: {rendered}"
            );
        }
    }
}

/// `--skip-validation` claimed to skip schema validation and skipped nothing.
/// It is gone; passing it must fail loudly rather than be accepted as a no-op.
#[test]
fn test_serve_rejects_removed_skip_validation_flag() {
    Command::cargo_bin("apexe")
        .unwrap()
        .args(["serve", "--skip-validation"])
        .assert()
        .failure()
        .code(2);
}

// --------------------------------------------------------------------------
// The ACL example in docs/user-manual.md §9.1 (issue #37.1)
// --------------------------------------------------------------------------

/// The manual's §9.1 deny rule used to carry `conditions: {require_approval:
/// true}` — not a registered apcore condition key, so the rule matched nothing
/// and the destructive module ran. The corrected example is unconditional;
/// this pins that it denies **on its own merits** by loading it under
/// `default_effect: allow`, where a non-matching rule would let the call
/// through.
#[test]
fn test_manual_acl_example_denies_the_destructive_module() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("acl.yaml");
    // Copied from docs/user-manual.md §9.1, with default_effect flipped to
    // `allow` so only a genuinely matching rule can produce a denial.
    std::fs::write(
        &path,
        "default_effect: allow\n\
         rules:\n\
         \x20 - callers: [\"*\"]\n\
         \x20   targets: [\"cli.git.status\", \"cli.git.log\", \"cli.git.diff\"]\n\
         \x20   effect: allow\n\
         \x20   description: \"Auto-allow readonly git commands\"\n\
         \x20 - callers: [\"*\"]\n\
         \x20   targets: [\"cli.git.push\"]\n\
         \x20   effect: deny\n\
         \x20   description: \"Block destructive git commands\"\n",
    )
    .unwrap();

    let acl = apexe::governance::AclManager::from_config(&path)
        .expect("the manual's example must be a loadable ACL")
        .into_inner();

    assert!(
        !acl.check(None, "cli.git.push", None),
        "the manual's deny rule must deny without relying on default_effect"
    );
    assert!(
        acl.check(None, "cli.git.status", None),
        "the manual's allow rule must still allow readonly commands"
    );
}

/// The condition key that made the old example inert must not come back.
#[test]
fn test_manual_acl_example_carries_no_unregistered_condition() {
    let manual = include_str!("../docs/user-manual.md");
    for line in manual.lines() {
        assert!(
            !line.trim_start().starts_with("require_approval:"),
            "docs/user-manual.md still shows `require_approval` as an ACL \
             condition; apcore registers only identity_types, roles, \
             max_call_depth, $or and $not, so such a rule can never match: {line}"
        );
    }
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
