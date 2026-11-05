//! Change evidence: did the behaviour the task asked for come into being?
//! Measuring it is another crate's concern (`emi-proving`); the receipt
//! takes its result as a [`ChangeEvidence`] value, which keeps the seam to
//! one argument.

use serde_json::{Value, json};

/// What the receipt's `change` part rests on.
#[derive(Clone, Debug, PartialEq)]
pub enum ChangeEvidence {
    /// Nothing measured the change. The receipt says so, and the action
    /// does not depend on it.
    NotMeasured(String),
    /// A `change` object as plan.md interface 3 defines it.
    Measured(Value),
    /// A measurement was attempted and could not complete for an
    /// environment reason (a sandbox that would not start, a coverage run
    /// that failed). The action is `incomplete`.
    Failed(String),
}

/// A redundant test and why it would not notice the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redundant {
    pub id: String,
    /// `passes at the baseline` or `executes no changed line`; empty when
    /// the evidence did not say.
    pub reason: String,
}

/// A test the candidate adds that does not pass on it, or a test file
/// only the candidate cannot collect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing {
    pub id: String,
    /// `failed`, `errored` or `not collected`; empty when the evidence
    /// did not say.
    pub result: String,
}

/// What the action table and the repair lines read from the change part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Tests the candidate adds that do not pass on it.
    pub failing: Vec<Failing>,
    pub redundant: Vec<Redundant>,
    /// Changed functions no proving test executes, as `file:name (line n)`.
    pub unexercised: Vec<String>,
}

const STATUSES: &[&str] = &[
    "exercised",
    "partly_exercised",
    "unexercised",
    "none",
    "unknown",
];
const LISTS: &[&str] = &[
    "functions",
    "proving_tests",
    "redundant_tests",
    "unknown_tests",
    "not_measured",
];

/// The `change` object for evidence that was not or could not be measured.
fn unmeasured(reason: &str) -> Value {
    json!({"status": "unknown", "functions": [], "proving_tests": [],
           "redundant_tests": [], "failing_tests": [], "unknown_tests": [],
           "not_measured": [reason]})
}

fn function_name(f: &Value) -> String {
    format!(
        "{}:{} (line {})",
        f["file"].as_str().unwrap_or("?"),
        f["function"].as_str().unwrap_or("?"),
        f["line"]
    )
}

impl ChangeEvidence {
    /// The receipt's `change` object.
    pub fn to_value(&self) -> Value {
        match self {
            ChangeEvidence::NotMeasured(reason) | ChangeEvidence::Failed(reason) => {
                unmeasured(reason)
            }
            ChangeEvidence::Measured(v) => v.clone(),
        }
    }

    /// Why the change could not be measured for an environment reason.
    /// A `Measured` object that does not have the shape of interface 3 is
    /// such a failure too: it cannot be read, so it cannot be trusted.
    pub fn fault(&self) -> Option<String> {
        match self {
            ChangeEvidence::Failed(reason) => Some(reason.clone()),
            ChangeEvidence::Measured(v) => shape_error(v).or_else(|| measurement_error(v)),
            ChangeEvidence::NotMeasured(_) => None,
        }
    }

    /// What the action table reads. Evidence that is not measured, or
    /// malformed (see [`ChangeEvidence::fault`]), yields nothing to act on.
    pub fn facts(&self) -> Facts {
        let ChangeEvidence::Measured(v) = self else {
            return Facts::default();
        };
        if shape_error(v).is_some() {
            return Facts::default();
        }
        let list = |key: &str| v[key].as_array().map_or(&[][..], Vec::as_slice);
        let text = |t: &Value, key: &str| t[key].as_str().unwrap_or("").to_owned();
        Facts {
            // absent in evidence measured before the list existed: none
            failing: list("failing_tests")
                .iter()
                .map(|t| Failing {
                    id: text(t, "id"),
                    result: text(t, "candidate_result"),
                })
                .collect(),
            redundant: list("redundant_tests")
                .iter()
                .map(|t| Redundant {
                    id: text(t, "id"),
                    reason: text(t, "reason"),
                })
                .collect(),
            unexercised: list("functions")
                .iter()
                .filter(|f| f["status"] == "unexercised")
                .map(function_name)
                .collect(),
        }
    }
}

fn measurement_error(v: &Value) -> Option<String> {
    let unknown_test = v["unknown_tests"].as_array()?.first();
    if let Some(test) = unknown_test {
        return Some(format!("{}: {}", test["id"], test["reason"]));
    }
    // a function is unknown while a test the candidate added fails: that
    // is the proposer's to repair, not something that could not be measured
    let failing = v["failing_tests"].as_array().is_some_and(|t| !t.is_empty());
    let unknown_function = v["functions"]
        .as_array()?
        .iter()
        .any(|f| f["status"] == "unknown");
    if !failing && (v["status"] == "unknown" || unknown_function) {
        return Some(
            v["not_measured"]
                .as_array()?
                .first()
                .and_then(Value::as_str)
                .unwrap_or("change execution could not be established")
                .to_owned(),
        );
    }
    None
}

fn shape_error(v: &Value) -> Option<String> {
    if !v["status"].as_str().is_some_and(|s| STATUSES.contains(&s)) {
        return Some("change evidence has no valid status".to_owned());
    }
    LISTS
        .iter()
        .find(|k| !v[**k].is_array())
        .map(|k| format!("change evidence has no {k} list"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured() -> Value {
        json!({"status": "partly_exercised",
               "functions": [{"file": "src/a.py", "function": "f", "line": 10, "status": "exercised"},
                             {"file": "src/a.py", "function": "g", "line": 20, "status": "unexercised"}],
               "proving_tests": [], "redundant_tests": [{"id": "tests/t.py::test_extra"}],
               "unknown_tests": [], "not_measured": []})
    }

    #[test]
    fn a_failing_added_test_is_not_a_measurement_fault() {
        let c = ChangeEvidence::Measured(json!({"status": "unknown",
            "functions": [{"file": "src/a.py", "function": "f", "line": 1, "status": "unknown"}],
            "proving_tests": [], "redundant_tests": [], "unknown_tests": [],
            "failing_tests": [{"id": "tests/t.py::test_w", "candidate_result": "failed"}],
            "not_measured": ["src/a.py: f: an added test could not be classified"]}));
        assert_eq!(c.fault(), None);
        assert_eq!(c.facts().failing.len(), 1);
    }

    #[test]
    fn measured_evidence_is_read_for_the_action_table() {
        let c = ChangeEvidence::Measured(measured());
        assert_eq!(c.fault(), None);
        assert_eq!(
            c.facts(),
            Facts {
                failing: Vec::new(),
                redundant: vec![Redundant {
                    id: "tests/t.py::test_extra".into(),
                    reason: String::new(),
                }],
                unexercised: vec!["src/a.py:g (line 20)".into()],
            }
        );
    }

    #[test]
    fn not_measured_states_its_reason_and_is_not_a_fault() {
        let c = ChangeEvidence::NotMeasured("not measured yet".into());
        let v = c.to_value();
        assert_eq!(
            (v["status"].as_str(), v["not_measured"][0].as_str()),
            (Some("unknown"), Some("not measured yet"))
        );
        assert_eq!((c.fault(), c.facts()), (None, Facts::default()));
    }

    #[test]
    fn a_failed_or_malformed_measurement_is_a_fault() {
        assert_eq!(
            ChangeEvidence::Failed("no coverage".into())
                .fault()
                .as_deref(),
            Some("no coverage")
        );
        for edit in [
            |v: &mut Value| v["status"] = json!("great"),
            |v: &mut Value| v["redundant_tests"] = json!(null),
            |v: &mut Value| {
                v.as_object_mut().map(|m| m.remove("functions"));
            },
        ] {
            let mut v = measured();
            edit(&mut v);
            assert!(ChangeEvidence::Measured(v).fault().is_some());
        }
    }
}
