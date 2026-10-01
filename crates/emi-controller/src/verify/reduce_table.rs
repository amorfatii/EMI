//! The reducer's truth table: every combination of requirement states and
//! gate states, against an independent statement of the precedence
//! (rejected, then incomplete, then needs_review, then accepted).

use serde_json::{Value, json};

use super::{Gates, reduce};

/// The strictness order of the four decisions.
fn rank(decision: &str) -> u8 {
    match decision {
        "accepted" => 0,
        "needs_review" => 1,
        "incomplete" => 2,
        _ => 3,
    }
}

/// What each state of a gate means for the decision, written down here and
/// not derived from the reducer: a failure rejects, an unresolved gate
/// leaves the decision incomplete, a review signal needs a person.
fn effect(state: &str) -> &'static str {
    match state {
        "fail" => "rejected",
        "unverified" => "incomplete",
        "needs_review" | "open" => "needs_review",
        _ => "accepted",
    }
}

/// A gate in `state`, shaped as the verification code writes it.
fn gate(name: &str, state: &str) -> Value {
    let mut g = json!({"state": state});
    match (name, state) {
        ("envelope", "fail") => g["violations"] = json!([{"path": "x", "reason": "r"}]),
        ("structural", "fail") => g["verdict"] = json!({"reasons": ["grew"]}),
        ("guardrails", "fail") => {
            g["violations"] = json!([{"tier": "block", "id": "tests.removed", "path": "t"}]);
        }
        ("guardrails", "needs_review") => {
            g["violations"] = json!([{"tier": "review", "id": "suppress.added", "path": "a"}]);
        }
        ("verification", "fail") => g["inventory"] = json!({"state": "fail", "missing": ["t"]}),
        ("verification", "needs_review") => g["restored"] = json!(["tests/test_a.py"]),
        ("refactor", "fail") => g["required"] = json!(0.2),
        ("dependencies", "unverified") => g["changed"] = json!(["venv"]),
        ("escalation", "open") => g["escalations"] = json!([{"index": 1, "reason": "r"}]),
        _ => {}
    }
    g
}

/// Each gate and the states it can be in.
const GATES: [(&str, &[&str]); 7] = [
    ("envelope", &["pass", "fail"]),
    (
        "structural",
        &["not_required", "pass", "needs_review", "unverified", "fail"],
    ),
    (
        "guardrails",
        &["not_required", "pass", "needs_review", "unverified", "fail"],
    ),
    (
        "verification",
        &["not_required", "pass", "needs_review", "unverified", "fail"],
    ),
    (
        "refactor",
        &["not_required", "pass", "needs_review", "unverified", "fail"],
    ),
    ("dependencies", &["not_required", "pass", "unverified"]),
    ("escalation", &["none", "open", "unverified"]),
];

fn requirement(state: &str) -> Value {
    json!({"id": "R1", "state": state})
}

/// The decision and its reasons for `ledger` and the gates in `states`
/// (one state per entry of [`GATES`]).
fn decide(ledger: &[Value], states: &[&str], require_all: bool) -> (&'static str, Vec<String>) {
    let g: Vec<Value> = GATES
        .iter()
        .zip(states)
        .map(|((name, _), state)| gate(name, state))
        .collect();
    let gates = Gates {
        envelope: &g[0],
        structural: &g[1],
        guardrails: &g[2],
        verification: &g[3],
        refactor: &g[4],
        dependencies: &g[5],
        escalation: &g[6],
    };
    let (decision, reasons) = reduce(ledger, &gates, require_all);
    (decision, reasons.into_iter().map(|r| r.text).collect())
}

/// Every choice of one state per gate.
fn gate_combinations() -> Vec<Vec<&'static str>> {
    GATES.iter().fold(vec![Vec::new()], |acc, (_, states)| {
        acc.iter()
            .flat_map(|prefix| {
                states.iter().map(move |s| {
                    let mut next = prefix.clone();
                    next.push(*s);
                    next
                })
            })
            .collect()
    })
}

#[test]
fn every_combination_reduces_to_the_strictest_signal() {
    let mut accepted_with_unverified = 0;
    for ledger_state in ["verified", "unverified", "failed"] {
        for require_all in [true, false] {
            for states in gate_combinations() {
                let (decision, reasons) =
                    decide(&[requirement(ledger_state)], &states, require_all);
                let ledger_effect = match (ledger_state, require_all) {
                    ("failed", _) => "rejected",
                    ("unverified", true) => "incomplete",
                    _ => "accepted",
                };
                let expected = states
                    .iter()
                    .map(|s| effect(s))
                    .chain([ledger_effect])
                    .max_by_key(|d| rank(d))
                    .unwrap_or("accepted");
                let case = format!("{ledger_state} require_all={require_all} {states:?}");
                assert_eq!(decision, expected, "{case}: {reasons:?}");
                assert!(!reasons.is_empty(), "{case}: a decision names its reasons");
                if decision == "accepted" {
                    let says_all = reasons[0].contains("every requirement verified");
                    assert_eq!(says_all, ledger_state == "verified", "{case}: {reasons:?}");
                    accepted_with_unverified += usize::from(ledger_state == "unverified");
                }
            }
        }
    }
    assert!(accepted_with_unverified > 0, "the policy path is exercised");
}

/// Every gate in the state a clean candidate leaves it in.
fn clean_gates() -> Vec<&'static str> {
    GATES
        .iter()
        .map(|(name, _)| {
            if *name == "escalation" {
                "none"
            } else {
                "pass"
            }
        })
        .collect()
}

#[test]
fn a_contract_with_no_requirements_verifies_nothing() {
    for require_all in [true, false] {
        let (decision, _) = decide(&[], &clean_gates(), require_all);
        assert_eq!(decision, "incomplete");
    }
}

#[test]
fn a_gate_state_the_reducer_does_not_know_is_a_gap_never_a_pass() {
    let clean = clean_gates();
    assert_eq!(
        decide(&[requirement("verified")], &clean, true).0,
        "accepted"
    );
    for (at, (name, _)) in GATES.iter().enumerate() {
        for unknown in ["errored", "", "PASS"] {
            let mut states = clean.clone();
            states[at] = unknown;
            let (decision, reasons) = decide(&[requirement("verified")], &states, true);
            assert_eq!(decision, "incomplete", "{name} {unknown:?}: {reasons:?}");
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains(name) && r.contains("state")),
                "{name} {unknown:?}: {reasons:?}"
            );
        }
    }
}

#[test]
fn only_the_ledger_states_verified_and_failed_decide_without_a_gap() {
    let clean = clean_gates();
    for (state, expected) in [
        ("verified", "accepted"),
        ("failed", "rejected"),
        ("unverified", "incomplete"),
        ("not_run", "incomplete"),
        ("", "incomplete"),
    ] {
        let (decision, reasons) = decide(&[requirement(state)], &clean, true);
        assert_eq!(decision, expected, "{state:?}: {reasons:?}");
    }
}
