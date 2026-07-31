#[test]
fn user_manual_does_not_claim_overlay_is_repeatable() {
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
fn feature_manifest_lists_every_builtin_parser() {
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
fn every_document_states_the_same_parser_count() {
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

/// The user manual must name the middleware `build_executor` actually wires.
///
/// §9.5 described `CircuitBreakerMiddleware` counting every failure — which is
/// precisely the behaviour reported as a defect and replaced by
/// `HealthOnlyCircuitBreaker`. The distinction is user-visible: an agent doing
/// schema trial-and-error is exactly the case the old text told readers to fear.
#[test]
fn user_manual_names_the_wired_circuit_breaker() {
    let manual = include_str!("../docs/user-manual.md");

    assert!(
        manual.contains("HealthOnlyCircuitBreaker"),
        "docs/user-manual.md §9.5 must name the middleware that is actually wired"
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
#[test]
fn user_manual_describes_approval_as_a_deny_gate() {
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
#[test]
fn every_document_marks_sse_as_deprecated() {
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

/// `--prefix` / `--tags` gate execution, not just listing. The v0.2.0 CHANGELOG
/// introduced them as "access control" while they filtered `tools/list` alone;
/// the manual must not reintroduce the weaker claim.
#[test]
fn user_manual_states_that_filters_gate_execution() {
    let manual = include_str!("../docs/user-manual.md");
    assert!(
        manual.contains("registration"),
        "docs/user-manual.md must say the tool filter is applied at registration"
    );
    assert!(
        manual.contains("ModuleNotFound"),
        "docs/user-manual.md must state what a filtered-out module returns"
    );
}

/// The manual must document the per-transport authentication defaults, since
/// turning auth on by default is a breaking change for existing HTTP setups.
#[test]
fn user_manual_documents_transport_authentication() {
    let manual = include_str!("../docs/user-manual.md");
    for expected in [
        "--auth-token",
        "--allow-unauthenticated-bind",
        "APEXE_AUTH_TOKEN",
        "Authorization: Bearer",
    ] {
        assert!(
            manual.contains(expected),
            "docs/user-manual.md must document `{expected}`"
        );
    }
}
