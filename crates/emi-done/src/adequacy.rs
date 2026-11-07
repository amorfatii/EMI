//! Adequacy: how tightly the laws and the proving tests pin down what the
//! change wrote. Each mutant is one token changed on a line the change
//! added (a comparison flipped, an operator or a literal swapped); only the
//! proving tests and the passing laws run against it. A mutant none of
//! them notices survives: the evidence would not tell that line going
//! wrong. Equivalent mutants exist, so survivors are places to look, and
//! by default they are reported, not acted on (`adequacy.on_survivors`).
//! Off unless `adequacy.max_mutants` is set: each mutant is one more run.

use emi_driver::{Driver, LawEvidence};
use serde_json::{Value, json};

use crate::Config;

/// What the mutants showed; `None` when adequacy is off.
#[derive(Clone, Debug, Default)]
pub struct Adequacy {
    pub report: Value,
    pub survivors: Vec<Value>,
}

impl Adequacy {
    pub fn to_value(&self) -> Value {
        self.report.clone()
    }
}

/// The tests that make up the evidence the mutants are run against.
fn evidence_ids(change: &Value, laws: &LawEvidence) -> Vec<String> {
    let mut ids: Vec<String> = change["proving_tests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["id"].as_str().map(str::to_owned))
        .collect();
    ids.extend(
        laws.laws
            .iter()
            .filter(|l| matches!(l.status, "holds" | "now_holds"))
            .map(|l| l.id.clone()),
    );
    ids.sort();
    ids.dedup();
    ids
}

/// Run the mutants of the candidate's added lines against the proving
/// tests and the passing laws.
pub fn measure(
    run: &emi_driver::Run,
    config: &Config,
    (change, laws): (&Value, &LawEvidence),
    driver: &dyn Driver,
) -> Option<Adequacy> {
    let max = usize::try_from(config.adequacy.max_mutants).ok()?;
    if max == 0 {
        return None;
    }
    let ids = evidence_ids(change, laws);
    if ids.is_empty() {
        return Some(Adequacy {
            report: json!({"state": "nothing_to_run",
                           "note": "no proving test or passing law to run the mutants against"}),
            survivors: Vec::new(),
        });
    }
    let mut report = match driver.adequacy(run, &ids, max) {
        Ok(report) => report,
        Err(why) => {
            return Some(Adequacy {
                report: json!({"state": "unverified", "reason": why.reason()}),
                survivors: Vec::new(),
            });
        }
    };
    // the unmutated run's record holds durations: not part of a decision
    if let Some(m) = report.as_object_mut() {
        m.remove("baseline");
        m.remove("isolation");
    }
    let survivors = report["survivors"].as_array().cloned().unwrap_or_default();
    Some(Adequacy { report, survivors })
}

/// One instruction per surviving mutant.
pub fn repair_lines(a: &Adequacy) -> Vec<String> {
    a.survivors
        .iter()
        .map(|s| {
            format!(
                "{}:{} in {}: changing `{}` to `{}` breaks no proving test or law; add a test \
                 that fails when that line goes wrong.",
                s["file"].as_str().unwrap_or("?"),
                s["line"],
                s["function"].as_str().unwrap_or("?"),
                s["original"].as_str().unwrap_or("?"),
                s["replacement"].as_str().unwrap_or("?"),
            )
        })
        .collect()
}

/// What the receipt does not establish about the mutants.
pub fn limits(a: &Adequacy) -> Vec<String> {
    match a.report["state"].as_str() {
        Some("measured") => vec![format!(
            "{} of {} mutant(s) of the changed lines survived the proving tests and laws; a \
             survivor may be equivalent to the original.",
            a.survivors.len(),
            a.report["run"]
        )],
        Some("unverified") => vec![format!(
            "The mutants of the changed lines could not be run: {}",
            a.report["reason"]
                .as_str()
                .unwrap_or("the unmutated run did not pass")
        )],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_evidence_is_the_proving_tests_and_the_laws_that_hold() {
        let change = json!({"proving_tests": [{"id": "tests/t.py::a"}, {"id": "tests/t.py::a"}]});
        let laws = LawEvidence {
            laws: vec![
                emi_driver::Law {
                    id: ".emi/laws/l.py::x".into(),
                    baseline: "passed".into(),
                    candidate: "passed".into(),
                    status: "holds",
                    counterexample: None,
                    examples: None,
                },
                emi_driver::Law {
                    id: ".emi/laws/l.py::y".into(),
                    baseline: "failed".into(),
                    candidate: "failed".into(),
                    status: "unmet",
                    counterexample: None,
                    examples: None,
                },
            ],
            ..LawEvidence::default()
        };
        assert_eq!(
            evidence_ids(&change, &laws),
            [".emi/laws/l.py::x", "tests/t.py::a"]
        );
    }

    #[test]
    fn a_survivor_says_which_line_and_what_to_do() {
        let a = Adequacy {
            report: json!({"state": "measured", "run": 4}),
            survivors: vec![json!({"file": "app.py", "line": 3, "function": "add",
                                   "original": "<", "replacement": ">="})],
        };
        let line = &repair_lines(&a)[0];
        assert!(
            line.starts_with("app.py:3 in add: changing `<` to `>=`"),
            "{line}"
        );
        assert!(limits(&a)[0].starts_with("1 of 4 mutant(s)"));
    }
}
