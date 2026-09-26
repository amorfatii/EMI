//! Every built-in rule compiles for its languages and passes its own
//! examples.

#[test]
fn builtin_rules_pass_their_examples() {
    let rules = emi_rules::load(&[], true).unwrap_or_else(|e| panic!("{e}"));
    assert!(rules.len() >= 14, "{} rules", rules.len());
    let failures = emi_rules::self_test(&rules);
    assert!(
        failures.is_empty(),
        "rule example failures:\n{}",
        failures.join("\n")
    );
    for r in &rules {
        assert!(
            !r.examples.matching.is_empty() && !r.examples.no_match.is_empty(),
            "{} needs examples",
            r.id
        );
        assert!(!r.description.is_empty(), "{} needs a description", r.id);
    }
}
