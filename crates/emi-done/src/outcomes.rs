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

    /// Tests whose outcome differs between two runs of the same code under
    /// the same pins: what happens to them is not decided by the code.
    pub fn unstable(&self, other: &Outcomes) -> BTreeSet<String> {
        let status = |o: &Outcomes, id: &str| (o.failed.contains(id), o.skipped.contains(id));
        self.failed
            .iter()
            .chain(&self.skipped)
            .chain(&other.failed)
            .chain(&other.skipped)
            .filter(|id| status(self, id) != status(other, id))
            .cloned()
            .collect()
    }

    /// Two runs taken together, conservatively: a test that did not pass
    /// in either is not passing.
    pub fn merged(&self, other: &Outcomes) -> Outcomes {
        let failed: BTreeSet<String> = self.failed.union(&other.failed).cloned().collect();
        let skipped: BTreeSet<String> = self
            .skipped
            .union(&other.skipped)
            .filter(|id| !failed.contains(*id))
            .cloned()
            .collect();
        Outcomes {
            passed: self.passed.min(other.passed),
            failed,
            skipped,
        }
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
    fn two_runs_name_their_unstable_tests_and_merge_conservatively() {
        let run = |failed: &[&str], skipped: &[&str]| Outcomes {
            passed: 5,
            failed: failed.iter().map(|s| (*s).to_owned()).collect(),
            skipped: skipped.iter().map(|s| (*s).to_owned()).collect(),
        };
        let a = run(&["t::flaky", "t::always"], &["t::skip"]);
        let b = run(
            &["t::always", "t::sometimes_skips"],
            &["t::skip", "t::flaky"],
        );
        let unstable: Vec<String> = a.unstable(&b).into_iter().collect();
        assert_eq!(unstable, ["t::flaky", "t::sometimes_skips"]);
        let both = a.merged(&b);
        assert!(both.failed.contains("t::flaky") && !both.skipped.contains("t::flaky"));
        assert!(both.not_passing("t::sometimes_skips") && both.not_passing("t::skip"));
        assert!(a.unstable(&a).is_empty());
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
