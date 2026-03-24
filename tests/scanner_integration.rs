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
    };
    (tmp, config)
}

// T44: Scan git integration test
#[test]
#[ignore]
fn test_scan_git() {
    let (_tmp, config) = test_config();
    let orchestrator = ScanOrchestrator::new(config);

    let results = orchestrator
        .scan(&["git".into()], true, 2)
        .expect("Failed to scan git");

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

    let results = orchestrator
        .scan(&["docker".into()], true, 2)
        .expect("Failed to scan docker");

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

// T46: Graceful degradation test
#[test]
fn test_graceful_degradation() {
    let (_tmp, config) = test_config();
    let orchestrator = ScanOrchestrator::new(config);

    // 'true' is a tool that produces no help output
    let results = orchestrator.scan(&["true".into()], true, 1);

    // Should not panic or crash
    assert!(results.is_ok());
    let tools = results.unwrap();
    assert_eq!(tools.len(), 1);

    let tool = &tools[0];
    assert_eq!(tool.name, "true");
    assert!(!tool.binary_path.is_empty());
    // May have warnings about empty help
    // The key test: no crash, no panic
}
