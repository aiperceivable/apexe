#![cfg(unix)]

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
        let fixture = Self {
            home: TempDir::new().unwrap(),
        };
        fixture.write_binding("exec:///bin/cat");
        fixture.write_acl("rules: []\ndefault_effect: deny\n");
        fixture
            .command()
            .args(["env", "--enable"])
            .assert()
            .success();
        fixture
    }

    fn directory(&self) -> PathBuf {
        self.home.path().join(".apexe/bin")
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("apexe").unwrap();
        command
            .env("HOME", self.home.path())
            .env("SHELL", "/bin/sh")
            .env("PATH", "/usr/bin:/bin")
            .env("RUST_LOG", "error")
            .env_remove("APEXE_SHIM_DIR")
            .env_remove("APEXE_BINDINGS_DIR")
            .env_remove("APCORE_CONFIG")
            .current_dir(self.home.path());
        command
    }

    fn check(&self) -> Command {
        let mut command = self.command();
        command
            .args(["env", "--check"])
            .env(
                "PATH",
                std::env::join_paths([
                    self.directory(),
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/bin"),
                ])
                .unwrap(),
            )
            .env("APEXE_SHIM_DIR", self.directory());
        command
    }

    fn write_binding(&self, target: &str) {
        let mut module = ScannedModule::new(
            "cli.cat".into(),
            "Check fixture".into(),
            json!({"type":"object","properties":{},"additionalProperties":false,
                "x-apexe-streaming-stdin-safe":true,"x-apexe-short-flag-bundling":true,
                "x-apexe-cross-flag-ordering":false}),
            json!({"type":"object"}),
            vec![],
            target.into(),
        );
        module
            .metadata
            .insert("command_path".into(), json!(["cat"]));
        module.metadata.insert("variant".into(), json!("unknown"));
        module.metadata.insert(
            "shim_contract".into(),
            json!({"authoritative":true,
            "variant":"unknown","streaming_stdin_safe":true,"short_flag_bundling":true,
            "cross_flag_ordering":false}),
        );
        apexe::output::YamlOutput::without_verification()
            .write(&[module], &self.home.path().join(".apexe/bindings"), false)
            .unwrap();
    }

    fn write_acl(&self, contents: &str) {
        std::fs::create_dir_all(self.home.path().join(".apexe")).unwrap();
        std::fs::write(self.home.path().join(".apexe/acl.yaml"), contents).unwrap();
    }

    fn change_manifest(&self, change: impl FnOnce(&mut Value)) {
        let path = self.directory().join(".apexe-shims.json");
        let mut value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        change(&mut value);
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn change_binding(&self, change: impl FnOnce(&mut ScannedModule)) {
        let directory = self.home.path().join(".apexe/bindings");
        let mut modules = apexe::output::load_modules_from_dir(&directory, None).unwrap();
        change(&mut modules[0]);
        apexe::output::YamlOutput::without_verification()
            .write(&modules, &directory, false)
            .unwrap();
    }
}

#[test]
fn test_env_check_passes_without_executing_or_modifying_installation() {
    let fixture = Fixture::new();
    let manifest = std::fs::read(fixture.directory().join(".apexe-shims.json")).unwrap();
    let startup = std::fs::read(fixture.home.path().join(".profile")).unwrap();
    fixture
        .check()
        .assert()
        .success()
        .stdout(contains("[PASS] PATH"))
        .stdout(contains("[PASS] MANIFEST"))
        .stdout(contains("[PASS] LINKS"))
        .stdout(contains("[PASS] BINDINGS"))
        .stdout(contains("[PASS] ACL"))
        .stdout(contains("0 failed"))
        .stdout(contains("aliases/functions"));
    assert_eq!(
        manifest,
        std::fs::read(fixture.directory().join(".apexe-shims.json")).unwrap()
    );
    assert_eq!(
        startup,
        std::fs::read(fixture.home.path().join(".profile")).unwrap()
    );
    assert!(!fixture.home.path().join(".apexe/audit.jsonl").exists());
}

#[test]
fn test_env_check_reports_inactive_path() {
    let fixture = Fixture::new();
    fixture
        .command()
        .args(["env", "--check"])
        .assert()
        .code(1)
        .stdout(contains("[FAIL] PATH"))
        .stdout(contains("apexe env"));
}

#[test]
fn test_env_check_reports_shadowed_command() {
    let fixture = Fixture::new();
    fixture
        .check()
        .env(
            "PATH",
            std::env::join_paths([PathBuf::from("/bin"), fixture.directory()]).unwrap(),
        )
        .assert()
        .code(1)
        .stdout(contains("[FAIL] PATH cat"))
        .stdout(contains("/bin/cat"));
}

#[test]
fn test_env_check_accepts_symlinked_path_directory() {
    let fixture = Fixture::new();
    let alias = fixture.home.path().join("alias");
    std::os::unix::fs::symlink(fixture.directory(), &alias).unwrap();
    fixture.check().env("PATH", alias).assert().success();
}

#[test]
fn test_env_check_warns_on_duplicate_path_entries() {
    let fixture = Fixture::new();
    fixture
        .check()
        .env(
            "PATH",
            std::env::join_paths([fixture.directory(), fixture.directory()]).unwrap(),
        )
        .assert()
        .success()
        .stdout(contains("[WARN] PATH"));
}

#[test]
fn test_env_check_reports_missing_or_corrupt_manifest() {
    for contents in [None, Some("not json"), Some("{\"version\":999}")] {
        let fixture = Fixture::new();
        let path = fixture.directory().join(".apexe-shims.json");
        if let Some(contents) = contents {
            std::fs::write(path, contents).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] MANIFEST"))
            .stdout(contains("[SKIP] BINDINGS"))
            .stdout(contains("[SKIP] ACL"));
    }
}

#[test]
fn test_env_check_rejects_invalid_manifest_structure() {
    for field in [
        "version",
        "commands",
        "routes",
        "name",
        "path",
        "executable",
        "bindings_dir",
        "acl",
    ] {
        let fixture = Fixture::new();
        fixture.change_manifest(|manifest| match field {
            "version" => manifest["version"] = json!(999),
            "commands" => manifest["commands"] = json!({}),
            "routes" => manifest["commands"]["cat"] = json!([]),
            "name" => manifest["commands"] = json!({"../escape":manifest["commands"]["cat"]}),
            "path" => manifest["commands"]["cat"][0]["command_path"] = json!(["wrong"]),
            "executable" | "bindings_dir" | "acl" => manifest[field] = json!("relative"),
            _ => unreachable!(),
        });
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] MANIFEST"));
    }
}

#[test]
fn test_env_check_reports_missing_replaced_or_dangling_links() {
    for replacement in ["missing", "file", "dangling", "wrong"] {
        let fixture = Fixture::new();
        let path = fixture.directory().join("cat");
        std::fs::remove_file(&path).unwrap();
        match replacement {
            "file" => std::fs::write(&path, "unowned").unwrap(),
            "dangling" => std::os::unix::fs::symlink("/missing-apexe", &path).unwrap(),
            "wrong" => std::os::unix::fs::symlink("/bin/cat", &path).unwrap(),
            _ => {}
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] LINKS cat"));
    }
}

#[test]
fn test_env_check_reports_unavailable_apexe_executable() {
    let fixture = Fixture::new();
    fixture.change_manifest(|manifest| manifest["executable"] = json!("/missing-apexe"));
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("[FAIL] LINKS executable"));
}

#[test]
fn test_env_check_rejects_links_to_a_different_executable() {
    let fixture = Fixture::new();
    fixture.change_manifest(|manifest| manifest["executable"] = json!("/bin/cat"));
    std::fs::remove_file(fixture.directory().join("cat")).unwrap();
    std::os::unix::fs::symlink("/bin/cat", fixture.directory().join("cat")).unwrap();
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("[FAIL] LINKS running executable"));
}

#[test]
fn test_env_check_reports_missing_corrupt_or_empty_bindings() {
    for replacement in ["missing", "corrupt", "empty"] {
        let fixture = Fixture::new();
        let path = fixture
            .home
            .path()
            .join(".apexe/bindings/cli.cat.binding.yaml");
        match replacement {
            "corrupt" => std::fs::write(path, "bindings: [").unwrap(),
            _ => std::fs::remove_file(path).unwrap(),
        }
        if replacement == "missing" {
            fixture
                .change_manifest(|manifest| manifest["bindings_dir"] = json!("/missing-bindings"));
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] BINDINGS"))
            .stdout(contains("[PASS] ACL"));
    }
}

#[test]
fn test_env_check_reports_stale_route_and_unavailable_native_binary() {
    for target in ["exec:///bin/cat", "exec:///missing-cat"] {
        let fixture = Fixture::new();
        fixture.write_binding(target);
        if target == "exec:///bin/cat" {
            fixture.change_manifest(|manifest| {
                manifest["commands"]["cat"][0]["module_id"] = json!("cli.missing")
            });
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] BINDINGS"));
    }
}

#[test]
fn test_env_check_reports_missing_or_malformed_acl() {
    for contents in [
        None,
        Some("rules: ["),
        Some("rules: [{callers: [], targets: ['*'], effect: allow}]"),
    ] {
        let fixture = Fixture::new();
        if let Some(contents) = contents {
            fixture.write_acl(contents);
        } else {
            std::fs::remove_file(fixture.home.path().join(".apexe/acl.yaml")).unwrap();
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] ACL"));
    }
}

#[test]
fn test_env_check_uses_installed_bindings_pattern_and_explicit_acl() {
    let fixture = Fixture::new();
    let bindings = fixture.home.path().join("custom-bindings");
    std::fs::create_dir(&bindings).unwrap();
    std::fs::rename(
        fixture
            .home
            .path()
            .join(".apexe/bindings/cli.cat.binding.yaml"),
        bindings.join("cat.yaml"),
    )
    .unwrap();
    let acl = fixture.home.path().join("custom-acl.yaml");
    std::fs::rename(fixture.home.path().join(".apexe/acl.yaml"), &acl).unwrap();
    fixture.change_manifest(|manifest| {
        manifest["bindings_dir"] = json!(bindings);
        manifest["bindings_pattern"] = json!("*.yaml");
        manifest["acl"] = json!(acl);
    });
    fixture
        .check()
        .env("APEXE_BINDINGS_DIR", "/ignored-bindings")
        .assert()
        .success();
    std::fs::remove_file(acl).unwrap();
    fixture.write_acl("rules: []\ndefault_effect: allow\n");
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("custom-acl.yaml"));
}

#[test]
fn test_env_check_supports_custom_shim_directory_and_environment() {
    let fixture = Fixture::new();
    let directory = fixture.home.path().join("custom-shims");
    fixture
        .command()
        .args(["env", "--disable"])
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--enable", "--shim-dir"])
        .arg(&directory)
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--check"])
        .env("PATH", &directory)
        .env("APEXE_SHIM_DIR", &directory)
        .assert()
        .success();
    fixture
        .command()
        .args(["env", "--check", "--shim-dir"])
        .arg(&directory)
        .env("PATH", &directory)
        .assert()
        .success();
}

#[test]
fn test_env_check_rejects_mutating_or_ignored_options() {
    let fixture = Fixture::new();
    for arguments in [
        vec!["--enable"],
        vec!["--reload"],
        vec!["--disable"],
        vec!["--acl", "policy.yaml"],
        vec!["--bindings-dir", "bindings"],
        vec!["--prefix", "cli."],
        vec!["--tags", "pilot"],
        vec!["--rc-file", "startup"],
    ] {
        fixture.check().args(arguments).assert().code(2);
    }
}

#[test]
fn test_env_check_missing_installation_does_not_create_directories() {
    let home = TempDir::new().unwrap();
    Command::cargo_bin("apexe")
        .unwrap()
        .env("HOME", home.path())
        .env_remove("APEXE_SHIM_DIR")
        .env_remove("APEXE_BINDINGS_DIR")
        .env_remove("APCORE_CONFIG")
        .args(["env", "--check"])
        .assert()
        .code(1)
        .stdout(contains("[FAIL] MANIFEST"));
    assert!(!home.path().join(".apexe").exists());
}

#[test]
fn test_env_check_reports_path_environment_mismatch() {
    let fixture = Fixture::new();
    fixture
        .check()
        .args(["--shim-dir"])
        .arg(fixture.directory())
        .env("APEXE_SHIM_DIR", "/different-shims")
        .assert()
        .code(1)
        .stdout(contains("[FAIL] PATH APEXE_SHIM_DIR"));
}

#[test]
fn test_env_check_reports_non_executable_native_binary_without_running_it() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let native = fixture.home.path().join("native");
    let marker = fixture.home.path().join("executed");
    std::fs::write(
        &native,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    fixture.write_binding(&format!("exec://{}", native.display()));
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("not executable"));
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture.check().assert().success();
    assert!(!marker.exists());
}

#[test]
fn test_env_check_reports_changed_contract_route_and_filter() {
    for change in [
        "contract",
        "command_path",
        "filter",
        "registration",
        "duplicate",
    ] {
        let fixture = Fixture::new();
        match change {
            "contract" => fixture.change_binding(|module| {
                module.metadata.get_mut("shim_contract").unwrap()["authoritative"] = json!(false);
            }),
            "command_path" => fixture.change_binding(|module| {
                module
                    .metadata
                    .insert("command_path".into(), json!(["other"]));
            }),
            "filter" => {
                fixture.change_manifest(|manifest| manifest["filter"]["prefix"] = json!("other."))
            }
            "registration" => {
                fixture.change_binding(|module| module.module_id = "invalid-id".into());
                fixture.change_manifest(|manifest| {
                    manifest["commands"]["cat"][0]["module_id"] = json!("invalid-id")
                });
            }
            "duplicate" => std::fs::copy(
                fixture
                    .home
                    .path()
                    .join(".apexe/bindings/cli.cat.binding.yaml"),
                fixture
                    .home
                    .path()
                    .join(".apexe/bindings/duplicate.binding.yaml"),
            )
            .map(|_| ())
            .unwrap(),
            _ => unreachable!(),
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] BINDINGS"));
    }
}

#[test]
fn test_env_check_rejects_native_target_that_resolves_to_shim_executable() {
    let fixture = Fixture::new();
    let native = fixture.home.path().join("native");
    std::os::unix::fs::symlink(fixture.directory().join("cat"), &native).unwrap();
    fixture.write_binding(&format!("exec://{}", native.display()));
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("Binding targets the shim executable"));
}

#[test]
fn test_env_check_validates_acl_targets_against_registered_modules() {
    let fixture = Fixture::new();
    fixture.write_acl(
        "rules: [{callers: ['*'], targets: ['cli.other'], effect: deny}]\ndefault_effect: deny\n",
    );
    fixture
        .check()
        .assert()
        .success()
        .stdout(contains("[WARN] ACL targets"));
    fixture.change_binding(|module| module.module_id = "cli.cat_file".into());
    fixture.change_manifest(|manifest| {
        manifest["commands"]["cat"][0]["module_id"] = json!("cli.cat_file")
    });
    fixture.write_acl("rules: [{callers: ['*'], targets: ['cli.cat-file'], effect: deny}]\ndefault_effect: deny\n");
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("[FAIL] ACL targets"));
}

#[test]
fn test_env_check_reports_all_independent_failures() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.directory().join("cat")).unwrap();
    std::fs::remove_file(
        fixture
            .home
            .path()
            .join(".apexe/bindings/cli.cat.binding.yaml"),
    )
    .unwrap();
    fixture.write_acl("rules: [");
    fixture
        .check()
        .env_remove("PATH")
        .assert()
        .code(1)
        .stdout(contains("[FAIL] PATH"))
        .stdout(contains("[FAIL] LINKS cat"))
        .stdout(contains("[FAIL] BINDINGS cli.cat"))
        .stdout(contains("[FAIL] ACL"));
}

#[test]
fn test_env_check_rejects_oversized_manifest_and_duplicate_routes() {
    let fixture = Fixture::new();
    fixture.change_manifest(|manifest| {
        let route = manifest["commands"]["cat"][0].clone();
        manifest["commands"]["cat"]
            .as_array_mut()
            .unwrap()
            .push(route);
    });
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("Duplicate route"));
    std::fs::write(
        fixture.directory().join(".apexe-shims.json"),
        vec![b' '; 1024 * 1024 + 1],
    )
    .unwrap();
    fixture
        .check()
        .assert()
        .code(1)
        .stdout(contains("exceeds 1 MiB"));
}

#[test]
fn test_env_check_reports_missing_or_modified_activation_script() {
    for name in ["activate", "deactivate"] {
        for replacement in ["missing", "modified"] {
            let fixture = Fixture::new();
            let script = fixture.directory().join(name);
            if replacement == "missing" {
                std::fs::remove_file(script).unwrap();
            } else {
                std::fs::write(script, "modified").unwrap();
            }
            fixture
                .check()
                .assert()
                .code(1)
                .stdout(contains("[FAIL] LINKS activation"));
        }
    }
}

#[test]
fn test_env_check_warns_about_legacy_scripts_without_creating_them() {
    let fixture = Fixture::new();
    fixture.change_manifest(|manifest| {
        manifest
            .as_object_mut()
            .unwrap()
            .remove("activation_script");
        manifest
            .as_object_mut()
            .unwrap()
            .remove("deactivation_script");
    });
    for name in ["activate", "deactivate"] {
        std::fs::remove_file(fixture.directory().join(name)).unwrap();
    }
    fixture
        .check()
        .assert()
        .success()
        .stdout(contains("[WARN] LINKS activation activate"))
        .stdout(contains("[WARN] LINKS activation deactivate"));
    assert!(!fixture.directory().join("activate").exists());
}

#[test]
fn test_env_check_rejects_reserved_persistent_helper_routes() {
    for name in [
        "apexe-activate.sh",
        "apexe-deactivate.sh",
        ".apexe-source-scripts.json",
    ] {
        let fixture = Fixture::new();
        fixture.change_manifest(|manifest| {
            manifest["commands"][name] = serde_json::json!([{
                "module_id": "cli.cat",
                "command_path": [name]
            }]);
        });
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] MANIFEST"))
            .stdout(contains("Invalid command name"));
    }
}

#[test]
fn test_env_check_reports_missing_modified_and_corrupt_persistent_scripts() {
    for change in ["missing", "modified", "record"] {
        let fixture = Fixture::new();
        let directory = fixture.home.path().join(".apexe/bin");
        match change {
            "missing" => std::fs::remove_file(directory.join("apexe-activate.sh")).unwrap(),
            "modified" => {
                std::fs::write(directory.join("apexe-deactivate.sh"), "modified").unwrap()
            }
            "record" => {
                std::fs::write(directory.join(".apexe-source-scripts.json"), "corrupt").unwrap()
            }
            _ => unreachable!(),
        }
        fixture
            .check()
            .assert()
            .code(1)
            .stdout(contains("[FAIL] LINKS persistent source scripts"));
    }
}
