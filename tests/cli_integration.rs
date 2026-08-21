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

// --------------------------------------------------------------------------
// A non-zero exit is not an MCP error (issue #39.2)
// --------------------------------------------------------------------------

/// The manual's `isError` claim must survive an apcore-mcp bump.
///
/// §13 tells a reader that a wrapped command exiting non-zero comes back as an
/// ordinary successful tool result — `isError: false`, with the real outcome in
/// `exit_code` inside `content[0].text` — and that keying on `isError` instead
/// lands them in exactly the trap the section warns about. `grep` exits 1 when
/// it finds nothing and `diff` exits 1 when files differ; reporting those as
/// failed tool calls would be wrong.
///
/// That is a claim about **apcore-mcp's** envelope, not about apexe, and it had
/// no guard: a change to how upstream maps a module result would leave the
/// manual quietly wrong. It also depends on apexe's own half — `CliModule`
/// returning `Ok` for a non-zero exit rather than an error — so either side can
/// break it.
///
/// Driven through the real binary over stdio, because the envelope only exists
/// at the transport: `Executor::call` returns the payload without it.
#[test]
fn test_a_non_zero_exit_is_not_reported_as_an_mcp_error() {
    let tmp = tempfile::tempdir().unwrap();
    let modules = tmp.path().join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    // `/usr/bin/false` exits 1 and prints nothing, which is the whole case:
    // a command that ran correctly and reported a non-zero result.
    std::fs::write(
        modules.join("cli.false.binding.yaml"),
        "spec_version: '1.0'\n\
         bindings:\n\
         - module_id: cli.false\n\
         \x20 target: exec:///usr/bin/false\n\
         \x20 description: Always exit non-zero\n\
         \x20 version: '1.0.0'\n\
         \x20 tags: [cli]\n\
         \x20 input_schema:\n\
         \x20   type: object\n\
         \x20   properties: {}\n\
         \x20   additionalProperties: false\n\
         \x20 output_schema:\n\
         \x20   type: object\n",
    )
    .unwrap();

    let session = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cli.false","arguments":{}}}"#,
        "\n",
    );

    let output = Command::cargo_bin("apexe")
        .unwrap()
        .args([
            "serve",
            "--transport",
            "stdio",
            "--modules-dir",
            modules.to_str().unwrap(),
        ])
        .write_stdin(session)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("MCP responses are UTF-8");
    let call: serde_json::Value = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|message| message["id"] == 5)
        .expect("the tools/call response must arrive");

    let result = &call["result"];
    assert_eq!(
        result["isError"], false,
        "a command that ran and exited non-zero is a successful call: {result}"
    );

    // The real outcome is inside the content, as a JSON string — the manual
    // shows this shape because `exit_code` is not a sibling of `isError`.
    let text = result["content"][0]["text"]
        .as_str()
        .expect("the payload rides in content[0].text");
    let payload: serde_json::Value =
        serde_json::from_str(text).expect("content[0].text is a JSON document");
    assert_eq!(
        payload["exit_code"], 1,
        "the non-zero exit must be readable where the manual says it is: {payload}"
    );
    assert!(
        payload.get("ai_guidance").is_some(),
        "a non-zero exit must carry guidance a caller can self-correct from: {payload}"
    );
}

/// `apexe --man` is the one thing `apcore-cli` is a dependency for.
///
/// README.md and docs/FEATURE_MANIFEST.md both name it as the reason the crate
/// is in the manifest, and this branch bumped that dependency and added a guard
/// pinning its version — but nothing exercised the feature, so an upstream
/// change to `has_man_flag` or `build_program_man_page` would have shipped
/// silently.
#[test]
fn test_man_flag_emits_a_man_page() {
    let output = Command::cargo_bin("apexe")
        .unwrap()
        .arg("--man")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let man = String::from_utf8(output).expect("a man page is UTF-8");
    assert!(
        man.contains(".TH"),
        "roff output must carry a title header: {}",
        &man[..man.len().min(200)]
    );
    assert!(man.contains("apexe"), "the page must name the program");
}

// --------------------------------------------------------------------------
// Explorer Try-It prefill (issue #38)
// --------------------------------------------------------------------------

/// The manual's account of the Explorer prefill must match what is served.
///
/// §10 tells a reader the Try-It editor emits only the `required` keys, with a
/// `null` placeholder that `Validate` deliberately refuses. That is
/// `mcp-embedded-ui`'s behaviour, reached through apcore-mcp — apexe only
/// serves the page — so it can go stale on a dependency bump with nothing in
/// this repo noticing. `mcp-embedded-ui` 0.4 filled every property with `""` /
/// `0` / `false`, which satisfied both `required` and the declared type, so
/// `Validate` certified an empty call and `Execute` sent it.
///
/// Asserted against the page the running server actually returns, not against
/// a vendored copy: what a reader sees in their browser is what has to match.
#[test]
fn test_the_explorer_prefills_only_required_keys() {
    let port = 8_931;
    let mut server = std::process::Command::new(env!("CARGO_BIN_EXE_apexe"))
        .args([
            "serve",
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--explorer",
            "--auth",
            "none",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the server binary starts");

    let url = format!("http://127.0.0.1:{port}/explorer");
    let html = (0..80)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::process::Command::new("curl")
                .args(["-sf", &url])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .unwrap_or_default();
    let _ = server.kill();
    let _ = server.wait();

    assert!(
        html.contains("function defaultFromSchema"),
        "the served page must carry the Try-It prefill"
    );
    assert!(
        html.contains("var required = schema.required;"),
        "the prefill must read `required` rather than every property"
    );
    // Each of these fabricates a value that satisfies the declared type, which
    // is what let `Validate` greenlight an untouched prefill.
    for fabricated in [
        "result[key] = '';",
        "result[key] = 0;",
        "result[key] = false;",
        "result[key] = [];",
        "result[key] = {};",
    ] {
        assert!(
            !html.contains(fabricated),
            "the served prefill fabricates a type-based value again: {fabricated}"
        );
    }
}

// --------------------------------------------------------------------------
// scan --dry-run / --verify, and the --explorer transport gate
// --------------------------------------------------------------------------

/// `--dry-run` must not touch the filesystem, for any of the three
/// deliverables.
///
/// Reporting what *would* be written is only useful if nothing is; a preview
/// that half-writes is worse than no preview, because the operator now has to
/// work out which half.
#[test]
fn test_scan_dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let out = tmp.path().join("modules");
    let skills = tmp.path().join("skills");

    // Progress messages go through `tracing` (stderr), the same channel the
    // real (non-dry-run) write path already uses -- see
    // test_dry_run_progress_messages_do_not_pollute_json_stdout below for why
    // that matters.
    let output = Command::cargo_bin("apexe")
        .unwrap()
        .env("HOME", &home)
        .args([
            "scan",
            "ls",
            "--output-dir",
            out.to_str().unwrap(),
            "--skills-dir",
            skills.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("Would write binding"),
        "a dry run must still name the bindings it skipped: {stderr}"
    );
    assert!(
        stderr.contains("Would write ACL policy"),
        "the ACL is a deliverable too: {stderr}"
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        !stdout.contains("Would write"),
        "dry-run progress messages must not land on stdout, apexe's \
         machine-readable channel: {stdout}"
    );

    let created = |dir: &std::path::Path| -> usize { walk_files(dir).len() };
    assert_eq!(created(&out), 0, "no binding may be written");
    assert_eq!(created(&skills), 0, "no skill may be written");
}

#[test]
fn test_dry_run_progress_messages_do_not_pollute_json_stdout() {
    // Regression: the three dry-run branches used `println!`, sharing stdout
    // with print_results' `--format json` document. `apexe scan --dry-run
    // --format json` is exactly the pipeline/CI combination the flag exists
    // for, and it produced a stream that was not valid JSON.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let out = tmp.path().join("modules");

    let stdout = Command::cargo_bin("apexe")
        .unwrap()
        .env("HOME", &home)
        .args([
            "scan",
            "ls",
            "--output-dir",
            out.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report = String::from_utf8(stdout).expect("stdout is UTF-8");
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&report);
    assert!(
        parsed.is_ok(),
        "stdout must be valid JSON under --dry-run --format json, got: {report}\nerror: {:?}",
        parsed.err()
    );
}

/// Count files under `dir`, treating a missing directory as empty.
fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A real scan is the control: the dry run's zero files must mean something.
#[test]
fn test_scan_without_dry_run_writes_the_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("modules");

    Command::cargo_bin("apexe")
        .unwrap()
        .args(["scan", "ls", "--output-dir", out.to_str().unwrap()])
        .assert()
        .success();

    assert!(
        !walk_files(&out).is_empty(),
        "the same scan without --dry-run must produce a binding"
    );
}

/// `--explorer` on stdio warns instead of silently doing nothing.
///
/// The Explorer is served over HTTP. Asking for it on stdio used to wire it
/// anyway, where nothing could ever reach it, so the flag looked accepted and
/// produced no UI and no diagnostic. Mirrors how `--metrics` already behaves.
#[test]
fn test_explorer_on_stdio_warns_that_it_does_nothing() {
    let session = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        "\n",
    );
    let stderr = Command::cargo_bin("apexe")
        .unwrap()
        .args(["serve", "--transport", "stdio", "--explorer"])
        .write_stdin(session)
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let log = String::from_utf8_lossy(&stderr).into_owned();
    assert!(
        log.contains("--explorer has no effect on stdio"),
        "the flag must say it does nothing here: {log}"
    );
}
