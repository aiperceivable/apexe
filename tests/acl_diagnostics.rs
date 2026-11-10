#![cfg(unix)]

//! What `apexe list --diagnostics` reports about a loaded ACL, and what
//! `AclManager::merge_default` retires when a tool is rescanned.
//!
//! The CLI half is here because the defect it covers is only observable through
//! the command: a grant that matches no loaded module used to have no reporting
//! surface at all. The merge half is here rather than only in `acl.rs` because
//! the public seam is what `scan` itself calls, and asserting through
//! `explain`/`merge_default` keeps the test independent of an installed tool:
//! `ScanArgs::write_acl` is `pub(super)`, so driving a real `apexe scan` would
//! mean depending on one machine's `--help` output.

use apcore::ModuleAnnotations;
use apcore_toolkit::ScannedModule;
use assert_cmd::Command;
use predicates::str::contains;
use serde_json::{json, Value};
use std::path::PathBuf;
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

    fn config_dir(&self) -> PathBuf {
        self.home.path().join(".apexe")
    }

    fn bindings(&self) -> PathBuf {
        self.config_dir().join("bindings")
    }

    fn acl_path(&self) -> PathBuf {
        self.config_dir().join("acl.yaml")
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("APEXE_OFFLINE", "true")
            .env("HOME", self.home.path())
            .env("RUST_LOG", "error")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APCORE_CONFIG")
            .current_dir(self.home.path());
        command
    }

    /// A readonly module id, in the shape `merge_default` files under the
    /// generated allow rule.
    ///
    /// `open_world` is set explicitly rather than defaulted: apcore's default
    /// is `true` (MCP's conservative `openWorldHint`), and `readonly_ids`
    /// disqualifies an open-world module even when it is readonly. Defaulting
    /// it here would produce no allow rule at all, so this case would "pass"
    /// for the wrong reason. See the same trap in `governance::acl`'s own test
    /// helper.
    fn module(&self, module_id: &str) -> ScannedModule {
        let mut module = ScannedModule::new(
            module_id.into(),
            "ACL diagnostics test module".into(),
            json!({"type":"object","properties":{},"additionalProperties":false}),
            json!({"type":"object"}),
            vec!["cli".into()],
            "exec:///bin/echo".into(),
        );
        module.annotations = Some(ModuleAnnotations {
            readonly: true,
            open_world: false,
            ..Default::default()
        });
        module
    }

    fn binding(&self, module_id: &str) {
        let module = self.module(module_id);
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.bindings(), false)
            .unwrap();
    }

    fn acl(&self, policy: &str) {
        std::fs::create_dir_all(self.config_dir()).unwrap();
        std::fs::write(self.acl_path(), policy).unwrap();
    }

    fn diagnostics_json(&self) -> Value {
        let output = self
            .command()
            .args(["list", "--diagnostics", "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }
}

#[test]
fn test_list_diagnostics_reports_acl_targets_with_no_loaded_module() {
    // The reporting gap this closes: the grant is in `acl.yaml`, no module can
    // match it, and before this report nothing a human reads said so.
    let fixture = Fixture::new();
    fixture.binding("cli.alpha");
    fixture.acl(
        "rules:\n  - callers: [\"*\"]\n    targets: [\"cli.alpha\", \"cli.gone\"]\n    \
         effect: allow\ndefault_effect: deny\n",
    );

    let report = fixture.diagnostics_json();
    let unmatched = report["unmatched_acl_targets"]
        .as_array()
        .expect("a loaded policy adds the ACL half of the report");
    assert_eq!(unmatched.len(), 1, "{report}");
    assert_eq!(unmatched[0]["pattern"], "cli.gone");
    assert_eq!(unmatched[0]["rule_index"], 0);
    assert_eq!(unmatched[0]["effect"], "allow");
    assert!(
        report.get("scan_inventory").is_some(),
        "the closed-route half keeps its own key: {report}"
    );
}

#[test]
fn test_list_diagnostics_without_a_policy_keeps_the_inventory_shape() {
    // Compatibility: installations that never wrote an ACL must keep parsing
    // the document they always got — a bare map of tool to closed routes.
    let fixture = Fixture::new();
    fixture.binding("cli.alpha");

    let report = fixture.diagnostics_json();
    assert!(
        report.is_object(),
        "the report stays a map when no policy is loaded: {report}"
    );
    assert!(
        report.get("unmatched_acl_targets").is_none(),
        "no policy means nothing to check, so no ACL key: {report}"
    );
}

#[test]
fn test_list_diagnostics_table_reports_a_clean_policy_as_none() {
    let fixture = Fixture::new();
    fixture.binding("cli.alpha");
    fixture.acl(
        "rules:\n  - callers: [\"*\"]\n    targets: [\"cli.alpha\"]\n    effect: allow\n\
         default_effect: deny\n",
    );

    fixture
        .command()
        .args(["list", "--diagnostics"])
        .assert()
        .success()
        .stdout(contains("ACL targets with no loaded module"))
        .stdout(contains("  none."));
}

#[test]
fn test_merge_default_retires_the_grant_of_a_route_the_rescan_dropped() {
    // `scan` calls this with the tools it just rescanned. A curated overlay that
    // narrows its declared surface stops producing the closed route, so its id
    // is absent from the batch and used to stay in the generated rule with
    // nothing to match — until the route was reopened and inherited an allow
    // nobody reviewed.
    let fixture = Fixture::new();
    let wider = vec![
        fixture.module("cli.git.log"),
        fixture.module("cli.git.status"),
        fixture.module("cli.docker.ps"),
    ];
    let path = fixture.acl_path();
    std::fs::create_dir_all(fixture.config_dir()).unwrap();
    apexe::governance::AclManager::generate_default(&wider)
        .write_config(&path)
        .unwrap();

    // Prove the starting point before asserting on the merge: if this fixture
    // ever stops producing an allow (apcore defaults `open_world` to true, which
    // disqualifies a module from the generated allow rule), the prune assertion
    // below would pass for the wrong reason.
    let before = apexe::governance::AclManager::from_config(&path).unwrap();
    assert_eq!(
        before.explain_external("cli.git.log").effect,
        "allow",
        "the fixture must start from a live grant, or the prune assertion proves nothing"
    );

    // `git` is rescanned and now produces only `status`; `docker` is untouched.
    let narrowed = vec![fixture.module("cli.git.status")];
    let merged = apexe::governance::AclManager::merge_default(&path, &narrowed).unwrap();

    let dropped = merged.explain_external("cli.git.log");
    assert_eq!(
        dropped.matched_rule_index, None,
        "a grant for a closed route must not survive the rescan"
    );
    assert_eq!(dropped.effect, "deny");
    assert_eq!(
        merged.explain_external("cli.git.status").effect,
        "allow",
        "the route that still exists keeps its grant"
    );
    assert_eq!(
        merged.explain_external("cli.docker.ps").effect,
        "allow",
        "a tool this batch never touched keeps its grant"
    );
}
