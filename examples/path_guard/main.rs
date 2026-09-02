//! Path guard demo — what a wrapped tool may be pointed at.
//!
//! The companion to [`acl_demo`](../acl_demo/), and deliberately a separate
//! example because the two answer different questions. An ACL decides *who may
//! call which module*, from the caller's roles. The path guard decides *what
//! one call may touch*, from the argument values. Neither substitutes for the
//! other, and the guard is the one that is on by default.
//!
//! Run it:
//!
//! ```bash
//! cargo run --example path_guard
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use apcore::module::ModuleAnnotations;
use apcore::Executor;
use apcore_toolkit::ScannedModule;
use apexe::governance::{GuardConfig, PathGuard};
use apexe::module::{build_executor, ExecutorOptions};
use apexe::output::YamlOutput;
use serde_json::json;
use tempfile::TempDir;

/// Build a module with one variadic path-typed operand, the shape a real scan
/// produces: `x-apexe-path` sits on `items`, because it is each element that
/// names a path rather than the list of them. `apexe scan rm` emits exactly
/// this for `rm`'s `file`.
///
/// `readonly` is what selects which of the guard's two lists binds the call —
/// the only difference between the two modules registered below.
fn path_taking_module(id: &str, description: &str, readonly: bool) -> ScannedModule {
    let mut module = ScannedModule::new(
        id.to_string(),
        description.to_string(),
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "array",
                    "items": { "type": "string", "x-apexe-path": true },
                    "x-apexe-positional": 0
                }
            },
            "additionalProperties": false
        }),
        json!({ "type": "object" }),
        vec!["demo".to_string()],
        // Both wrap `echo`: this example is about which paths are admitted,
        // not about the command. A real binding points at the actual binary.
        "exec:///bin/echo".to_string(),
    );
    module.annotations = Some(ModuleAnnotations {
        readonly,
        destructive: !readonly,
        requires_approval: !readonly,
        ..Default::default()
    });
    module
}

/// Register a reader and a writer through the same pipeline `apexe scan` uses:
/// build a `ScannedModule`, write it as a `.binding.yaml`, load it back.
///
/// No ACL is attached. Everything refused below is refused by the path guard
/// alone, which is the point — it needs no `--acl` and no `--enable-approval`.
fn build_demo_executor(modules_dir: &Path) -> Arc<Executor> {
    let modules = [
        path_taking_module("demo.read", "Read a file (readonly)", true),
        path_taking_module("demo.write", "Modify a file (writer)", false),
    ];
    YamlOutput::new()
        .write(&modules, modules_dir, false)
        .expect("failed to write demo binding files");

    build_executor(&ExecutorOptions {
        modules_dir: Some(modules_dir),
        timeout_ms: 5_000,
        acl_path: None,
        filter: apexe::module::ModuleFilter::default(),
        audit_path: None,
        enable_logging: false,
        log_arguments: false,
        enable_approval: false,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
    })
    .expect("failed to build executor")
}

/// One probe: a module, a path, and what the reader should expect to see.
struct Probe {
    module: &'static str,
    path: String,
    note: &'static str,
}

fn probes(home: &Path, workspace: &Path) -> Vec<Probe> {
    let p = |module, path: String, note| Probe { module, path, note };
    vec![
        p(
            "demo.read",
            "/etc/hosts".to_string(),
            "system paths stay legible to a reader",
        ),
        p(
            "demo.write",
            "/etc/hosts".to_string(),
            "the same path, from a module that can modify it",
        ),
        p(
            "demo.read",
            home.join(".ssh/id_rsa").display().to_string(),
            "credentials bind readers too — exfiltration leaves no trace",
        ),
        p(
            "demo.write",
            "/".to_string(),
            "a writer may not target a directory that CONTAINS a protected one",
        ),
        p(
            "demo.read",
            "/".to_string(),
            "a reader may list it — `ls /` is not `rm -rf /`",
        ),
        p(
            "demo.write",
            workspace
                .join("../../../../../../etc/passwd")
                .display()
                .to_string(),
            "resolved before comparison, so a climb cannot hide the target",
        ),
        p(
            "demo.write",
            "/etcetera/notes".to_string(),
            "matching is per path component: /etcetera is not /etc",
        ),
        p(
            "demo.write",
            workspace.join("build/out.txt").display().to_string(),
            "ordinary workspace paths are untouched",
        ),
    ]
}

async fn run_probes(executor: &Executor, probes: &[Probe]) {
    for probe in probes {
        let outcome = executor
            .call(
                probe.module,
                json!({ "file": [probe.path.clone()] }),
                None,
                None,
            )
            .await;
        let verdict = match &outcome {
            Ok(_) => "ALLOWED".to_string(),
            Err(e) => format!("REFUSED ({:?})", e.code),
        };
        println!(
            "  {:<11} {:<46} {verdict}",
            probe.module,
            elide(&probe.path)
        );
        println!("  {:11} └─ {}", "", probe.note);
    }
}

/// Keep the table readable when a temp-dir path runs long.
fn elide(path: &str) -> String {
    const MAX: usize = 44;
    if path.chars().count() <= MAX {
        return path.to_string();
    }
    let tail: String = path
        .chars()
        .skip(path.chars().count() - (MAX - 1))
        .collect();
    format!("…{tail}")
}

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let modules_dir = TempDir::new().expect("failed to create temp modules dir");
    let workspace = TempDir::new().expect("failed to create temp workspace");
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/user"));
    let executor = build_demo_executor(modules_dir.path());

    println!("=== The compiled-in baselines (no configuration) ===\n");
    run_probes(&executor, &probes(&home, workspace.path())).await;

    println!("\n=== Carve-outs: `allowed_paths` in config.yaml ===\n");
    demonstrate_carve_out();

    println!(
        "\nThe guard needs no flag and has no off switch. See docs/user-manual.md \
         §9.7 and docs/threat-model.md §4.8."
    );
}

/// The one setting that relaxes the guard, shown against the same paths.
///
/// Built directly rather than through the `Executor`, because the interesting
/// part is the policy decision and a second registry would only add noise. This
/// is the same [`PathGuard`] the executor consults.
fn demonstrate_carve_out() {
    let allowed = [PathBuf::from("/etc/nginx/conf.d")];
    let denied = [PathBuf::from("/etc/nginx/conf.d/secrets")];

    let baseline = PathGuard::new(PathBuf::from("/"), GuardConfig::default());
    let configured = PathGuard::new(
        PathBuf::from("/"),
        GuardConfig {
            denied: &denied,
            allowed: &allowed,
        },
    );

    println!("  allowed_paths:            [/etc/nginx/conf.d]");
    println!("  additional_denied_paths:  [/etc/nginx/conf.d/secrets]\n");
    println!(
        "  {:<40} {:>10} {:>12}",
        "path (as a writer)", "baseline", "configured"
    );
    for path in [
        "/etc/nginx/conf.d/site.conf",
        "/etc/nginx/conf.d/secrets/key",
        "/etc/nginx/nginx.conf",
        "/etc/passwd",
    ] {
        println!(
            "  {:<40} {:>10} {:>12}",
            path,
            verdict(&baseline, path),
            verdict(&configured, path)
        );
    }
    println!("\n  A carve-out grants everything beneath it, and a more specific");
    println!("  denial still wins. Nothing validates that a carve-out is wise —");
    println!("  every one is logged at startup, and the risky ones at `warn`.");
}

fn verdict(guard: &PathGuard, path: &str) -> &'static str {
    match guard.check("path", path, apexe::governance::AccessMode::Write) {
        Ok(()) => "ALLOWED",
        Err(_) => "REFUSED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four-quadrant contract the example prints, as an assertion.
    #[tokio::test]
    async fn test_path_guard_demo_contract() {
        let modules_dir = TempDir::new().unwrap();
        let executor = build_demo_executor(modules_dir.path());
        let home = dirs::home_dir().expect("home directory");

        let call = |module: &'static str, path: String| {
            let executor = executor.clone();
            async move {
                executor
                    .call(module, json!({ "file": [path] }), None, None)
                    .await
            }
        };

        // A reader may name a system path; a writer may not.
        assert!(call("demo.read", "/etc/hosts".into()).await.is_ok());
        assert!(call("demo.write", "/etc/hosts".into()).await.is_err());

        // Credentials bind both.
        let key = home.join(".ssh/id_rsa").display().to_string();
        assert!(call("demo.read", key.clone()).await.is_err());
        assert!(call("demo.write", key).await.is_err());

        // Ancestry binds writers only.
        assert!(call("demo.read", "/".into()).await.is_ok());
        assert!(call("demo.write", "/".into()).await.is_err());

        // Component-wise matching, not string prefixes.
        assert!(call("demo.write", "/etcetera/notes".into()).await.is_ok());
    }

    #[test]
    fn test_carve_out_reopens_only_its_own_subtree() {
        let allowed = [PathBuf::from("/etc/nginx/conf.d")];
        let denied = [PathBuf::from("/etc/nginx/conf.d/secrets")];
        let guard = PathGuard::new(
            PathBuf::from("/"),
            GuardConfig {
                denied: &denied,
                allowed: &allowed,
            },
        );

        assert_eq!(verdict(&guard, "/etc/nginx/conf.d/site.conf"), "ALLOWED");
        assert_eq!(verdict(&guard, "/etc/nginx/conf.d/secrets/key"), "REFUSED");
        assert_eq!(verdict(&guard, "/etc/nginx/nginx.conf"), "REFUSED");
        assert_eq!(verdict(&guard, "/etc/passwd"), "REFUSED");
    }
}
