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
        let path = format!("overlays/find@{variant}.json");
        let raw = std::fs::read_to_string(&path).expect("overlay should be readable");
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("overlay should parse");

        let path_operand = doc["positional_args"]
            .as_array()
            .expect("positional_args")
            .iter()
            .find(|arg| arg["name"] == "path")
            .unwrap_or_else(|| panic!("{path}: no `path` operand"));
        assert_eq!(
            path_operand["before_flags"], true,
            "{path}: `path` must render before the predicates"
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
            "{path}: find's true options must render before the path, got {pre_operand:?}"
        );
    }
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
