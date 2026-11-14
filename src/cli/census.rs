//! The PATH census: three states a command on this machine can be in.
//!
//! `list` answers "what can run here?" from the bindings. It cannot answer the
//! question that follows — *why* a command the operator just installed does
//! nothing — because that depends on state the bindings do not hold: whether the
//! executable is on `PATH` at all, and whether the policy grants it to the
//! caller. Reporting only the scanned surface makes "never scanned", "scanned but
//! denied" and "usable" indistinguishable, and an operator meets the gap one
//! refused call at a time.
//!
//! Nothing here executes a command or scans one: the census reads directory
//! entries and the decisions already computed for `list --verbose`. Turning an
//! `unscanned` entry into a usable route is `apexe scan <tool>`, which is the
//! operator's call — a machine has thousands of executables and probing all of
//! them is neither cheap nor side-effect free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apcore_toolkit::ScannedModule;

use crate::governance::AclManager;

/// What the census found, grouped by the decision an operator has to make next.
#[derive(Debug, Default)]
pub(super) struct Census {
    /// Executables reachable through `PATH`, deduplicated by name.
    pub path_executables: usize,
    /// Has a binding the caller's policy allows.
    pub granted: Vec<String>,
    /// Has a binding the caller's policy refuses.
    pub denied: Vec<String>,
    /// No binding at all — `apexe scan <tool>` is the missing step.
    pub unscanned: Vec<String>,
}

/// Directories to inspect after removing the ones a census must not count.
///
/// The shim directory is excluded or every installed command would appear to
/// have a second, fake installation whose entries are apexe itself. Empty
/// entries (a `PATH` beginning or ending with `:`, which means "the current
/// directory") are dropped rather than resolved: the census describes the
/// machine, not the caller's working directory.
pub(super) fn census_directories(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path)
        .filter(|entry| !entry.as_os_str().is_empty())
        .filter(|entry| entry.is_dir())
        .filter(|entry| !crate::runtime_path::is_shim_directory(entry))
        .collect()
}

/// Every executable reachable in `directories`, name → the path `PATH` resolves.
///
/// First entry wins, exactly as a shell resolves the name, so the census and a
/// governed call agree about which binary a name means.
pub(super) fn path_executables(directories: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == "apexe" || found.contains_key(name) || !is_executable_file(&path) {
                continue;
            }
            found.insert(name.to_string(), path);
        }
    }
    found
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The command names a binding makes runnable.
///
/// Prefers `metadata.command_path[0]`, which is the name the operator typed and
/// the one a shim installs under. A binding without it falls back to the target
/// binary's own file name, so an older binding is still counted rather than
/// silently reporting its tool as unscanned.
pub(super) fn bound_tool_names(modules: &[ScannedModule]) -> BTreeMap<String, Vec<String>> {
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for module in modules {
        let name = module
            .metadata
            .get("command_path")
            .and_then(|value| value.as_array())
            .and_then(|path| path.first())
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                module
                    .target
                    .strip_prefix("exec://")
                    .and_then(|target| target.split_whitespace().next())
                    .and_then(|binary| Path::new(binary).file_name())
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            });
        if let Some(name) = name {
            by_name
                .entry(name)
                .or_default()
                .push(module.module_id.clone());
        }
    }
    by_name
}

/// Build the census from what is installed, what is bound, and what the policy
/// grants.
///
/// A tool is `granted` when *any* of its routes is allowed, because that is the
/// question the operator is asking ("can I run this command?"); a tool whose
/// every route is refused is `denied`, and `apexe acl explain <module-id>` names
/// the rule responsible.
pub(super) fn collect(
    directories: &[PathBuf],
    modules: &[ScannedModule],
    acl: Option<&AclManager>,
    caller: &str,
) -> Census {
    let executables = path_executables(directories);
    let bound = bound_tool_names(modules);
    let mut census = Census {
        path_executables: executables.len(),
        ..Default::default()
    };
    for name in executables.keys() {
        match bound.get(name) {
            None => census.unscanned.push(name.clone()),
            Some(module_ids) => {
                let allowed = acl.is_none_or(|acl| {
                    module_ids
                        .iter()
                        .any(|module_id| acl.explain_for(caller, module_id).effect == "allow")
                });
                if allowed {
                    census.granted.push(name.clone());
                } else {
                    census.denied.push(name.clone());
                }
            }
        }
    }
    for name in bound.keys().filter(|name| !executables.contains_key(*name)) {
        // Bound but uninstalled: neither runnable nor a scanning candidate here.
        // `list --available-only` reports the same condition per module.
        census.denied.push(name.clone());
    }
    census.denied.sort();
    census.denied.dedup();
    census
}

pub(super) fn print(census: &Census, format: &str) -> anyhow::Result<()> {
    if format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path_executables": census.path_executables,
                "granted": census.granted,
                "denied": census.denied,
                "unscanned": census.unscanned,
            }))?
        );
        return Ok(());
    }
    println!(
        "PATH census: {} executable(s) on PATH, {} bound",
        census.path_executables,
        census.granted.len() + census.denied.len()
    );
    println!(
        "  granted   ({:>4}): {}",
        census.granted.len(),
        join(&census.granted)
    );
    println!(
        "  denied    ({:>4}): {}",
        census.denied.len(),
        join(&census.denied)
    );
    println!(
        "  unscanned ({:>4}): {}",
        census.unscanned.len(),
        join(&census.unscanned)
    );
    println!();
    println!("`granted` and `denied` name bound tools; `denied` is a policy decision, not a");
    println!("scan gap. `unscanned` is the scan gap: `apexe scan <tool>` covers one at a time.");
    Ok(())
}

fn join(names: &[String]) -> String {
    const SHOWN: usize = 24;
    if names.is_empty() {
        return "-".to_string();
    }
    let mut shown = names
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        shown.push_str(&format!(", … (+{})", names.len() - SHOWN));
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_module(id: &str, tool: &str) -> ScannedModule {
        let mut module = ScannedModule::new(
            id.to_string(),
            format!("{tool} route"),
            serde_json::json!({"type": "object"}),
            serde_json::json!({"type": "object"}),
            vec!["cli".to_string()],
            format!("exec:///usr/bin/{tool}"),
        );
        module
            .metadata
            .insert("command_path".to_string(), serde_json::json!([tool]));
        module
    }

    fn executable(dir: &Path, name: &str) {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn test_census_separates_unscanned_from_denied() {
        let root = tempfile::TempDir::new().unwrap();
        executable(root.path(), "bound_tool");
        executable(root.path(), "never_scanned");
        // A non-executable file is not a command the caller can run.
        std::fs::write(root.path().join("README"), "notes").unwrap();

        let acl = crate::governance::AclManager::generate_starter(
            &[make_module("cli.bound_tool", "bound_tool")],
            crate::governance::PolicyProfile::Developer,
        );
        let census = collect(
            &[root.path().to_path_buf()],
            &[make_module("cli.bound_tool", "bound_tool")],
            Some(&acl),
            "local",
        );

        assert_eq!(census.path_executables, 2, "README is not executable");
        assert_eq!(census.granted, vec!["bound_tool".to_string()]);
        assert_eq!(census.unscanned, vec!["never_scanned".to_string()]);
        assert!(
            census.denied.is_empty(),
            "a developer-profile local grant covers the bound tool: {:?}",
            census.denied
        );
    }

    #[test]
    fn test_census_reports_a_bound_but_denied_tool() {
        let root = tempfile::TempDir::new().unwrap();
        executable(root.path(), "rm_like");
        let acl = crate::governance::AclManager::generate_starter(
            &[make_module("cli.rm_like", "rm_like")],
            crate::governance::PolicyProfile::Strict,
        );

        let census = collect(
            &[root.path().to_path_buf()],
            &[make_module("cli.rm_like", "rm_like")],
            Some(&acl),
            "local",
        );

        assert_eq!(census.denied, vec!["rm_like".to_string()]);
        assert!(census.granted.is_empty());
        assert!(census.unscanned.is_empty());
    }

    #[test]
    fn test_census_counts_a_bound_tool_that_is_no_longer_installed() {
        let root = tempfile::TempDir::new().unwrap();
        let acl = crate::governance::AclManager::generate_starter(
            &[make_module("cli.gone", "gone")],
            crate::governance::PolicyProfile::Developer,
        );

        let census = collect(
            &[root.path().to_path_buf()],
            &[make_module("cli.gone", "gone")],
            Some(&acl),
            "local",
        );

        assert_eq!(census.path_executables, 0);
        assert_eq!(
            census.denied,
            vec!["gone".to_string()],
            "a binding whose binary is gone is not runnable, and it is not unscanned either"
        );
    }

    #[test]
    fn test_census_drops_the_shim_directory() {
        let path = std::env::join_paths([Path::new("/nonexistent-a"), Path::new("/nonexistent-b")])
            .unwrap();
        assert!(
            census_directories(&path).is_empty(),
            "directories that do not exist are not part of this machine's census"
        );
    }
}
