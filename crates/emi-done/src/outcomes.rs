//! Per-test outcomes of the test check, as the controller recorded them
//! from the run's own report: which tests it named as failing and which as
//! not run (skipped, expected failures). Two of these, the baseline's and
//! the candidate's, are what regression evidence compares.

use std::collections::BTreeSet;

use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcomes {
    pub passed: u64,
    /// Failed or errored.
    pub failed: BTreeSet<String>,
    /// Skipped, or run against a written expectation of failure.
    pub skipped: BTreeSet<String>,
}

fn ids(v: &Value) -> Option<BTreeSet<String>> {
    v.as_array()?
        .iter()
        .map(|x| x.as_str().map(str::to_owned))
        .collect()
}

impl Outcomes {
    /// The outcomes in a recorded check run, or `None` when the run's
    /// report did not name its tests one by one (or was cut).
    pub fn of(check: Option<&Value>) -> Option<Outcomes> {
        let o = &check?["evidence"]["outcomes"];
        Some(Outcomes {
            passed: o["passed"].as_u64()?,
            failed: ids(&o["failed"])?,
            skipped: ids(&o["skipped"])?,
        })
    }

    /// Tests the report gave an outcome for.
    pub fn reported(&self) -> u64 {
        self.passed + (self.failed.len() + self.skipped.len()) as u64
    }

    /// Whether `id` did not pass in this run.
    pub fn not_passing(&self, id: &str) -> bool {
        self.failed.contains(id) || self.skipped.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outcomes_are_read_from_the_recorded_evidence() {
        let check = json!({"evidence": {"outcomes":
            {"passed": 3, "failed": ["t.py::a"], "skipped": ["t.py::b", "t.py::c"]}}});
        let o = Outcomes::of(Some(&check)).unwrap_or_default();
        assert_eq!((o.passed, o.reported()), (3, 6));
        assert!(o.not_passing("t.py::a") && o.not_passing("t.py::c") && !o.not_passing("t.py::z"));
    }

    #[test]
    fn a_run_without_per_test_outcomes_has_none() {
        for check in [
            json!({}),
            json!({"evidence": null}),
            json!({"evidence": {"outcomes": null}}),
            json!({"evidence": {"outcomes": {"passed": 1, "failed": [3], "skipped": []}}}),
        ] {
            assert_eq!(Outcomes::of(Some(&check)), None, "{check}");
        }
        assert_eq!(Outcomes::of(None), None);
    }
}
