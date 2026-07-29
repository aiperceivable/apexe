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
