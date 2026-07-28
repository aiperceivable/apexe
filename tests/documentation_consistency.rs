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
