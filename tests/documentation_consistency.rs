#[test]
fn test_user_manual_does_not_claim_overlay_is_repeatable() {
    let manual = include_str!("../docs/user-manual.md");
    let repeatable_claims: Vec<_> = manual
        .lines()
        .filter(|line| line.contains("--overlay") && line.contains("repeatable"))
        .collect();

    assert!(
        repeatable_claims.is_empty(),
        "`--overlay` accepts one explicit override, but the manual says it is repeatable: \
         {repeatable_claims:?}"
    );
}

#[test]
fn test_feature_manifest_lists_every_builtin_parser() {
    let parser_count = apexe::scanner::ParserPipeline::new(None).parser_count();
    let manifest = include_str!("../docs/FEATURE_MANIFEST.md");
    let expected_inventory =
        format!("({parser_count} built-in parsers: Man, BSD Usage, GNU, Click, Cobra, Clap)");

    assert!(
        manifest.contains(&expected_inventory),
        "feature manifest must match the runtime parser inventory: {expected_inventory}"
    );
    assert!(
        manifest.contains("Man, BSD Usage, GNU, Click, Cobra, Clap format parsers"),
        "feature manifest module map must include every built-in parser"
    );
}

/// Every document that states the parser count must state the same one.
///
/// The guard used to cover `FEATURE_MANIFEST.md` alone, so adding the Man
/// parser updated that file and silently left three others behind: the user
/// manual still said "Five", and the README and the scanner's own crate doc
/// still listed the four parsers from before BSD Usage existed. The manual is
/// what a user reads to work out why a tool parsed the way it did, and the
/// omitted parser is the one tried *first* for every tool.
#[test]
fn test_every_document_states_the_same_parser_count() {
    let parser_count = apexe::scanner::ParserPipeline::new(None).parser_count();
    let spelled = match parser_count {
        5 => "Five",
        6 => "Six",
        7 => "Seven",
        other => panic!("add the word for {other} parsers to this test"),
    };

    let manual = include_str!("../docs/user-manual.md");
    assert!(
        manual.contains(&format!("{spelled} built-in parsers")),
        "docs/user-manual.md must say `{spelled} built-in parsers` to match the runtime"
    );

    let readme = include_str!("../README.md");
    assert!(
        readme.contains(&format!("{parser_count} built-in parsers")),
        "README.md must say `{parser_count} built-in parsers` to match the runtime"
    );

    // Every parser's display name must appear in the manual's table and in the
    // crate doc, so a rename cannot pass by keeping the count the same.
    let crate_doc = include_str!("../src/scanner/mod.rs");
    for parser in ["Man", "BSD Usage", "GNU", "Click", "Cobra", "Clap"] {
        assert!(
            manual.contains(parser),
            "docs/user-manual.md omits the {parser} parser"
        );
        assert!(
            crate_doc.contains(parser),
            "src/scanner/mod.rs Tier 1 list omits the {parser} parser"
        );
    }
}

/// The manual must name the middleware `build_executor` actually wires.
///
/// §9.5 described `CircuitBreakerMiddleware` counting every failure — which is
/// precisely the behaviour reported as a defect and replaced by
/// `HealthOnlyCircuitBreaker`. The distinction is user-visible: an agent doing
/// schema trial-and-error is exactly the case the old text told readers to fear.
///
/// Anchored to the runtime type name rather than to a literal, so renaming the
/// middleware in `src/module/breaker.rs` fails this test.
#[test]
fn test_user_manual_names_the_wired_circuit_breaker() {
    let wired = std::any::type_name::<apexe::module::HealthOnlyCircuitBreaker>()
        .rsplit("::")
        .next()
        .expect("a type name always has a last segment");
    let manual = include_str!("../docs/user-manual.md");

    assert!(
        manual.contains(wired),
        "docs/user-manual.md §9.5 must name the middleware that is actually wired ({wired})"
    );
    assert!(
        !manual.contains("**`CircuitBreakerMiddleware`**"),
        "docs/user-manual.md still documents the replaced middleware as the wired one"
    );
}

/// The manual must not promise an approval prompt that cannot happen.
///
/// §9.6 previously said `--enable-approval` "blocks until the connected MCP
/// client's user responds to an elicitation prompt". No such path exists from
/// a CLI-launched server: the flag denies every gated call. An operator who
/// reads the old text enables it, finds every destructive tool bricked, and
/// turns it back off — landing on the ungoverned default.
///
/// The handler is driven first, so the manual is only held to a claim the
/// runtime has just demonstrated.
#[tokio::test]
async fn test_user_manual_describes_approval_as_a_deny_gate() {
    use apcore::approval::{ApprovalHandler, ApprovalRequest};

    let handler = apexe::module::DenyApprovalHandler::new();
    let mut request = ApprovalRequest::default();
    request.module_id = "cli.rm".to_string();
    let outcome = handler
        .request_approval(&request)
        .await
        .expect("the deny gate answers rather than erroring");
    assert_eq!(
        outcome.status, "rejected",
        "the wired approval handler must deny, not prompt"
    );
    let reason = outcome.reason.unwrap_or_default();
    assert!(
        reason.contains("denies every such call"),
        "the denial must say it is unconditional: {reason}"
    );

    let manual = include_str!("../docs/user-manual.md");
    assert!(
        manual.contains("DenyApprovalHandler"),
        "docs/user-manual.md must name the approval handler that is actually wired"
    );
    assert!(
        !manual.contains("blocks until the connected MCP client's user responds"),
        "docs/user-manual.md still promises an elicitation prompt that cannot be delivered"
    );
    assert!(
        !manual.contains("Sends approval request to MCP client"),
        "docs/user-manual.md's middleware table still promises an approval prompt"
    );
    // §11 kept calling the flag a "human-in-the-loop gate" long after §9.6 and
    // the CHANGELOG agreed there is no human in the loop to gate on.
    assert!(
        !manual.contains("human-in-the-loop"),
        "docs/user-manual.md still calls --enable-approval a human-in-the-loop gate"
    );
}

/// SSE is refused by default, so no document may present it as an ordinary
/// option: with two clients connected, one receives the other's tool output.
///
/// The refusal is exercised first — a document is only required to carry the
/// caveat for as long as the builder actually enforces it.
#[test]
fn test_every_document_marks_sse_as_deprecated() {
    let Err(err) = apexe::mcp::McpServerBuilder::new().transport("sse").build() else {
        panic!("SSE must be refused without --allow-deprecated-sse");
    };
    assert!(
        err.message.contains("--allow-deprecated-sse"),
        "the refusal must name the acknowledgement flag: {}",
        err.message
    );

    for (name, text) in [
        (
            "docs/user-manual.md",
            include_str!("../docs/user-manual.md"),
        ),
        ("docs/quickstart.md", include_str!("../docs/quickstart.md")),
        ("README.md", include_str!("../README.md")),
    ] {
        for line in text.lines().filter(|l| l.contains("--transport sse")) {
            assert!(
                line.contains("--allow-deprecated-sse") || line.contains("deprecated"),
                "{name} presents `--transport sse` without its caveat: {line}"
            );
        }
    }
}

/// `--prefix` / `--tags` gate execution, not just listing.
///
/// The v0.2.0 CHANGELOG introduced them as "access control" while they
/// filtered `tools/list` alone. The assertion that matters is the runtime one:
/// a filtered-out module must not be *callable*. That is established here
/// against a real `Executor`, and only then is the manual required to say so.
#[tokio::test]
async fn test_user_manual_states_that_filters_gate_execution() {
    use apcore::ErrorCode;
    use apexe::module::{build_executor, ExecutorOptions, ModuleFilter};

    let dir = tempfile::TempDir::new().unwrap();
    let modules = vec![apcore_toolkit::ScannedModule::new(
        "cli.cp".to_string(),
        "Copy".to_string(),
        serde_json::json!({"type": "object"}),
        serde_json::json!({"type": "object"}),
        vec!["cli".to_string()],
        "exec:///bin/cp".to_string(),
    )];
    apexe::output::YamlOutput::without_verification()
        .write(&modules, dir.path(), false)
        .unwrap();

    let executor = build_executor(&ExecutorOptions {
        modules_dir: Some(dir.path()),
        timeout_ms: 1_000,
        acl_path: None,
        filter: ModuleFilter {
            prefix: Some("cli.git".to_string()),
            tags: None,
        },
        audit_path: None,
        enable_logging: false,
        log_arguments: false,
        enable_approval: false,
        enable_circuit_breaker: false,
        enable_retry: false,
        approval_store: None,
    })
    .unwrap();

    let err = executor
        .call("cli.cp", serde_json::json!({}), None, None)
        .await
        .expect_err("a filtered-out module must not be callable");
    assert_eq!(err.code, ErrorCode::ModuleNotFound);

    let manual = include_str!("../docs/user-manual.md");
    assert!(
        manual.contains("applied at\n**registration** time")
            || manual.contains("applied at **registration** time"),
        "docs/user-manual.md must say the tool filter is applied at registration time"
    );
    assert!(
        manual.contains(&format!("{:?}", err.code)),
        "docs/user-manual.md must name the error a filtered-out module returns ({:?})",
        err.code
    );
}

/// The manual must document the per-transport authentication defaults, since
/// turning auth on by default is a breaking change for existing HTTP setups.
///
/// Every claim is checked against `resolve_auth` first. Presence-only
/// assertions on the four literals could not fail if the fallback flipped from
/// `AuthMode::Token` to `AuthMode::None`, which is the regression that matters.
#[test]
fn test_user_manual_documents_transport_authentication() {
    use apexe::auth::{resolve_auth, AuthMode, AuthOptions, ResolvedAuth};

    let manual = include_str!("../docs/user-manual.md");

    // HTTP on loopback defaults to a *generated* bearer token.
    let loopback = resolve_auth("http", "127.0.0.1", &AuthOptions::default()).unwrap();
    assert!(
        loopback.require_auth(),
        "HTTP on loopback must require a credential by default"
    );
    assert!(
        matches!(
            loopback,
            ResolvedAuth::Token {
                generated: true,
                ..
            }
        ),
        "the loopback default must be a generated token"
    );
    assert!(
        manual.contains("Authorization: Bearer"),
        "docs/user-manual.md must state the header the generated token goes in"
    );
    assert!(
        manual.contains("--auth-token") && manual.contains("APEXE_AUTH_TOKEN"),
        "docs/user-manual.md must document both ways to pin the token"
    );

    // stdio is the one transport with no credential.
    let stdio = resolve_auth("stdio", "127.0.0.1", &AuthOptions::default()).unwrap();
    assert!(!stdio.require_auth(), "stdio must not require a credential");

    // `--auth none` on a non-loopback bind refuses without the acknowledgement.
    let refused = resolve_auth(
        "http",
        "0.0.0.0",
        &AuthOptions {
            mode: Some(AuthMode::None),
            ..AuthOptions::default()
        },
    )
    .expect_err("--auth none on a public bind must refuse to start");
    let flag = "--allow-unauthenticated-bind";
    assert!(refused.message.contains(flag), "{}", refused.message);
    assert!(
        manual.contains(flag),
        "docs/user-manual.md must document `{flag}`, which the refusal points operators at"
    );
}
