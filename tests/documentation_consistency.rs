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
        // Anchored to the §6 table ROW, not to the bare name. `contains("Man")`
        // was satisfied unconditionally by the manual's own H1
        // (`# apexe User Manual`), so deleting the Man row — the exact drift
        // this test says it exists to catch — left it green.
        assert!(
            manual.contains(&format!("| **{parser}** |")),
            "docs/user-manual.md's Tier 1 table omits the {parser} parser row"
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
/// The wiring is exercised first. The previous version of this guard read only
/// `type_name::<HealthOnlyCircuitBreaker>()`, which proves the type is still
/// exported and nothing about whether `build_executor` installs it — so §9.5's
/// "wires two middleware on by default" could go stale with the test green.
///
/// One limit, stated rather than papered over: `HealthOnlyCircuitBreaker::name`
/// deliberately returns `"circuit_breaker"`, the same name apcore's own
/// middleware uses, because apcore keys ordering and duplicate detection on it.
/// So the installed-chain listing proves the breaker is wired and honours the
/// flag; it cannot prove *which* breaker. That half is pinned by behaviour
/// instead, in `breaker.rs::test_acl_denials_do_not_open_the_circuit`.
#[test]
fn test_user_manual_names_the_wired_circuit_breaker() {
    use apexe::module::{build_executor, ExecutorOptions};

    let installed = |enabled: bool| -> Vec<String> {
        let opts = ExecutorOptions {
            modules_dir: None,
            timeout_ms: 30_000,
            acl_path: None,
            filter: apexe::module::ModuleFilter::default(),
            audit_path: None,
            enable_logging: false,
            log_arguments: false,
            enable_approval: false,
            enable_circuit_breaker: enabled,
            enable_retry: false,
            approval_store: None,
        };
        build_executor(&opts)
            .expect("an executor with no modules still builds")
            .middlewares()
    };

    assert!(
        installed(true).iter().any(|name| name == "circuit_breaker"),
        "build_executor must wire the circuit breaker on by default, as §9.5 says"
    );
    assert!(
        !installed(false)
            .iter()
            .any(|name| name == "circuit_breaker"),
        "--no-circuit-breaker must actually remove it, or the flag is decorative"
    );

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

/// The manual's account of `--enable-approval` must match what the gate does.
///
/// This claim has now been wrong in both directions. §9.6 once promised a
/// prompt that no CLI-launched server could deliver; it was then corrected to
/// "denies every call", which apcore-mcp 0.18 made wrong in turn by giving an
/// externally-built handler a route to the live elicitation callback. Either
/// error costs the same: an operator who cannot predict what the flag does
/// turns it off and lands on the ungoverned default.
///
/// The gate is driven first, so the manual is only held to a claim the runtime
/// has just demonstrated. A bare `ApprovalRequest` carries no context and so no
/// live callback id — the same position a client that declared no elicitation
/// support puts the gate in.
#[tokio::test]
async fn test_user_manual_describes_what_the_approval_gate_actually_does() {
    use apcore::approval::{ApprovalHandler, ApprovalRequest};

    let gate = apexe::module::ApprovalGate::new();
    let mut request = ApprovalRequest::default();
    request.module_id = "cli.rm".to_string();
    let outcome = gate
        .request_approval(&request)
        .await
        .expect("the gate answers rather than erroring");

    assert_eq!(
        outcome.status, "rejected",
        "with no prompt deliverable the gate must fail closed"
    );
    let reason = outcome.reason.unwrap_or_default();
    assert!(
        reason.contains("no MCP elicitation support"),
        "the refusal must say the prompt could not be delivered, not just that it was \
         refused: {reason}"
    );
    assert!(
        reason.contains("--acl"),
        "the refusal must name what to use instead: {reason}"
    );

    let manual = include_str!("../docs/user-manual.md");
    assert!(
        manual.contains("ApprovalGate"),
        "docs/user-manual.md must name the approval handler that is actually wired"
    );
    // The version is load-bearing: on apcore-mcp 0.17 the prompt cannot be
    // delivered at all, so a reader on an older pin needs to know why the flag
    // behaves differently for them.
    assert!(
        manual.contains("0.18 or later"),
        "docs/user-manual.md must state the apcore-mcp version the prompt needs"
    );
    // Both retired claims. The first over-promised, the second under-promised.
    assert!(
        !manual.contains("blocks until the connected MCP client's user responds"),
        "docs/user-manual.md still describes the pre-0.18 promise"
    );
    // Not a single literal: the stale claim survived in two other spellings
    // (`**Deny** every call to a ...` in the §4.2 flag table and
    // `unconditional **deny** gate` in §11) while the pinned string matched
    // neither, so the guard stayed green over live drift. Any line pairing
    // `enable-approval` with deny-language is what has to fail.
    for (number, line) in manual.lines().enumerate() {
        let lowered = line.to_lowercase();
        if !lowered.contains("enable-approval") {
            continue;
        }
        assert!(
            !(lowered.contains("deny") || lowered.contains("denies")),
            "docs/user-manual.md:{} still describes --enable-approval as a deny gate: {line}",
            number + 1
        );
    }
}

/// SSE is served again — apcore-mcp 0.18 scoped a session per connection, so
/// the cross-client delivery defect that made apexe refuse it is gone. It is
/// still deprecated upstream, so no document may present it without saying so.
///
/// The builder is exercised first, in the direction that now holds: a document
/// is required to carry the caveat only for as long as apexe actually serves
/// the transport. If SSE were refused again, this test would fail here rather
/// than silently keep asserting prose about a transport nobody can reach.
#[test]
fn test_every_document_marks_sse_as_deprecated() {
    assert!(
        apexe::mcp::McpServerBuilder::new()
            .transport("sse")
            .build()
            .is_ok(),
        "SSE must build without an acknowledgement flag"
    );

    for (name, text) in [
        (
            "docs/user-manual.md",
            include_str!("../docs/user-manual.md"),
        ),
        ("docs/quickstart.md", include_str!("../docs/quickstart.md")),
        ("README.md", include_str!("../README.md")),
    ] {
        // The caveat may sit on the mention itself (a table cell, a sentence) or
        // on the line immediately above it — these documents introduce a command
        // with a comment and open a callout with a headline. A reader sees both,
        // so both count; anything further away does not.
        let lines: Vec<&str> = text.lines().collect();
        let mut mentions = 0usize;
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("--transport sse") {
                continue;
            }
            mentions += 1;
            let window = match index {
                0 => line.to_lowercase(),
                _ => format!("{}\n{}", lines[index - 1], line).to_lowercase(),
            };
            assert!(
                window.contains("deprecated"),
                "{name} presents `--transport sse` without its caveat: {line}"
            );
        }
        assert!(
            mentions > 0,
            "{name} no longer mentions `--transport sse` at all — this guard has \
             stopped guarding anything; re-anchor it on whatever spelling replaced it"
        );
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

/// Every document that introduces `apexe a2a` must disclose that it has no
/// transport authentication.
///
/// The manual said so; the README did not, and a reader who starts from the
/// README meets the `--auth*` flags under `apexe serve` and reasonably assumes
/// they apply. apcore-a2a has no `Authenticator` at all, so the only defence is
/// the bind address — which is exactly what the refusal below enforces.
#[tokio::test]
async fn test_every_document_discloses_that_a2a_has_no_authentication() {
    let err = apexe::a2a::A2aServerBuilder::new()
        .url("http://0.0.0.0:8000")
        .agent_card()
        .await
        .expect_err("a non-loopback A2A bind must refuse without the acknowledgement");
    assert!(
        err.message.contains("no transport authentication"),
        "the refusal must say why it refuses: {}",
        err.message
    );
    assert!(
        err.message.contains("--allow-unauthenticated-bind"),
        "the refusal must name the acknowledgement flag: {}",
        err.message
    );

    for (name, text) in [
        (
            "docs/user-manual.md",
            include_str!("../docs/user-manual.md"),
        ),
        ("README.md", include_str!("../README.md")),
    ] {
        assert!(
            text.contains("no transport authentication"),
            "{name} introduces `apexe a2a` without disclosing that it has none"
        );
        assert!(
            text.contains("--allow-unauthenticated-bind"),
            "{name} must name the flag a non-loopback A2A bind demands"
        );
    }
}

/// Both dependency tables must state the versions `Cargo.toml` actually
/// requires.
///
/// The apcore 0.27 / apcore-mcp 0.18 / apcore-a2a 0.5 upgrade left README.md
/// and FEATURE_MANIFEST.md naming 0.26 / 0.17 / 0.4 — through a full `make
/// check`, because nothing tied the prose to the manifest. A reader consulting
/// either table to reproduce the build gets a graph that no longer resolves,
/// and the tables are the only place apexe states which upstream contract it
/// codes against.
///
/// Anchored to `Cargo.toml` rather than to literals, so the next bump fails
/// here instead of shipping.
#[test]
fn test_every_dependency_table_states_the_required_version() {
    let manifest = include_str!("../Cargo.toml");
    let readme = include_str!("../README.md");
    let features = include_str!("../docs/FEATURE_MANIFEST.md");

    // The first quoted string on the crate's dependency line is its version,
    // for both `foo = "0.1"` and `foo = { version = "0.1", .. }`.
    let required = |crate_name: &str| -> String {
        let line = manifest
            .lines()
            .map(str::trim)
            .find(|line| {
                line.strip_prefix(crate_name)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
            .unwrap_or_else(|| panic!("Cargo.toml has no dependency line for {crate_name}"));
        let (_, after) = line.split_once('"').expect("a quoted version");
        let (version, _) = after.split_once('"').expect("a closed quote");
        version.to_string()
    };

    for crate_name in [
        "apcore",
        "apcore-a2a",
        "apcore-cli",
        "apcore-mcp",
        "apcore-toolkit",
    ] {
        let version = required(crate_name);

        // README links the crate, then names the version: `](..-rust) 0.27 |`.
        let readme_row = format!("-rust) {version} |");
        let readme_mentions = readme
            .lines()
            .filter(|line| line.contains(crate_name) && line.starts_with('|'))
            .count();
        assert!(
            readme_mentions > 0,
            "README.md dependency table lost its {crate_name} row"
        );
        assert!(
            readme
                .lines()
                .any(|line| line.contains(crate_name) && line.contains(&readme_row)),
            "README.md must state {crate_name} {version} to match Cargo.toml"
        );

        // FEATURE_MANIFEST uses a plain table: `| `apcore` | 0.27 | .. |`.
        let manifest_row = format!("| `{crate_name}` | {version} |");
        assert!(
            features.contains(&manifest_row),
            "docs/FEATURE_MANIFEST.md must state `{manifest_row}` to match Cargo.toml"
        );
    }
}

/// The manual's account of an A2A denial must match what apcore-a2a maps.
///
/// §9.1 tells the reader that an ACL denial reaches an A2A caller as `-32001
/// Task not found` — a claim about upstream, not about apexe, and therefore
/// one that can silently become false on an `apcore-a2a` bump. The advice that
/// depends on it ("check `audit.jsonl`, not the response") would then send an
/// operator to the wrong place, so the claim is anchored to the mapper itself.
///
/// If this fails because upstream started reporting the real reason, that is
/// good news: delete the caveat from §9.1 rather than restoring the mapping.
#[test]
fn test_user_manual_matches_the_a2a_denial_apcore_actually_maps() {
    let denial = apcore::ModuleError::new(
        apcore::ErrorCode::ACLDenied,
        "Access denied: caller 'None' cannot access module 'cli.cp'".to_string(),
    );
    let mapped = apcore_a2a::ErrorMapper::to_jsonrpc_error(&denial);

    let manual = include_str!("../docs/user-manual.md");
    assert!(
        manual.contains(&format!("`-{}`", -mapped.code)),
        "docs/user-manual.md must state the JSON-RPC code {} that apcore-a2a maps an ACL \
         denial to",
        mapped.code
    );
    assert!(
        manual.contains(&format!("`{}`", mapped.message)),
        "docs/user-manual.md must quote the message apcore-a2a returns: {:?}",
        mapped.message
    );
    assert!(
        !mapped.message.contains("cli.cp"),
        "the caveat exists because the reason is withheld; upstream now leaks the module \
         name, so §9.1 needs rewriting: {:?}",
        mapped.message
    );
}
