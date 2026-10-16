use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use apcore::{ErrorCode, ModuleError};
use apcore_toolkit::ScannedModule;
use apexe::module::{build_executor_with_openapi, ExecutorOptions, ModuleFilter};
use apexe::openapi::OpenApiOptions;
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use tempfile::TempDir;

/// Loopback fixtures must be reached directly. A workstation proxy can answer
/// for `127.0.0.1` instead of passing the request through, which turns a
/// fixture call into an empty `502`.
///
/// `.cargo/config.toml` supplies this to every process cargo runs -- the only
/// route that reaches a client built inside the test process -- and
/// `scripts/runtime_validation.py` sets it for the processes it starts. Stating
/// it at this call site as well keeps the requirement unconditional: a
/// developer whose own `NO_PROXY` covers something other than loopback leaves
/// the config value unapplied.
const LOOPBACK_NO_PROXY: &str = "127.0.0.1,localhost,::1";

struct TestApi {
    base_url: String,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestApi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_api() -> TestApi {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().route("/pets/{id}", get(move |axum::extract::Path(id): axum::extract::Path<String>, axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>, headers: axum::http::HeaderMap| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Json(json!({"id":id, "query":query.get("q"), "authorization":headers.get("authorization").and_then(|v| v.to_str().ok()), "spec_header":headers.get("x-spec-key").and_then(|v| v.to_str().ok())}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestApi {
        base_url,
        calls,
        task,
    }
}

fn document() -> Value {
    json!({"openapi":"3.1.0", "info":{"title":"Pets", "version":"1.0"}, "paths":{
        "/pets/{id}":{"get":{"operationId":"getPet", "tags":["pets"], "parameters":[
            {"name":"id", "in":"path", "required":true,"schema":{"type":"string"}},
            {"name":"q", "in":"query", "schema":{"type":"string"}}
        ], "responses":{"200":{"description":"Pet", "content":{"application/json":{"schema":{"type":"object"}}}}}}}
    }})
}

fn source(dir: &TempDir, document: &Value, base_url: &str) -> OpenApiOptions {
    let path = dir.path().join("openapi.json");
    std::fs::write(&path, document.to_string()).unwrap();
    OpenApiOptions {
        source: path.to_string_lossy().into(),
        base_url: Some(base_url.into()),
        prefix: "api".into(),
        spec_headers: HashMap::new(),
        bearer_token: None,
    }
}

fn options(dir: &TempDir) -> ExecutorOptions<'_> {
    ExecutorOptions {
        bindings_dir: Some(dir.path()),
        bindings_pattern: None,
        timeout_ms: 5_000,
        acl_path: None,
        filter: ModuleFilter::default(),
        audit_path: None,
        enable_logging: false,
        log_arguments: false,
        enable_approval: false,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
    }
}

#[tokio::test]
async fn test_openapi_import_executes_path_query_and_separate_api_token() {
    let server = start_api().await;
    let dir = TempDir::new().unwrap();
    let audit = dir.path().join("audit.jsonl");
    let mut opts = options(&dir);
    opts.audit_path = Some(&audit);
    let mut source = source(&dir, &document(), &server.base_url);
    source.bearer_token = Some("api-secret".into());
    source
        .spec_headers
        .insert("x-spec-key".into(), "spec-secret".into());
    let executor = build_executor_with_openapi(&opts, &source).await.unwrap();
    let result = executor
        .call(
            "api.get_pet",
            json!({"id":"a/b", "q":"hello world"}),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result["id"], "a/b");
    assert_eq!(result["query"], "hello world");
    assert_eq!(result["authorization"], "Bearer api-secret");
    assert_eq!(result["spec_header"], Value::Null);
    let audit = std::fs::read_to_string(audit).unwrap();
    assert!(audit.contains("api.get_pet"));
    assert!(!audit.contains("api-secret"));
    assert!(!audit.contains("hello world"));
}

#[tokio::test]
async fn test_openapi_schema_and_acl_rejections_make_no_http_request() {
    let server = start_api().await;
    let dir = TempDir::new().unwrap();
    let source = source(&dir, &document(), &server.base_url);
    let executor = build_executor_with_openapi(&options(&dir), &source)
        .await
        .unwrap();
    let error = executor
        .call("api.get_pet", json!({}), None, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SchemaValidationError);
    let acl = dir.path().join("acl.yaml");
    std::fs::write(&acl, "version: '1.0'\ndefault_effect: deny\nrules:\n  - callers: ['*']\n    targets: [api.get_pet]\n    effect: deny\n").unwrap();
    let mut opts = options(&dir);
    opts.acl_path = Some(&acl);
    let executor = build_executor_with_openapi(&opts, &source).await.unwrap();
    let error = executor
        .call("api.get_pet", json!({"id":"1"}), None, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ACLDenied);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_openapi_filtered_modules_are_uncallable() {
    let dir = TempDir::new().unwrap();
    let source = source(&dir, &document(), "http://127.0.0.1:1");
    let mut opts = options(&dir);
    opts.filter.tags = Some(vec!["missing".into()]);
    let executor = build_executor_with_openapi(&opts, &source).await.unwrap();
    assert_eq!(executor.registry().count(), 0);
    let error = executor
        .call("api.get_pet", json!({"id":"1"}), None, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ModuleNotFound);
}

#[tokio::test]
async fn test_openapi_import_preserves_cli_modules_and_refuses_collisions() {
    let dir = TempDir::new().unwrap();
    let module = ScannedModule::new(
        "cli.get_pet".into(),
        "Echo".into(),
        json!({"type":"object"}),
        json!({"type":"object"}),
        vec![],
        "exec:///bin/echo".into(),
    );
    apexe::output::YamlOutput::without_verification()
        .write(&[module], dir.path(), false)
        .unwrap();
    let mut source = source(&dir, &document(), "http://127.0.0.1:1");
    let executor = build_executor_with_openapi(&options(&dir), &source)
        .await
        .unwrap();
    assert!(executor.registry().has("cli.get_pet"));
    assert!(executor.registry().has("api.get_pet"));
    source.prefix = "cli".into();
    let error: ModuleError = build_executor_with_openapi(&options(&dir), &source)
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("collides"));
}

#[tokio::test]
async fn test_openapi_import_fails_on_missing_origin_and_unsupported_query_encoding() {
    let dir = TempDir::new().unwrap();
    let mut source = source(&dir, &document(), "http://127.0.0.1:1");
    source.base_url = None;
    let error = build_executor_with_openapi(&options(&dir), &source)
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("--openapi-base-url"));
    let mut document = document();
    document["paths"]["/pets/{id}"]["post"] = document["paths"]["/pets/{id}"]["get"].take();
    std::fs::write(&source.source, document.to_string()).unwrap();
    source.base_url = Some("http://127.0.0.1:1".into());
    let error = build_executor_with_openapi(&options(&dir), &source)
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("cannot encode"));
}

#[tokio::test]
async fn test_mcp_and_a2a_build_with_imported_openapi() {
    let dir = TempDir::new().unwrap();
    let source = source(&dir, &document(), "http://127.0.0.1:1");
    apexe::mcp::McpServerBuilder::new()
        .bindings_dir(dir.path())
        .build_with_openapi(&source)
        .await
        .unwrap();
    let (executor, config) = apexe::a2a::A2aServerBuilder::new()
        .bindings_dir(dir.path())
        .prepare_with_openapi(&source)
        .await
        .unwrap();
    let (_router, card) =
        apcore_a2a::build_app(apcore_a2a::BackendSource::Executor(executor), config)
            .await
            .unwrap();
    let card = serde_json::to_value(card).unwrap();
    assert!(card.to_string().contains("api.get_pet"), "{card}");
}

#[tokio::test]
async fn test_openapi_fetches_remote_yaml_with_spec_credentials_only() {
    let server = start_api().await;
    let yaml = serde_yaml::to_string(&document()).unwrap();
    let app = Router::new().route(
        "/spec.yaml",
        get(move |headers: axum::http::HeaderMap| {
            let yaml = yaml.clone();
            async move {
                assert_eq!(headers["x-spec-key"], "spec-secret");
                assert!(!headers.contains_key("authorization"));
                yaml
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/spec.yaml", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let spec_server = TestApi {
        base_url: url.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
        task,
    };
    let dir = TempDir::new().unwrap();
    let source = OpenApiOptions {
        source: spec_server.base_url.clone(),
        base_url: Some(server.base_url.clone()),
        prefix: "api".into(),
        spec_headers: HashMap::from([("x-spec-key".into(), "spec-secret".into())]),
        bearer_token: Some("api-secret".into()),
    };
    let executor = build_executor_with_openapi(&options(&dir), &source)
        .await
        .unwrap();
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    let result = executor
        .call("api.get_pet", json!({"id":"1"}), None, None)
        .await
        .unwrap();
    assert_eq!(result["authorization"], "Bearer api-secret");
    assert_eq!(result["spec_header"], Value::Null);
}

async fn send_rpc(stdin: &mut tokio::process::ChildStdin, request: Value) {
    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
}

async fn read_rpc(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
) -> Value {
    let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn test_cli_mcp_serves_and_calls_imported_openapi() {
    use tokio::io::AsyncBufReadExt;
    let server = start_api().await;
    let dir = TempDir::new().unwrap();
    let source = source(&dir, &document(), &server.base_url);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_apexe"))
        .env("HOME", dir.path())
        .env("NO_PROXY", LOOPBACK_NO_PROXY)
        .env("no_proxy", LOOPBACK_NO_PROXY)
        .env_remove("APEXE_BINDINGS_DIR")
        .current_dir(dir.path())
        .args([
            "serve",
            "--openapi",
            &source.source,
            "--openapi-base-url",
            &server.base_url,
            "--no-log-arguments",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    send_rpc(&mut stdin, json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"apexe-test","version":"1"}}})).await;
    assert!(read_rpc(&mut lines).await.get("result").is_some());
    send_rpc(
        &mut stdin,
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
    )
    .await;
    send_rpc(
        &mut stdin,
        json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
    )
    .await;
    let tools = read_rpc(&mut lines).await;
    let name = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|tool| {
            tool["name"]
                .as_str()
                .filter(|name| name.contains("get_pet"))
                .map(str::to_owned)
        })
        .unwrap();
    send_rpc(&mut stdin, json!({"jsonrpc":"2.0", "id":3,"method":"tools/call","params":{"name":name,"arguments":{"id":"1","q":"from-mcp"}}})).await;
    let result = read_rpc(&mut lines).await;
    assert_ne!(result["result"]["isError"], true, "{result}");
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    assert!(result.to_string().contains("from-mcp"));
    child.kill().await.unwrap();
}
