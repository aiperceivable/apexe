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
        format!("({parser_count} built-in parsers: BSD Usage, GNU, Click, Cobra, Clap)");

    assert!(
        manifest.contains(&expected_inventory),
        "feature manifest must match the runtime parser inventory: {expected_inventory}"
    );
    assert!(
        manifest.contains("BSD Usage, GNU, Click, Cobra, Clap format parsers"),
        "feature manifest module map must include the BSD Usage parser"
    );
}
