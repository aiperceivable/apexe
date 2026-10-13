#![cfg(unix)]
use assert_cmd::Command;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    binary: PathBuf,
    overlay: PathBuf,
    cases: PathBuf,
}

impl Fixture {
    fn new(body: &str, destructive: bool) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let binary = home.path().join("verification-fixture");
        let script = format!("#!/bin/sh\ncase \"$1\" in\n--help) printf 'Usage: verification-fixture [OPTIONS] FILE\\nOptions:\\n  --dry-run  Review without writes\\n'; exit 0;;\n--version) echo 'fixture 1.0'; exit 0;;\n--dry-run) ;;\n*) exit 0;;\nesac\n{body}\n");
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let overlay = home.path().join("fixture-overlay.json");
        let cases = home.path().join("cases.json");
        std::fs::write(&overlay, json!({"schema_version":"1.1","command":"verification-fixture",
            "variant":"unknown","mode":"authoritative","confidence":"low",
            "description":"Synthetic verification fixture", "flags":[{"long":"--dry-run","type":"boolean"}],
            "positional_args":[{"name":"file","type":"path","required":true}],
            "annotations":{"readonly":false,"idempotent":!destructive,"destructive":destructive,
                "requires_approval":destructive,"open_world":false}}).to_string()).unwrap();
        let fixture = Self {
            home,
            binary,
            overlay,
            cases,
        };
        fixture
            .command()
            .args(["scan"])
            .arg(&fixture.binary)
            .arg("--overlay")
            .arg(&fixture.overlay)
            .arg("--no-cache")
            .assert()
            .success();
        fixture.write_cases(
            if destructive { "dry_run" } else { "isolated" },
            "dry-run\n",
        );
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("HOME", self.home.path())
            .env("APEXE_OVERLAY_DIRS", "")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env("RUST_LOG", "error")
            .current_dir(self.home.path());
        command
    }

    fn bindings(&self) -> PathBuf {
        self.home.path().join(".apexe/bindings")
    }

    fn write_cases(&self, mode: &str, output: &str) {
        let digest = format!("{:x}", Sha256::digest(std::fs::read(&self.binary).unwrap()));
        std::fs::write(&self.cases, json!({"allowlist":["cli.verification_fixture"],"cases":[{
            "module_id":"cli.verification_fixture","mode":mode,"binary_digest":digest,
            "dry_run_parameter":"dry_run","input":{"dry_run":true,"file":"$FIXTURE/input"},
            "fixtures":{"input":"original"},"expected_stdout":output,"expected_files":{"input":"original"}
        }]}).to_string()).unwrap();
    }

    fn verify(&self, arguments: &[&str]) -> Value {
        let output = self
            .command()
            .args(["verify", "--cases"])
            .arg(&self.cases)
            .arg("--overlay")
            .arg(&self.overlay)
            .args(arguments)
            .output()
            .unwrap();
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"))
    }

    fn edit_cases(&self, edit: impl FnOnce(&mut Value)) {
        let mut cases: Value =
            serde_json::from_slice(&std::fs::read(&self.cases).unwrap()).unwrap();
        edit(&mut cases);
        std::fs::write(&self.cases, cases.to_string()).unwrap();
    }
}

fn isolation_available(report: &Value) -> bool {
    if report["behavioral"][0]["status"] == "unverifiable" {
        assert!(
            report["behavioral"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("isolation unavailable"),
            "{report}"
        );
        return false;
    }
    assert!(report["behavioral"][0]["isolation"] != "none", "{report}");
    true
}

#[test]
fn test_dry_run_requires_opt_in_and_never_executes_a_missing_or_false_switch() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    assert_eq!(
        fixture.verify(&[])["behavioral"][0]["status"],
        "unverifiable"
    );
    fixture.edit_cases(|cases| cases["cases"][0]["input"]["dry_run"] = json!(false));
    let report = fixture.verify(&["--allow-dry-run"]);
    assert_eq!(report["behavioral"][0]["status"], "unverifiable");
    assert!(report["behavioral"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("must be true"));
}

#[test]
fn test_dry_run_requires_matching_binary_digest_and_mandatory_isolation() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    assert_eq!(
        fixture.verify(&["--allow-dry-run", "--isolation", "none"])["behavioral"][0]["status"],
        "unverifiable"
    );
    fixture.edit_cases(|cases| cases["cases"][0]["binary_digest"] = json!("0".repeat(64)));
    let report = fixture.verify(&["--allow-dry-run"]);
    assert_eq!(report["behavioral"][0]["status"], "unverifiable");
    assert!(report["behavioral"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("binary_digest"));
}

#[test]
fn test_dry_run_executes_in_isolation_without_rewriting_original_bindings() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    let binding = fixture
        .bindings()
        .join("cli.verification_fixture.binding.yaml");
    let before = std::fs::read(&binding).unwrap();
    let report = fixture.verify(&["--allow-dry-run", "--isolation", "required"]);
    assert_eq!(std::fs::read(binding).unwrap(), before);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "passed", "{report}");
    assert_eq!(report["behavioral"][0]["mode"], "dry_run");
}

#[test]
fn test_recorded_confidence_is_earned_and_invalidated_by_binding_and_binary_changes() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    let report = fixture.verify(&["--allow-dry-run", "--record"]);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "passed", "{report}");
    assert_eq!(report["drift"][0]["status"], "unchanged", "{report}");
    let load = || apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    let mut modules = load();
    assert_eq!(modules[0].metadata["confidence"], "verified");
    assert_eq!(
        modules[0].metadata["verification"]["scope"],
        "explicit_cases"
    );
    assert!(modules[0].annotations.as_ref().unwrap().requires_approval);
    modules[0].input_schema["description"] = json!("Changed contract");
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    assert_ne!(load()[0].metadata["confidence"], "verified");
    assert!(!load()[0].metadata.contains_key("verification"));
    fixture
        .command()
        .arg("scan")
        .arg(&fixture.binary)
        .arg("--overlay")
        .arg(&fixture.overlay)
        .arg("--no-cache")
        .assert()
        .success();
    let report = fixture.verify(&["--allow-dry-run", "--record"]);
    assert_eq!(report["behavioral"][0]["status"], "passed");
    let mut script = std::fs::read_to_string(&fixture.binary).unwrap();
    script.push_str("\n# New build\n");
    std::fs::write(&fixture.binary, script).unwrap();
    assert_ne!(load()[0].metadata["confidence"], "verified");
}

#[test]
fn test_a_failed_case_replaces_previous_successful_evidence() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    let report = fixture.verify(&["--allow-dry-run", "--record"]);
    if !isolation_available(&report) {
        return;
    }
    fixture.edit_cases(|cases| cases["cases"][0]["expected_stdout"] = json!("wrong"));
    assert_eq!(
        fixture.verify(&["--allow-dry-run", "--record"])["behavioral"][0]["status"],
        "failed"
    );
    let modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    assert_ne!(modules[0].metadata["confidence"], "verified");
}

#[test]
fn test_dry_run_cannot_mutate_even_fixture_paths() {
    let fixture = Fixture::new("printf mutation > \"$2\"; printf 'dry-run\\n'", true);
    let report = fixture.verify(&["--allow-dry-run"]);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "failed", "{report}");
}

#[test]
fn test_isolated_cases_check_temporary_file_effects() {
    let fixture = Fixture::new("printf changed > \"$2\"; printf 'dry-run\\n'", false);
    fixture.edit_cases(|cases| cases["cases"][0]["expected_files"]["input"] = json!("changed"));
    let report = fixture.verify(&[]);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "passed", "{report}");
}

#[test]
fn test_isolation_blocks_real_filesystem_writes() {
    let sentinel = tempfile::tempdir().unwrap();
    let path = sentinel.path().join("outside");
    let fixture = Fixture::new(
        &format!("printf escaped > {}; printf 'dry-run\\n'", path.display()),
        false,
    );
    assert!(
        !path.exists(),
        "Source introspection must not execute the fixture body"
    );
    let report = fixture.verify(&[]);
    assert!(!path.exists());
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "failed", "{report}");
}

#[test]
fn test_isolation_blocks_host_network_connections() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let curl = which::which("curl").unwrap();
    let fixture = Fixture::new(
        &format!(
            "{} --max-time 1 --silent http://{}/ >/dev/null 2>&1; printf 'dry-run\\n'",
            curl.display(),
            listener.local_addr().unwrap()
        ),
        false,
    );
    let report = fixture.verify(&[]);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "passed", "{report}");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn test_no_case_input_or_open_world_annotation_can_bypass_admission() {
    let fixture = Fixture::new("printf 'dry-run\\n'", false);
    fixture.edit_cases(|cases| cases["allowlist"] = json!([]));
    assert_eq!(
        fixture.verify(&[])["behavioral"][0]["status"],
        "unverifiable"
    );
    fixture.write_cases("isolated", "dry-run\n");
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0].annotations.as_mut().unwrap().open_world = true;
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    assert_eq!(
        fixture.verify(&[])["behavioral"][0]["status"],
        "unverifiable"
    );
}

#[test]
fn test_record_and_dry_run_options_require_explicit_cases() {
    for flag in ["--record", "--allow-dry-run"] {
        Command::cargo_bin("apexe")
            .unwrap()
            .args(["verify", flag])
            .assert()
            .failure()
            .code(2);
    }
}

#[test]
fn test_author_written_verified_metadata_is_never_earned_evidence() {
    let fixture = Fixture::new("printf 'dry-run\\n'", false);
    let mut modules = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    modules[0]
        .metadata
        .insert("confidence".into(), json!("verified"));
    modules[0]
        .metadata
        .insert("verification".into(), json!({"scope":"all_commands"}));
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    let loaded = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    assert_ne!(loaded[0].metadata["confidence"], "verified");
    assert!(!loaded[0].metadata.contains_key("verification"));
}

#[test]
fn test_case_file_changes_and_executor_metadata_invalidate_evidence() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    let report = fixture.verify(&["--allow-dry-run", "--record"]);
    if !isolation_available(&report) {
        return;
    }
    fixture.edit_cases(|cases| cases["cases"][0]["expected_files"]["input"] = json!("different"));
    let load = || apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    assert_ne!(load()[0].metadata["confidence"], "verified");
    fixture.write_cases("dry_run", "dry-run\n");
    fixture.verify(&["--allow-dry-run", "--record"]);
    let mut modules = load();
    assert_eq!(modules[0].metadata["confidence"], "verified");
    modules[0]
        .metadata
        .insert("json_flag".into(), json!("--json"));
    apexe::output::YamlOutput::without_verification()
        .write(&modules, &fixture.bindings(), false)
        .unwrap();
    assert_ne!(load()[0].metadata["confidence"], "verified");
}

#[test]
fn test_corrupt_or_symlinked_records_fail_closed() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    fixture.verify(&["--allow-dry-run", "--record"]);
    let record = fixture.bindings().join(".apexe-verification/evidence.json");
    std::fs::write(&record, "{invalid json}").unwrap();
    assert!(apexe::output::load_modules_from_dir(&fixture.bindings(), None).is_err());
    let sentinel = fixture.home.path().join("sentinel");
    std::fs::write(&sentinel, "preserved").unwrap();
    std::fs::remove_file(&record).unwrap();
    symlink(&sentinel, &record).unwrap();
    assert!(apexe::output::load_modules_from_dir(&fixture.bindings(), None).is_err());
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "preserved");
}

#[test]
fn test_mixed_passed_and_failed_cases_cannot_promote_module_confidence() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    fixture.edit_cases(|cases| {
        let mut failed = cases["cases"][0].clone();
        failed["expected_stdout"] = json!("wrong");
        cases["cases"].as_array_mut().unwrap().push(failed);
    });
    let report = fixture.verify(&["--allow-dry-run", "--record"]);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "passed");
    assert_eq!(report["behavioral"][1]["status"], "failed");
    let loaded = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    assert_ne!(loaded[0].metadata["confidence"], "verified");
}

#[test]
fn test_digest_pinned_dry_run_supports_fixture_free_boolean_input() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    let mut overlay: Value =
        serde_json::from_slice(&std::fs::read(&fixture.overlay).unwrap()).unwrap();
    overlay["positional_args"] = json!([]);
    std::fs::write(&fixture.overlay, overlay.to_string()).unwrap();
    fixture
        .command()
        .arg("scan")
        .arg(&fixture.binary)
        .arg("--overlay")
        .arg(&fixture.overlay)
        .arg("--no-cache")
        .assert()
        .success();
    fixture.edit_cases(|cases| {
        cases["cases"][0]["input"] = json!({"dry_run":true});
        cases["cases"][0]["fixtures"] = json!({});
        cases["cases"][0]["expected_files"] = json!({});
    });
    let report = fixture.verify(&["--allow-dry-run"]);
    if !isolation_available(&report) {
        return;
    }
    assert_eq!(report["behavioral"][0]["status"], "passed", "{report}");
}

#[test]
fn test_readonly_annotations_cannot_earn_evidence_for_fixture_writes() {
    let fixture = Fixture::new("printf changed > \"$2\"; printf 'dry-run\\n'", false);
    let mut overlay: Value =
        serde_json::from_slice(&std::fs::read(&fixture.overlay).unwrap()).unwrap();
    overlay["annotations"]["readonly"] = json!(true);
    std::fs::write(&fixture.overlay, overlay.to_string()).unwrap();
    fixture
        .command()
        .arg("scan")
        .arg(&fixture.binary)
        .arg("--overlay")
        .arg(&fixture.overlay)
        .arg("--no-cache")
        .assert()
        .success();
    fixture.edit_cases(|cases| cases["cases"][0]["expected_files"]["input"] = json!("changed"));
    let report = fixture.verify(&[]);
    assert_eq!(report["behavioral"][0]["status"], "unverifiable");
    assert!(report["behavioral"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("Readonly"));
}

#[test]
fn test_verifier_identity_changes_invalidate_previous_evidence() {
    let fixture = Fixture::new("printf 'dry-run\\n'", true);
    let report = fixture.verify(&["--allow-dry-run", "--record"]);
    if !isolation_available(&report) {
        return;
    }
    let path = fixture.bindings().join(".apexe-verification/evidence.json");
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["verifier"]["path"] = json!(fixture.home.path().join("missing-verifier"));
    std::fs::write(&path, record.to_string()).unwrap();
    let loaded = apexe::output::load_modules_from_dir(&fixture.bindings(), None).unwrap();
    assert_ne!(loaded[0].metadata["confidence"], "verified");
}
