//! ACL demo — enforce apcore Access Control Lists on apexe's CLI-wrapped
//! modules.
//!
//! The same `orders.delete` (admins only) / `orders.list` (public read)
//! contract used across the apcore framework integrations (see
//! `axum-apcore/examples/acl_demo`), applied here to apexe's `CliModule` +
//! `Executor` + [`AclManager`](apexe::governance::AclManager) — the exact
//! building blocks `apexe serve --acl` / `apexe a2a --acl` use under the
//! hood.
//!
//! Run it:
//!
//! ```bash
//! cargo run --example acl_demo
//! ```

use std::path::Path;
use std::sync::Arc;

use apcore::module::ModuleAnnotations;
use apcore::{Context, Executor, Identity};
use apcore_toolkit::ScannedModule;
use apexe::module::{build_executor, ExecutorOptions};
use apexe::output::YamlOutput;
use serde_json::json;
use tempfile::TempDir;

/// Register the two ACL-protected `orders.*` modules as apexe `CliModule`s
/// and load `acl.yaml` on top -- the same "scan → write bindings → serve
/// --acl" pipeline `apexe` runs, minus the actual CLI scan step (these two
/// modules are hand-built here instead of coming from `apexe scan`).
///
/// Both wrap a harmless `echo` invocation: this demo is about the ACL
/// contract, not the underlying command. A real integration would point
/// `target` at the actual delete/list binary (as `apexe scan` does).
fn build_demo_executor(modules_dir: &Path) -> Arc<Executor> {
    let mut delete_module = ScannedModule::new(
        "orders.delete".to_string(),
        "Delete an order (admins only)".to_string(),
        json!({
            "type": "object",
            "properties": { "order_id": { "type": "integer" } },
            "required": ["order_id"]
        }),
        json!({ "type": "object" }),
        vec!["orders".to_string()],
        "exec:///bin/echo deleted".to_string(),
    );
    delete_module.annotations = Some(ModuleAnnotations {
        destructive: true,
        requires_approval: true,
        ..Default::default()
    });

    let mut list_module = ScannedModule::new(
        "orders.list".to_string(),
        "List orders (public read)".to_string(),
        json!({ "type": "object" }),
        json!({ "type": "object" }),
        vec!["orders".to_string()],
        "exec:///bin/echo listed".to_string(),
    );
    list_module.annotations = Some(ModuleAnnotations {
        readonly: true,
        ..Default::default()
    });

    YamlOutput::new()
        .write(&[delete_module, list_module], modules_dir, false)
        .expect("failed to write orders.* binding files");

    let acl_path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/acl_demo/acl.yaml"
    ));

    build_executor(&ExecutorOptions {
        modules_dir: Some(modules_dir),
        timeout_ms: 5_000,
        acl_path: Some(acl_path),
        filter: apexe::module::ModuleFilter::default(),
        audit_path: None,
        enable_logging: false,
        log_arguments: false,
        enable_approval: false,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
        relay_denial_reason: false,
    })
    .expect("failed to build executor")
}

fn ctx_with_roles(roles: Vec<String>) -> Context<serde_json::Value> {
    let anonymous = roles.is_empty();
    Context::new(Identity::new(
        if anonymous { "anonymous" } else { "u1" }.to_string(),
        if anonymous { "anonymous" } else { "user" }.to_string(),
        roles,
        Default::default(),
    ))
}

#[tokio::main]
async fn main() {
    // Honor RUST_LOG (as the README documents), falling back to "error"
    // when it isn't set rather than hardcoding the filter to that literal.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let modules_dir = TempDir::new().expect("failed to create temp modules dir");
    let executor = build_demo_executor(modules_dir.path());

    let admin = ctx_with_roles(vec!["admin".to_string()]);
    let user = ctx_with_roles(vec!["user".to_string()]);
    let anon = ctx_with_roles(vec![]);

    println!("=== orders.delete (admins only) ===");
    for (label, ctx) in [("admin", &admin), ("user", &user), ("anonymous", &anon)] {
        let result = executor
            .call("orders.delete", json!({ "order_id": 1 }), Some(ctx), None)
            .await;
        match result {
            Ok(_) => println!("  {label:<9} -> ALLOWED"),
            Err(e) => println!("  {label:<9} -> DENIED ({:?})", e.code),
        }
    }

    println!("\n=== orders.list (public read) ===");
    for (label, ctx) in [("admin", &admin), ("user", &user), ("anonymous", &anon)] {
        let result = executor
            .call("orders.list", json!({}), Some(ctx), None)
            .await;
        match result {
            Ok(_) => println!("  {label:<9} -> ALLOWED"),
            Err(e) => println!("  {label:<9} -> DENIED ({:?})", e.code),
        }
    }

    println!("\nSee examples/acl_demo/README.md for the full contract.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_acl_demo_orders_contract() {
        let modules_dir = TempDir::new().unwrap();
        let executor = build_demo_executor(modules_dir.path());

        let admin = ctx_with_roles(vec!["admin".to_string()]);
        let user = ctx_with_roles(vec!["user".to_string()]);
        let anon = ctx_with_roles(vec![]);

        // admin may delete
        let ok = executor
            .call(
                "orders.delete",
                json!({ "order_id": 1 }),
                Some(&admin),
                None,
            )
            .await;
        assert!(ok.is_ok(), "admin delete should succeed: {ok:?}");

        // anonymous + non-admin denied
        assert!(executor
            .call("orders.delete", json!({ "order_id": 1 }), Some(&user), None)
            .await
            .is_err());
        assert!(executor
            .call("orders.delete", json!({ "order_id": 1 }), Some(&anon), None)
            .await
            .is_err());

        // anyone may read the public list
        assert!(executor
            .call("orders.list", json!({}), Some(&anon), None)
            .await
            .is_ok());
    }
}
