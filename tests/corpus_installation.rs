use assert_cmd::Command;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    repository: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        let home = directory.path().join("home");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let fixture = Self {
            directory,
            repository,
            home,
        };
        fixture.git(&["init", "--quiet"]);
        fixture.write_overlay("overlays", "fixture", "--ready");
        fixture.write_manifest("fixture-corpus", "0.1.0", "overlays");
        fixture.commit();
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("HOME", &self.home)
            .env_remove("APEXE_OVERLAY_DIRS")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .env_remove("APEXE_SHIM_DIR")
            .env("RUST_LOG", "error")
            .current_dir(self.directory.path());
        command
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repository)
            .args([
                "-c",
                "user.name=Corpus Test",
                "-c",
                "user.email=tests@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(arguments)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit(&self) -> String {
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "-m", "Update test corpus"]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn write_overlay(&self, subdir: &str, name: &str, flag: &str) {
        let directory = self.repository.join(subdir);
        std::fs::create_dir_all(&directory).unwrap();
        let overlay = json!({"schema_version":"1.1","command":name,"variant":"unknown",
            "mode":"authoritative","confidence":"low", "cross_flag_ordering":false,
            "short_flag_bundling":false,"streaming_stdin_safe":false,
            "flags":[{"long":flag,"type":"boolean","description":"Test operation"}],
            "annotations":{"readonly":true,"destructive":false,"idempotent":true,
                "requires_approval":false,"open_world":false}});
        std::fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&overlay).unwrap(),
        )
        .unwrap();
    }

    fn write_manifest(&self, name: &str, version: &str, subdir: &str) {
        let manifest = json!({"schema_version":"1.0","name":name,"description":"Synthetic test corpus",
            "version":version,"overlay_directory":subdir,"license":"Apache-2.0"});
        std::fs::write(
            self.repository.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn install(&self, arguments: &[&str]) -> Value {
        result(
            self.command()
                .args(["overlays", "install"])
                .arg(&self.repository)
                .args(arguments),
        )
    }

    fn list(&self) -> Value {
        result(self.command().args(["overlays", "list"]))
    }

    fn root(&self) -> PathBuf {
        self.home.join(".apexe/corpora")
    }
}

fn result(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().clone();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn test_install_reads_manifest_and_registers_resolved_commit() {
    let fixture = Fixture::new();
    let expected = fixture.git(&["rev-parse", "HEAD"]);
    let installed = fixture.install(&[]);
    assert_eq!(installed["name"], "fixture-corpus");
    assert_eq!(installed["manifest"]["version"], "0.1.0");
    assert_eq!(installed["overlay_count"], 1);
    assert_eq!(installed["commit"], expected);
    assert_eq!(fixture.list()[0], installed);
    assert!(!fixture.home.join(".apexe/config.yaml").exists());
}

#[test]
fn test_install_without_manifest_supports_legacy_corpora() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.repository.join("manifest.json")).unwrap();
    fixture.commit();
    let installed = fixture.install(&[]);
    assert_eq!(installed["name"], "repository");
    assert!(installed.get("manifest").is_none());
    assert_eq!(installed["overlay_count"], 1);
}

#[test]
fn test_install_honors_name_and_subdirectory_overrides() {
    let fixture = Fixture::new();
    fixture.write_overlay("alternate", "other", "--alternate");
    fixture.commit();
    let installed = fixture.install(&["--name", "repository", "--subdir", "alternate"]);
    assert_eq!(installed["name"], "repository");
    assert_eq!(installed["subdirectory"], "alternate");
    assert_eq!(installed["subdirectory_override"], "alternate");
    assert_eq!(installed["manifest"]["name"], "fixture-corpus");
}

#[test]
fn test_update_refreshes_commit_and_manifest_without_invalidating_old_snapshot() {
    let fixture = Fixture::new();
    let initial = fixture.install(&[]);
    fixture.write_overlay("new-overlays", "extra", "--extra");
    fixture.write_manifest("fixture-corpus", "0.2.0", "new-overlays");
    let commit = fixture.commit();
    let updated = result(
        fixture
            .command()
            .args(["overlays", "update", "fixture-corpus"]),
    );
    assert_eq!(updated["commit"], commit);
    assert_ne!(updated["snapshot"], initial["snapshot"]);
    assert_eq!(updated["manifest"]["version"], "0.2.0");
    assert_eq!(updated["subdirectory"], "new-overlays");
    assert!(fixture
        .root()
        .join("packages/fixture-corpus")
        .join(initial["snapshot"].as_str().unwrap())
        .join("overlays/fixture.json")
        .exists());
}

#[test]
fn test_update_preserves_subdirectory_override() {
    let fixture = Fixture::new();
    fixture.write_overlay("alternate", "extra", "--extra");
    fixture.commit();
    fixture.install(&["--subdir", "alternate"]);
    fixture.write_manifest("fixture-corpus", "0.2.0", "missing-directory");
    fixture.commit();
    let updated = result(
        fixture
            .command()
            .args(["overlays", "update", "fixture-corpus"]),
    );
    assert_eq!(updated["subdirectory"], "alternate");
    assert_eq!(updated["manifest"]["version"], "0.2.0");
}

#[test]
fn test_pinned_commit_does_not_follow_head_until_explicitly_changed() {
    let fixture = Fixture::new();
    let initial = fixture.git(&["rev-parse", "HEAD"]);
    fixture.install(&["--ref", &initial]);
    fixture.write_manifest("fixture-corpus", "0.2.0", "overlays");
    let latest = fixture.commit();
    let pinned = result(
        fixture
            .command()
            .args(["overlays", "update", "fixture-corpus"]),
    );
    assert_eq!(pinned["commit"], initial);
    let changed =
        result(
            fixture
                .command()
                .args(["overlays", "update", "fixture-corpus", "--ref", "HEAD"]),
        );
    assert_eq!(changed["commit"], latest);
}

#[test]
fn test_invalid_update_preserves_catalog_and_current_snapshot() {
    let fixture = Fixture::new();
    let previous = fixture.install(&[]);
    std::fs::write(
        fixture.repository.join("overlays/fixture.json"),
        "{invalid json}",
    )
    .unwrap();
    fixture.commit();
    fixture
        .command()
        .args(["overlays", "update", "fixture-corpus"])
        .assert()
        .failure();
    assert_eq!(fixture.list()[0], previous);
    assert!(!fixture.root().join(".lock").exists());
}

#[test]
fn test_invalid_manifest_is_rejected_without_registering_installation() {
    let fixture = Fixture::new();
    for version in ["invalid", "01.2.3"] {
        fixture.write_manifest("fixture-corpus", version, "overlays");
        fixture.commit();
        fixture
            .command()
            .args(["overlays", "install"])
            .arg(&fixture.repository)
            .assert()
            .failure();
        assert_eq!(fixture.list(), json!([]));
    }
}

#[test]
fn test_install_rejects_unsupported_overlay_version_and_unbacked_verified_claim() {
    let fixture = Fixture::new();
    let path = fixture.repository.join("overlays/fixture.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut invalid = original.clone();
    invalid["schema_version"] = json!("9.0");
    for overlay in [invalid, {
        let mut value = original;
        value["confidence"] = json!("verified");
        value
    }] {
        std::fs::write(&path, serde_json::to_vec(&overlay).unwrap()).unwrap();
        fixture.commit();
        fixture
            .command()
            .args(["overlays", "install"])
            .arg(&fixture.repository)
            .assert()
            .failure();
        assert_eq!(fixture.list(), json!([]));
    }
}

#[test]
fn test_remove_deletes_only_owned_corpus_and_unregisters_it() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    std::fs::write(fixture.root().join("unrelated.txt"), "preserved").unwrap();
    fixture
        .command()
        .args(["overlays", "remove", "fixture-corpus"])
        .assert()
        .success();
    assert_eq!(fixture.list(), json!([]));
    assert!(!fixture.root().join("packages/fixture-corpus").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("unrelated.txt")).unwrap(),
        "preserved"
    );
}

#[test]
fn test_remove_refuses_modified_ownership_marker() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let marker = fixture
        .root()
        .join("packages/fixture-corpus/.apexe-corpus.json");
    std::fs::write(&marker, "{}").unwrap();
    fixture
        .command()
        .args(["overlays", "remove", "fixture-corpus"])
        .assert()
        .failure();
    assert!(marker.exists());
}

#[test]
fn test_install_rejects_duplicate_names_and_preserves_original_record() {
    let fixture = Fixture::new();
    let previous = fixture.install(&[]);
    fixture
        .command()
        .args(["overlays", "install"])
        .arg(&fixture.repository)
        .assert()
        .failure();
    assert_eq!(fixture.list()[0], previous);
}

#[test]
fn test_install_rejects_path_traversal_and_unsafe_repository_schemes() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["--name", "../outside"],
        vec!["--subdir", "../outside"],
        vec!["--ref", "--upload-pack=bad"],
    ] {
        fixture
            .command()
            .args(["overlays", "install"])
            .arg(&fixture.repository)
            .args(arguments)
            .assert()
            .failure();
    }
    for repository in [
        "ext::sh -c anything",
        "ssh://git@example.invalid/repository",
        "https://user:secret@example.invalid/repository",
    ] {
        fixture
            .command()
            .args(["overlays", "install", repository])
            .assert()
            .failure();
    }
    assert_eq!(fixture.list(), json!([]));
}

#[cfg(unix)]
#[test]
fn test_install_rejects_overlay_and_directory_symlinks() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let path = fixture.repository.join("overlays/fixture.json");
    std::fs::remove_file(&path).unwrap();
    symlink("../../outside.json", &path).unwrap();
    fixture.commit();
    fixture
        .command()
        .args(["overlays", "install"])
        .arg(&fixture.repository)
        .assert()
        .failure();
    std::fs::remove_dir_all(fixture.repository.join("overlays")).unwrap();
    symlink("../outside", fixture.repository.join("overlays")).unwrap();
    fixture.commit();
    fixture
        .command()
        .args(["overlays", "install"])
        .arg(&fixture.repository)
        .assert()
        .failure();
    assert_eq!(fixture.list(), json!([]));
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn test_installed_corpus_is_discovered_and_governs_a_nonbuiltin_cli() {
    let fixture = Fixture::new();
    fixture.install(&[]);
    let binary = fixture.directory.path().join("fixture");
    write_executable(&binary, "case \"$1\" in --version) echo 'fixture 1.0';; --ready) echo ready;; *) printf 'Usage: fixture [OPTIONS]\\nOptions:\\n  --ready  Test operation\\n';; esac");
    let scan = result(fixture.command().args(["scan"]).arg(&binary).args([
        "--no-cache",
        "--format",
        "json",
    ]));
    assert!(scan["commands"][0]["input_schema"]["properties"]
        .get("ready")
        .is_some());
    fixture
        .command()
        .args(["env", "--enable", "--shell", "bash"])
        .assert()
        .success();
    Command::new(fixture.home.join(".apexe/bin/fixture"))
        .env("HOME", &fixture.home)
        .arg("--ready")
        .assert()
        .success()
        .stdout("ready\n");
    fixture
        .command()
        .args(["overlays", "remove", "fixture-corpus"])
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn test_install_does_not_execute_repository_scripts_or_inherited_git_hooks() {
    let fixture = Fixture::new();
    let sentinel = fixture.directory.path().join("executed");
    let template = fixture.directory.path().join("template");
    std::fs::create_dir_all(template.join("hooks")).unwrap();
    write_executable(
        &template.join("hooks/post-checkout"),
        &format!("touch '{}'", sentinel.display()),
    );
    write_executable(
        &fixture.repository.join("install.sh"),
        &format!("touch '{}'", sentinel.display()),
    );
    fixture.commit();
    fixture
        .command()
        .env("GIT_TEMPLATE_DIR", &template)
        .args(["overlays", "install"])
        .arg(&fixture.repository)
        .assert()
        .success();
    assert!(!sentinel.exists());
}

#[test]
fn test_install_invalid_reference_preserves_empty_catalog_and_releases_lock() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["overlays", "install"])
        .arg(&fixture.repository)
        .args(["--ref", "missing-reference"])
        .assert()
        .failure();
    assert_eq!(fixture.list(), json!([]));
    assert!(!fixture.root().join(".lock").exists());
    fixture.install(&[]);
}
