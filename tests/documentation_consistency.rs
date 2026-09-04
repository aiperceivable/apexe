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

/// The manual's account of an A2A denial must match what a caller is told.
///
/// §9.1 quotes the code and text an ACL denial reaches an A2A caller as. Since
/// apcore-a2a 0.6 that is upstream's own answer rather than something apexe
/// re-codes: a dedicated `-32040`, and — because `apexe a2a` sets
/// `disclose_refusal_reason` — apcore's own message rather than the fixed
/// "Access denied" string. Both halves are asserted against the mapper, so a
/// bump that changes either one fails here instead of leaving §9.1 describing
/// text nobody receives.
#[test]
fn test_user_manual_matches_the_a2a_denial_a_caller_actually_receives() {
    let denial = apcore::ModuleError::new(
        apcore::ErrorCode::ACLDenied,
        "Access denied: caller 'None' cannot access module 'cli.cp'".to_string(),
    );
    let manual = include_str!("../docs/user-manual.md");

    // Default (no disclosure): a governance code, and the fixed per-class
    // string. This is what a deployment that does NOT set the flag gets.
    let fixed = apcore_a2a::ErrorMapper::to_jsonrpc_error(&denial);
    assert_eq!(fixed.code, -32040, "the access-denied code moved");
    assert_eq!(fixed.message, "Access denied");

    // What `apexe a2a` actually serves: the same code, apcore's own reason.
    let disclosed = apcore_a2a::ErrorMapper::to_jsonrpc_error_with(&denial, true);
    assert_eq!(disclosed.code, -32040);
    assert_eq!(
        disclosed.message, "Access denied: caller 'None' cannot access module 'cli.cp'",
        "apexe sets disclose_refusal_reason, so apcore's message must survive"
    );

    assert!(
        manual.contains("-32040"),
        "docs/user-manual.md must state the access-denied code a caller receives"
    );
    assert!(
        manual.contains("Access denied: caller 'None' cannot access module 'cli.cp'"),
        "docs/user-manual.md must quote the reason an A2A caller now receives"
    );
}

// ---------------------------------------------------------------------------
// Drift guards
//
// The tests above each pin one sentence that was found wrong. The ones below
// pin whole *surfaces* — every subcommand, every visible flag, every contract
// keyword — because the recurring failure is not a wrong sentence but a missing
// one: a feature lands, the code is reviewed, and the document that was
// supposed to describe it is simply never opened. `apexe policy` shipped and
// went undocumented in four files at once, and CI was green throughout.
// ---------------------------------------------------------------------------

/// The version is stated in three documents and generated from none of them.
///
/// It had drifted to three different answers at once: `Cargo.toml` said 0.7.0
/// and carried a matching git tag, both long-form documents said 0.6.0, and
/// `examples/README.md` still showed the 0.1.0 it was written against.
#[test]
fn test_every_document_states_the_crate_version() {
    let version = env!("CARGO_PKG_VERSION");

    let cases = [
        (
            "docs/user-manual.md",
            include_str!("../docs/user-manual.md"),
            format!("| **Version** | {version} |"),
        ),
        (
            "docs/FEATURE_MANIFEST.md",
            include_str!("../docs/FEATURE_MANIFEST.md"),
            format!("**Version:** {version}"),
        ),
        (
            "examples/README.md",
            include_str!("../examples/README.md"),
            format!("# apexe {version}"),
        ),
    ];

    for (path, contents, expected) in cases {
        assert!(
            contents.contains(&expected),
            "{path} must state the crate version -- expected to find `{expected}`"
        );
    }
}

/// A release tag without a changelog section leaves its users nothing to read.
///
/// `rust/v0.7.0` was tagged while every one of its entries still sat under
/// `## [Unreleased]`. Running before the tag is the point: `cargo test` is a
/// release prerequisite, so this fails while the omission is still cheap.
#[test]
fn test_changelog_has_a_section_for_the_current_version() {
    let version = env!("CARGO_PKG_VERSION");
    let changelog = include_str!("../CHANGELOG.md");
    let heading = format!("## [{version}]");

    assert!(
        changelog.contains(&heading),
        "CHANGELOG.md has no `{heading}` section. Promote `## [Unreleased]` \
         before tagging -- a tagged release with no entries tells its users nothing."
    );
}

/// Every subcommand clap accepts must have a section in the manual.
///
/// Read off `Cli::command()` rather than a hand-kept list, so a new subcommand
/// fails here the moment it parses. `apexe policy` shipped with no mention in
/// the manual, the README, the threat model or the changelog -- `--help` was
/// its only documentation.
#[test]
fn test_user_manual_documents_every_subcommand() {
    use clap::CommandFactory;

    let manual = include_str!("../docs/user-manual.md");
    let command = apexe::cli::Cli::command();

    for subcommand in command.get_subcommands() {
        let name = subcommand.get_name();
        // The manual's §4 headings read: ### 4.6 `apexe policy`
        let heading = format!("`apexe {name}`");
        assert!(
            manual
                .lines()
                .any(|line| { line.starts_with("### 4.") && line.contains(&heading) }),
            "docs/user-manual.md §4 has no `### 4.N {heading}` heading. \
             Every subcommand clap accepts needs a section in the commands reference."
        );
    }
}

/// Every flag a user can see must appear in the manual.
///
/// Hidden flags are exempt by definition -- `--allow-deprecated-sse` is
/// accepted only so existing invocations keep parsing, and is deliberately
/// absent from `--help`. Auto-generated `help`/`version` are exempt too.
///
/// The check is that the flag is mentioned *somewhere* in the manual rather
/// than in a particular table: a flag documented in the wrong section is a
/// reviewer's problem, while one documented nowhere is this test's.
#[test]
fn test_user_manual_documents_every_visible_flag() {
    use clap::CommandFactory;

    let manual = include_str!("../docs/user-manual.md");
    let command = apexe::cli::Cli::command();

    let mut undocumented: Vec<String> = Vec::new();
    let mut check = |owner: &str, arg: &clap::Arg| {
        if arg.is_hide_set() {
            return;
        }
        let Some(long) = arg.get_long() else {
            return;
        };
        if matches!(long, "help" | "version") {
            return;
        }
        // Options render as `--transport <TYPE>` and flags as `--explorer`, so
        // anchor on the opening backtick plus the literal and let either form
        // match.
        if !manual.contains(&format!("`--{long}")) {
            undocumented.push(format!("{owner} --{long}"));
        }
    };

    for arg in command.get_arguments() {
        check("apexe", arg);
    }
    for subcommand in command.get_subcommands() {
        for arg in subcommand.get_arguments() {
            check(subcommand.get_name(), arg);
        }
    }

    assert!(
        undocumented.is_empty(),
        "docs/user-manual.md documents no such flag: {undocumented:?}. \
         A flag a user can see in `--help` and cannot find in the manual is \
         a flag they will not use."
    );
}

/// Every `x-apexe-*` keyword the schema builder emits must be documented.
///
/// The set is read out of `src/` rather than kept in a list here, so it cannot
/// fall behind the code. These keywords are the contract an MCP client or an
/// overlay author binds against; three of them (`x-apexe-conflicts-with`,
/// `x-apexe-flag-position`, `x-apexe-positional`) were emitted into every
/// binding file on disk and described in no document at all.
#[test]
fn test_every_emitted_contract_keyword_is_documented() {
    let docs = [
        include_str!("../docs/user-manual.md"),
        include_str!("../docs/overlays.md"),
        include_str!("../docs/threat-model.md"),
    ];

    let mut keywords: Vec<String> = collect_contract_keywords("src".as_ref());
    keywords.sort();
    keywords.dedup();

    assert!(
        keywords.len() >= 10,
        "expected to find the contract keywords in src/, found {keywords:?} -- \
         the source walk is broken, not the documentation"
    );

    let undocumented: Vec<&String> = keywords
        .iter()
        .filter(|keyword| !docs.iter().any(|doc| doc.contains(keyword.as_str())))
        .collect();

    assert!(
        undocumented.is_empty(),
        "these contract keywords are emitted but documented nowhere: {undocumented:?}. \
         Add them to docs/user-manual.md §7.1."
    );
}

/// Collect every `x-apexe-*` literal appearing under `dir`, recursively.
fn collect_contract_keywords(dir: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(collect_contract_keywords(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            found.extend(extract_contract_keywords(&contents));
        }
    }
    found
}

/// Pull `x-apexe-<name>` literals out of one file's text.
///
/// Hand-rolled rather than a regex so the test carries no extra dependency.
/// A keyword ends at the first character that is not lowercase, a digit or a
/// hyphen, and a trailing hyphen is trimmed so a prose mention like
/// `x-apexe-flag-` does not become its own keyword.
fn extract_contract_keywords(contents: &str) -> Vec<String> {
    const PREFIX: &str = "x-apexe-";
    let mut found = Vec::new();
    let bytes = contents.as_bytes();
    let mut search_from = 0;

    while let Some(offset) = contents[search_from..].find(PREFIX) {
        let start = search_from + offset;
        let mut end = start + PREFIX.len();
        while end < bytes.len()
            && (bytes[end].is_ascii_lowercase()
                || bytes[end].is_ascii_digit()
                || bytes[end] == b'-')
        {
            end += 1;
        }
        let keyword = contents[start..end].trim_end_matches('-');
        if keyword.len() > PREFIX.len() {
            found.push(keyword.to_string());
        }
        search_from = end.max(start + 1);
    }
    found
}

/// Every table-of-contents link in the manual must reach a real heading.
///
/// The §15 link broke when the heading gained the words "Global Flags" and the
/// anchor was left behind -- silently, because a dead in-page anchor scrolls
/// nowhere rather than erroring. The manual is 1,500 lines and its TOC is how
/// it is navigated.
#[test]
fn test_user_manual_toc_anchors_resolve() {
    let manual = include_str!("../docs/user-manual.md");

    let anchors: Vec<String> = manual
        .lines()
        .filter(|line| line.starts_with("## "))
        .map(|line| github_slug(line.trim_start_matches("## ")))
        .collect();

    let mut broken: Vec<(&str, String)> = Vec::new();
    for line in manual.lines() {
        // TOC entries read: `4. [Commands Reference](#4-commands-reference)`
        let Some(open) = line.find("](#") else {
            continue;
        };
        if !line.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let rest = &line[open + 3..];
        let Some(close) = rest.find(')') else {
            continue;
        };
        let anchor = &rest[..close];
        if !anchors.iter().any(|heading| heading == anchor) {
            broken.push((line.trim(), anchor.to_string()));
        }
    }

    assert!(
        broken.is_empty(),
        "docs/user-manual.md table of contents links to anchors no heading \
         produces: {broken:?}. Known headings: {anchors:?}"
    );

    // The manual is published twice -- on GitHub and through MkDocs -- and the
    // two slug rules agree only while a heading contains nothing that gets
    // dropped from between two spaces. GitHub keeps the space each dropped
    // character sat next to and turns both into hyphens; MkDocs collapses the
    // run. So "Error Handling & AI Guidance" anchored as
    // `...handling--ai...` on one and `...handling-ai...` on the other, and
    // the TOC link worked in exactly one of the two places. Both headings that
    // hit this now spell the word "and"; this keeps it that way, because the
    // failure is invisible from whichever renderer you happen to be reading.
    let ambiguous: Vec<&str> = manual
        .lines()
        .filter(|line| line.starts_with("## ") && line.contains(" & "))
        .collect();
    assert!(
        ambiguous.is_empty(),
        "these headings slug differently on GitHub and MkDocs, so any link to \
         them is broken on one of the two: {ambiguous:?}. Write `and` instead of `&`."
    );
}

/// Reproduce GitHub's heading-to-anchor rule.
///
/// Lowercase, drop everything that is not alphanumeric, a space or a hyphen,
/// then turn spaces into hyphens. Note that a dropped character leaves its
/// neighbouring spaces behind, so `A & B` would anchor as `a--b` with two
/// hyphens -- MkDocs collapses that to one, which is why the caller also
/// refuses such headings outright.
fn github_slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Every file the MkDocs navigation points at must exist.
///
/// A nav entry naming a missing file is only a warning at build time, so the
/// site publishes with a dead menu item. `mkdocs build --strict` would catch
/// it, but cannot be enabled yet: the docs carry 16 pre-existing warnings for
/// links that are correct on GitHub and unresolvable inside the site (`../../
/// src/...`, `SECURITY.md`, `CHANGELOG.md`). This covers the part of `--strict`
/// that matters most, without waiting on that cleanup.
#[test]
fn test_every_mkdocs_nav_entry_resolves() {
    let config = include_str!("../mkdocs.yml");

    // Copied into docs/ by .github/workflows/deploy-docs.yml at build time, so
    // they are absent from a checkout by design.
    const GENERATED_AT_DEPLOY: &[&str] = &["index.md", "changelog.md", "examples.md"];

    let mut missing: Vec<&str> = Vec::new();
    let mut checked = 0usize;

    for line in config.lines() {
        // nav entries read: `      - User Manual: user-manual.md`
        let Some((_, target)) = line.rsplit_once(": ") else {
            continue;
        };
        let target = target.trim();
        if !target.ends_with(".md") {
            continue;
        }
        checked += 1;
        if GENERATED_AT_DEPLOY.contains(&target) {
            continue;
        }
        if !std::path::Path::new("docs").join(target).exists() {
            missing.push(target);
        }
    }

    assert!(
        checked >= 10,
        "parsed only {checked} nav entries from mkdocs.yml -- the parser is \
         broken, not the navigation"
    );
    assert!(
        missing.is_empty(),
        "mkdocs.yml navigates to files that do not exist under docs/: {missing:?}"
    );
}

/// Every document describing the scan pipeline must account for all its tiers.
///
/// A document listing only the three tiers that read the binary reads as a
/// complete account while omitting the one a human wrote by hand -- and tier 4
/// is the one that can *replace* the other three outright under
/// `mode: authoritative`. The count comes from the same constant the
/// orchestrator raises `scan_tier` to, so the documents cannot drift from the
/// code the way the parser count once did.
#[test]
fn test_every_document_accounts_for_the_top_scan_tier() {
    let top = apexe::scanner::MAX_SCAN_TIER;
    assert_eq!(top, 4, "add the word form for tier {top} to this test");

    let cases = [
        (
            "docs/user-manual.md",
            include_str!("../docs/user-manual.md"),
        ),
        ("README.md", include_str!("../README.md")),
        (
            "docs/FEATURE_MANIFEST.md",
            include_str!("../docs/FEATURE_MANIFEST.md"),
        ),
        ("docs/overlays.md", include_str!("../docs/overlays.md")),
    ];

    for (path, contents) in cases {
        let lowered = contents.to_lowercase();
        assert!(
            lowered.contains(&format!("tier {top}")) || lowered.contains("fourth tier"),
            "{path} describes the scan pipeline without accounting for tier {top} \
             (the curated overlay). Say `tier {top}` or `fourth tier`."
        );
    }
}
