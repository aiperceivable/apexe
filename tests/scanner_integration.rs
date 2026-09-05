//! Integration tests for the CLI Scanner Engine (F2).
//!
//! Tests marked #[ignore] require specific CLI tools installed.

use apexe::config::ApexeConfig;
use apexe::scanner::ScanOrchestrator;
use tempfile::TempDir;

fn test_config() -> (TempDir, ApexeConfig) {
    let tmp = TempDir::new().unwrap();
    let config = ApexeConfig {
        modules_dir: tmp.path().join("modules"),
        cache_dir: tmp.path().join("cache"),
        config_dir: tmp.path().to_path_buf(),
        audit_log: tmp.path().join("audit.jsonl"),
        log_level: "warn".into(),
        default_timeout: 30,
        scan_depth: 2,
        json_output_preference: true,
        ..ApexeConfig::default()
    };
    (tmp, config)
}

// T44: Scan git integration test
/// The corpus these tests read, or `None` when this checkout has none.
///
/// apexe ships no overlays — the corpus lives in the `cli-permissions`
/// repository — so a test asserting something about a real entry needs it on
/// disk. `APEXE_TEST_CORPUS` names it (CI sets that); otherwise a sibling
/// checkout is used, which is the ordinary local layout.
///
/// **Set but missing panics**, because a test that silently skips forever
/// reports green while covering nothing.
fn corpus_dir() -> Option<std::path::PathBuf> {
    if let Some(configured) = std::env::var_os("APEXE_TEST_CORPUS") {
        let path = std::path::PathBuf::from(configured);
        assert!(
            path.is_dir(),
            "APEXE_TEST_CORPUS points at {}, which is not a directory",
            path.display()
        );
        return Some(path);
    }
    let sibling = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("cli-permissions/overlays");
    sibling.is_dir().then_some(sibling)
}

#[test]
#[ignore]
fn test_scan_git() {
    let (_tmp, config) = test_config();
    let orchestrator = ScanOrchestrator::new(config);

    let outcome = orchestrator.scan(&["git".into()], true, 2);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let results = outcome.tools;

    assert_eq!(results.len(), 1);
    let git = &results[0];

    assert_eq!(git.name, "git");
    assert!(git.version.is_some(), "git should report a version");
    assert!(!git.binary_path.is_empty());

    // Check that key subcommands are discovered
    let subcmd_names: Vec<&str> = git.subcommands.iter().map(|s| s.name.as_str()).collect();
    assert!(
        subcmd_names.contains(&"commit"),
        "Expected 'commit' in subcommands, found: {subcmd_names:?}"
    );
    assert!(
        subcmd_names.contains(&"push"),
        "Expected 'push' in subcommands"
    );
    assert!(
        subcmd_names.contains(&"pull"),
        "Expected 'pull' in subcommands"
    );
    assert!(
        subcmd_names.contains(&"clone"),
        "Expected 'clone' in subcommands"
    );

    // Should have a significant number of subcommands
    assert!(
        git.subcommands.len() > 20,
        "Expected >20 subcommands, got {}",
        git.subcommands.len()
    );

    // Check that 'git commit' has --message flag
    if let Some(commit) = git.subcommands.iter().find(|s| s.name == "commit") {
        let has_message = commit
            .flags
            .iter()
            .any(|f| f.long_name.as_deref() == Some("--message"));
        assert!(
            has_message,
            "Expected --message flag on git commit, flags: {:?}",
            commit
                .flags
                .iter()
                .map(|f| f.long_name.as_deref().unwrap_or("?"))
                .collect::<Vec<_>>()
        );
    }

    assert!(git.scan_tier >= 1);
}

// T45: Scan docker integration test (nested subcommands)
#[test]
#[ignore]
fn test_scan_docker() {
    let (_tmp, config) = test_config();
    let orchestrator = ScanOrchestrator::new(config);

    let outcome = orchestrator.scan(&["docker".into()], true, 2);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let results = outcome.tools;

    assert_eq!(results.len(), 1);
    let docker = &results[0];

    assert_eq!(docker.name, "docker");
    assert!(docker.version.is_some());

    // Check that 'container' is a subcommand
    let container = docker.subcommands.iter().find(|s| s.name == "container");
    assert!(
        container.is_some(),
        "Expected 'container' subcommand in docker"
    );

    let container = container.unwrap();

    // With depth=2, 'container' should have nested subcommands
    let nested_names: Vec<&str> = container
        .subcommands
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        nested_names.contains(&"ls") || nested_names.contains(&"list"),
        "Expected 'ls' or 'list' in docker container subcommands, found: {nested_names:?}"
    );
}

/// Regression (#20): scan the real `find` on this host, build its module the
/// way `apexe scan` does, and run it — for BOTH classes of dash-token.
///
/// This is deliberately end to end. `find`'s options and predicates render
/// correctly as strings under any ordering, so a string assertion on the argv
/// proves nothing — the defect is that the binary *rejects* the wrong order:
/// `find -name '*.txt' dir` is "illegal option -- n" on BSD and "paths must
/// precede expression" on GNU, while `find dir -L` is "unknown primary or
/// operator" / "unknown predicate". Only executing it can tell them apart.
///
/// The option case is the one the first version of this test missed: it
/// exercised only path+predicate, so the fix that moved the predicates behind
/// the path also moved the options there — breaking them — undetected.
///
/// The environment precondition is explicit rather than assumed. Operand
/// placement comes only from a curated overlay (docs/overlays.md states it is
/// deliberately not derivable from a scan), so on a host whose `find` matches
/// no shipped overlay — BusyBox, or a BSD other than macOS/FreeBSD — there is
/// nothing to assert and the test reports that instead of blaming the renderer.
#[tokio::test]
async fn test_find_renders_options_before_the_path_and_predicates_after() {
    use apcore::Module;
    use apexe::adapter::CliToolConverter;
    use apexe::module::CliModule;

    let (tmp, config) = test_config();
    let sandbox = tmp.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    std::fs::write(sandbox.join("wanted.txt"), b"x").unwrap();
    std::fs::write(sandbox.join("ignored.log"), b"x").unwrap();
    let sandbox = sandbox.to_str().unwrap();

    let orchestrator = ScanOrchestrator::new(config);
    let outcome = orchestrator.scan(&["find".into()], true, 1);
    if outcome.tools.is_empty() {
        eprintln!("skipping: no `find` on PATH");
        return;
    }
    let tools = outcome.tools;
    let Some(overlay) = tools[0].overlay.clone() else {
        eprintln!(
            "skipping: this host's `find` matches no shipped overlay, and operand \
             placement is overlay-supplied only"
        );
        return;
    };

    let modules = CliToolConverter::new().convert(&tools[0]);
    let module = CliModule::from_scanned(&modules[0], 10_000).expect("binding should parse");

    // A predicate must follow the path.
    let by_predicate = module
        .execute(
            serde_json::json!({ "path": [sandbox], "name": "*.txt" }),
            &apcore::Context::anonymous(),
        )
        .await
        .expect("find should execute");
    assert_eq!(
        by_predicate["exit_code"], 0,
        "{overlay}: find rejected the predicate ordering: {by_predicate}"
    );
    let stdout = by_predicate["stdout"].as_str().unwrap_or_default();
    assert!(
        stdout.contains("wanted.txt") && !stdout.contains("ignored.log"),
        "{overlay}: the predicate did not take effect: {by_predicate}"
    );

    // A true option must PRECEDE the path. `-L` is spelled the same on both
    // variants and is a plain boolean, so one input covers BSD and GNU.
    let with_option = module
        .execute(
            serde_json::json!({ "path": [sandbox], "L": true, "name": "*.txt" }),
            &apcore::Context::anonymous(),
        )
        .await
        .expect("find should execute");
    assert_eq!(
        with_option["exit_code"], 0,
        "{overlay}: find rejected `-L` — an option rendered after the path is \
         'unknown primary or operator' (BSD) / 'unknown predicate' (GNU): {with_option}"
    );
    assert!(
        with_option["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("wanted.txt"),
        "{overlay}: the predicate stopped working once an option was added: {with_option}"
    );
}

/// Regression (#20, GNU half): the shipped `find` overlays must carry the
/// placement markers on both sides, asserted without needing a GNU host.
///
/// The end-to-end test above can only exercise whichever variant this machine
/// has. Nothing else pinned `find@gnu`, so deleting its markers would have left
/// the suite green on every host while every GNU predicate broke.
#[test]
fn test_shipped_find_overlays_declare_operand_and_flag_placement() {
    for variant in ["bsd", "gnu"] {
        let Some(dir) = corpus_dir() else { return };
        let path = dir.join(format!("find@{variant}.json"));
        let raw = std::fs::read_to_string(&path).expect("overlay should be readable");
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("overlay should parse");

        let path_operand = doc["positional_args"]
            .as_array()
            .expect("positional_args")
            .iter()
            .find(|arg| arg["name"] == "path")
            .unwrap_or_else(|| panic!("{}: no `path` operand", path.display()));
        assert_eq!(
            path_operand["before_flags"],
            true,
            "{}: `path` must render before the predicates",
            path.display()
        );

        let pre_operand: Vec<&str> = doc["flags"]
            .as_array()
            .expect("flags")
            .iter()
            .filter(|f| f["before_operands"] == true)
            .filter_map(|f| f["short"].as_str().or_else(|| f["long"].as_str()))
            .collect();
        // `-L` is the shared case; both variants reject it after the path.
        assert!(
            pre_operand.contains(&"-L"),
            "{}: find's true options must render before the path, got {pre_operand:?}",
            path.display()
        );
    }
}

/// Regression (#40): every shipped overlay flag whose value is optional must
/// say so, asserted on the JSON without needing the matching host.
///
/// `value_optional` is what makes the executor render `--flag=value`; without
/// it the value is emitted as a separate argv entry, which these tools read as
/// "no value, and here is an operand". `ls --color never .` then lists a file
/// called `never` and leaves colour ON — the caller's request inverted, not
/// merely dropped — and `mkdir --context ctx d` silently creates a spurious
/// directory. The marker reached the loader, the schema and the executor a
/// release before any overlay carried it, so the whole curated surface rendered
/// the wrong spelling with the suite green.
///
/// Each entry below was established by running the flag in both spellings
/// against the build its overlay's provenance records; see `docs/overlays.md`.
#[test]
fn test_shipped_overlays_mark_every_optional_value_flag() {
    // (overlay, flags whose value is optional and therefore must be attached)
    const OPTIONAL: &[(&str, &[&str])] = &[
        (
            "cp@gnu",
            &[
                "--backup",
                "--preserve",
                "--reflink",
                "--update",
                "--context",
            ],
        ),
        ("df@gnu", &["--output"]),
        ("diff@bsd", &["--context", "--unified", "--color"]),
        ("diff@gnu", &["--context", "--unified", "--color"]),
        ("grep@bsd", &["--context", "--color", "--colour"]),
        ("grep@gnu", &["--color", "--colour"]),
        ("ln@gnu", &["--backup"]),
        ("ls@bsd", &["--color"]),
        ("ls@gnu", &["--color", "--classify", "--hyperlink"]),
        ("mkdir@gnu", &["--context"]),
        ("mv@gnu", &["--backup", "--update"]),
        ("rm@gnu", &["--interactive", "--preserve-root"]),
        ("sort@apple", &["--check"]),
        ("tail@gnu", &["--follow"]),
        ("uniq@bsd", &["--all-repeated"]),
        ("uniq@gnu", &["--all-repeated", "--group"]),
        ("xargs@gnu", &["--eof", "--replace"]),
    ];

    let mut marked = 0;
    for (overlay, flags) in OPTIONAL {
        for long in *flags {
            let flag = overlay_flag(overlay, long);
            assert_eq!(
                flag["value_optional"], true,
                "{overlay}: {long} takes an optional value, so it must render as \
                 `{long}=<value>`; without the marker the value becomes an operand"
            );
            marked += 1;
        }
    }
    assert_eq!(
        marked, 34,
        "the verified set is 34 flags across 17 overlays"
    );
}

/// The other half of #40, and the reason the fix is a per-flag fact rather than
/// a heuristic: an enum is not evidence that a value is optional.
///
/// Every flag here is enum- or word-valued and looks exactly like the ones
/// above, yet each takes a *required* value — `ls --sort time .` and
/// `sort --sort general-numeric f` both work — so marking them would break the
/// spelling that currently works. `ls --context` and `mv --context` are listed
/// for a third reason: `-Z, --context` takes no value at all. Pinning the
/// near-misses is what stops the tempting "enum implies optional" shortcut from
/// being introduced later.
#[test]
fn test_shipped_overlays_leave_required_value_flags_unmarked() {
    const REQUIRED: &[(&str, &[&str])] = &[
        ("du@gnu", &["--time-style"]),
        ("grep@bsd", &["--after-context", "--before-context"]),
        ("grep@gnu", &["--context"]),
        (
            "ls@gnu",
            &[
                "--sort",
                "--format",
                "--time-style",
                "--quoting-style",
                "--indicator-style",
                "--context",
            ],
        ),
        ("mv@gnu", &["--context"]),
        ("sort@apple", &["--sort"]),
        ("sort@gnu", &["--sort"]),
        ("uniq@bsd", &["--skip-fields", "--skip-chars"]),
    ];

    for (overlay, flags) in REQUIRED {
        for long in *flags {
            let flag = overlay_flag(overlay, long);
            assert!(
                flag.get("value_optional").is_none(),
                "{overlay}: {long} takes a required value (or none at all), so it must \
                 render as `{long} <value>`; marking it would break a spelling that works"
            );
        }
    }
}

/// Read one flag out of a shipped overlay, by its long name.
/// An overlay's `annotations` block, or `None` when it declares none.
fn overlay_annotations(overlay: &str) -> Option<serde_json::Value> {
    let path = corpus_dir()?.join(format!("{overlay}.json"));
    let raw = std::fs::read_to_string(&path).expect("overlay should be readable");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("overlay should parse");
    doc.get("annotations").cloned()
}

/// A curated overlay replaces the scan result, `annotations` included — so a
/// tool the scanner correctly escalates can be *de*-escalated by an overlay
/// that predates the rule.
///
/// That is exactly what happened here: `EXEC_WRAPPER_TOOLS` marks `xargs`
/// destructive, and both `xargs` overlays kept `destructive: false`, so the
/// shipped data quietly overrode the fix. Each overlay's own description says
/// what the tool does — BSD's "executes utility", GNU's "Run COMMAND" — which
/// is the whole argument for the classification.
///
/// Every overlay whose tool runs a caller-supplied command belongs here.
#[test]
fn test_shipped_overlays_mark_command_executors_destructive() {
    const EXEC_WRAPPERS: &[&str] = &["xargs@bsd", "xargs@gnu"];

    for overlay in EXEC_WRAPPERS {
        let annotations = overlay_annotations(overlay)
            .unwrap_or_else(|| panic!("{overlay}: a command executor must state its annotations"));
        assert_eq!(
            annotations["destructive"], true,
            "{overlay}: this tool runs whatever it is handed, so the ACL must \
             deny it by default rather than fall through"
        );
        assert_eq!(
            annotations["readonly"], false,
            "{overlay}: a command executor is never readonly — the path guard \
             would judge its arguments as a reader's"
        );
    }
}

fn overlay_flag(overlay: &str, long: &str) -> serde_json::Value {
    let path = corpus_dir()
        .expect("caller must check corpus_dir() first")
        .join(format!("{overlay}.json"));
    let raw = std::fs::read_to_string(&path).expect("overlay should be readable");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("overlay should parse");
    doc["flags"]
        .as_array()
        .expect("flags")
        .iter()
        .find(|flag| flag["long"] == long)
        .unwrap_or_else(|| panic!("{}: no flag named {long}", path.display()))
        .clone()
}

// T46: Graceful degradation test
#[test]
fn test_graceful_degradation() {
    let (_tmp, config) = test_config();
    let orchestrator = ScanOrchestrator::new(config);

    // 'true' is a tool that produces no help output
    let outcome = orchestrator.scan(&["true".into()], true, 1);

    // Should not panic or crash
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let tools = outcome.tools;
    assert_eq!(tools.len(), 1);

    let tool = &tools[0];
    assert_eq!(tool.name, "true");
    assert!(!tool.binary_path.is_empty());
    // May have warnings about empty help
    // The key test: no crash, no panic
}
