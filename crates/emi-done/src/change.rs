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

/// What the action table reads from the change part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub redundant_tests: usize,
    pub unproven_functions: usize,
    pub first_redundant: Option<String>,
    pub first_unproven: Option<String>,
}

const STATUSES: &[&str] = &["proven", "partly_proven", "unproven", "none", "unknown"];
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
           "redundant_tests": [], "unknown_tests": [], "not_measured": [reason]})
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
            ChangeEvidence::Measured(v) => shape_error(v),
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
        let redundant = v["redundant_tests"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let unproven: Vec<&Value> = v["functions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["status"] == "unproven")
            .collect();
        Facts {
            redundant_tests: redundant.len(),
            unproven_functions: unproven.len(),
            first_redundant: redundant
                .first()
                .map(|t| t["id"].as_str().unwrap_or("?").to_owned()),
            first_unproven: unproven.first().map(|f| function_name(f)),
        }
    }
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
        json!({"status": "partly_proven",
               "functions": [{"file": "src/a.py", "function": "f", "line": 10, "status": "proven"},
                             {"file": "src/a.py", "function": "g", "line": 20, "status": "unproven"}],
               "proving_tests": [], "redundant_tests": [{"id": "tests/t.py::test_extra"}],
               "unknown_tests": [], "not_measured": []})
    }

    #[test]
    fn measured_evidence_is_read_for_the_action_table() {
        let c = ChangeEvidence::Measured(measured());
        assert_eq!(c.fault(), None);
        assert_eq!(
            c.facts(),
            Facts {
                redundant_tests: 1,
                unproven_functions: 1,
                first_redundant: Some("tests/t.py::test_extra".into()),
                first_unproven: Some("src/a.py:g (line 20)".into()),
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
