//! Regression evidence: did the behaviour the baseline's tests establish
//! survive in the candidate? The controller restores the baseline version
//! of every trusted test before running them, so a test edited to hide a
//! bug is run as it was; this module reads the controller's decision and
//! the check's recorded output into the receipt's `regression` object.

use serde_json::{Value, json};

use crate::pytest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Held,
    Broken,
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Held => "held",
            Status::Broken => "broken",
            Status::Unknown => "unknown",
        }
    }
}

/// How one restored (baseline) test file did on the candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restored {
    Passes,
    Fails,
    Unknown,
}

impl Restored {
    pub fn as_str(self) -> &'static str {
        match self {
            Restored::Passes => "passes",
            Restored::Fails => "fails",
            Restored::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Regression {
    pub status: Status,
    /// Tests that ran to a result; `None` when the output did not say.
    pub tests_run: Option<u64>,
    pub failed: Vec<String>,
    pub missing: Vec<String>,
    pub reasons: Vec<String>,
    /// Whether `failed` and `missing` name every test that did not pass.
    /// Without that, a file with no failure listed is not known to pass.
    pub complete: bool,
}

impl Regression {
    pub fn unknown(reasons: Vec<String>) -> Regression {
        Regression {
            status: Status::Unknown,
            tests_run: None,
            failed: Vec::new(),
            missing: Vec::new(),
            reasons,
            complete: false,
        }
    }

    pub fn to_value(&self) -> Value {
        json!({"status": self.status.as_str(), "tests_run": self.tests_run,
               "failed": self.failed, "missing": self.missing, "reasons": self.reasons})
    }

    /// Whether the baseline version of `path` passes on the candidate.
    pub fn restored_result(&self, path: &str) -> Restored {
        let named = self
            .failed
            .iter()
            .chain(&self.missing)
            .any(|id| pytest::file_of(id) == path);
        match (named, self.complete) {
            (true, _) => Restored::Fails,
            (false, true) => Restored::Passes,
            (false, false) => Restored::Unknown,
        }
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

/// Why the test command's own exit status says nothing about the tests: it
/// never started, or pytest reports it found nothing to run, could not
/// read its command line, or failed inside itself.
fn not_about_the_tests(code: Option<i64>, pytest: bool) -> Option<String> {
    match code {
        None => Some("the test command did not exit normally".to_owned()),
        Some(c @ (126 | 127)) => Some(format!("the test command could not be started (exit {c})")),
        Some(5) if pytest => Some("pytest found no tests to run".to_owned()),
        Some(4) if pytest => Some("pytest did not understand its command line".to_owned()),
        Some(3) if pytest => Some("pytest failed internally".to_owned()),
        Some(_) => None,
    }
}

fn all_verified(decision: &Value) -> bool {
    decision["requirements"]
        .as_array()
        .is_some_and(|r| !r.is_empty() && r.iter().all(|x| x["state"] == "verified"))
}

fn held(decision: &Value, output: &str) -> Regression {
    let ran = pytest::summary(output).map(|s| s.ran());
    let inventory = &decision["gates"]["verification"]["inventory"];
    if ran == Some(0) {
        return Regression::unknown(vec![
            "the test command succeeded without running a test".to_owned(),
        ]);
    }
    if inventory["state"] == "pass" && inventory["baseline"].as_u64() == Some(0) {
        return Regression::unknown(vec!["the baseline has no tests to hold".to_owned()]);
    }
    Regression {
        status: Status::Held,
        tests_run: ran,
        failed: Vec::new(),
        missing: Vec::new(),
        reasons: Vec::new(),
        complete: true,
    }
}

fn rejected(decision: &Value, check: Option<&Value>, output: &str, pytest: bool) -> Regression {
    let reasons = strings(&decision["reasons"]);
    let code = check.and_then(|c| c["exit_code"].as_i64());
    let tests_failed = !all_verified(decision);
    if let Some(why) = tests_failed
        .then(|| not_about_the_tests(code, pytest))
        .flatten()
    {
        let mut all = vec![why];
        all.extend(reasons);
        return Regression::unknown(all);
    }
    let missing = strings(&decision["gates"]["verification"]["inventory"]["missing"]);
    let failed = if tests_failed {
        pytest::failures(output)
    } else {
        Vec::new()
    };
    let complete = !tests_failed
        || pytest::summary(output)
            .is_some_and(|s| s.failed + s.errors == failed.len() as u64 && !failed.is_empty());
    Regression {
        status: Status::Broken,
        tests_run: pytest::summary(output).map(|s| s.ran()),
        failed,
        missing,
        reasons,
        complete,
    }
}

/// The receipt's `regression` from the controller's decision, the recorded
/// run of the test check (`None` when it left none) and whether the test
/// command is pytest.
pub fn from_decision(decision: &Value, check: Option<&Value>, pytest: bool) -> Regression {
    let output = check
        .and_then(|c| c["stdout_tail"].as_str())
        .unwrap_or_default();
    match decision["decision"].as_str() {
        Some("accepted" | "needs_review") => held(decision, output),
        Some("rejected") => rejected(decision, check, output, pytest),
        _ => Regression::unknown(strings(&decision["reasons"])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(kind: &str, r1: &str, extra: Value) -> Value {
        let mut d = json!({"decision": kind, "reasons": ["why"],
            "requirements": [{"id": "R1", "state": r1}],
            "gates": {"verification": {"inventory": {"state": "pass", "baseline": 5, "candidate": 5, "missing": []}}}});
        if let (Some(m), Some(e)) = (d.as_object_mut(), extra.as_object()) {
            m.extend(e.clone());
        }
        d
    }

    fn run(code: i64, out: &str) -> Value {
        json!({"exit_code": code, "stdout_tail": out})
    }

    #[test]
    fn accepted_and_needs_review_hold_with_the_count() {
        for kind in ["accepted", "needs_review"] {
            let r = from_decision(
                &decision(kind, "verified", json!({})),
                Some(&run(0, "5 passed in 1s\n")),
                true,
            );
            assert_eq!((r.status, r.tests_run), (Status::Held, Some(5)), "{kind}");
            assert_eq!(r.restored_result("tests/a.py"), Restored::Passes);
        }
    }

    #[test]
    fn success_that_ran_nothing_is_not_a_hold() {
        let d = decision("accepted", "verified", json!({}));
        let r = from_decision(&d, Some(&run(0, "1 skipped in 1s\n")), true);
        assert_eq!(r.status, Status::Unknown);
        let mut empty = decision("accepted", "verified", json!({}));
        empty["gates"]["verification"]["inventory"]["baseline"] = json!(0);
        assert_eq!(
            from_decision(&empty, Some(&run(0, "")), true).status,
            Status::Unknown
        );
    }

    #[test]
    fn failures_are_named_and_files_are_judged_by_them() {
        let out = "..F\nFAILED tests/a.py::test_x - assert\n1 failed, 4 passed in 1s\n";
        let r = from_decision(
            &decision("rejected", "failed", json!({})),
            Some(&run(1, out)),
            true,
        );
        assert_eq!(r.status, Status::Broken);
        assert_eq!(r.failed, ["tests/a.py::test_x"]);
        assert_eq!(r.tests_run, Some(5));
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
        assert_eq!(r.restored_result("tests/b.py"), Restored::Passes);
    }

    #[test]
    fn a_failure_list_that_may_be_cut_leaves_other_files_unknown() {
        // the summary counts 3 failures, only one id survived the tail
        let out = "FAILED tests/a.py::test_x - assert\n3 failed, 4 passed in 1s\n";
        let r = from_decision(
            &decision("rejected", "failed", json!({})),
            Some(&run(1, out)),
            true,
        );
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
        assert_eq!(r.restored_result("tests/b.py"), Restored::Unknown);
        let none = from_decision(
            &decision("rejected", "failed", json!({})),
            Some(&run(1, "noise")),
            true,
        );
        assert_eq!(
            (none.status, none.restored_result("tests/b.py")),
            (Status::Broken, Restored::Unknown)
        );
    }

    #[test]
    fn tests_missing_from_the_inventory_break_the_regression() {
        let mut d = decision("rejected", "verified", json!({}));
        d["gates"]["verification"]["inventory"] = json!({"state": "fail", "baseline": 5, "candidate": 4, "missing": ["tests/a.py::test_gone"]});
        let r = from_decision(&d, Some(&run(0, "4 passed in 1s\n")), true);
        assert_eq!(
            (r.status, r.missing.clone()),
            (Status::Broken, vec!["tests/a.py::test_gone".to_owned()])
        );
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
        assert_eq!(
            r.restored_result("tests/b.py"),
            Restored::Passes,
            "the run itself was clean"
        );
    }

    #[test]
    fn exit_statuses_that_say_nothing_about_the_tests_are_unknown() {
        for (code, pytest, status) in [
            (5, true, Status::Unknown),
            (4, true, Status::Unknown),
            (3, true, Status::Unknown),
            (127, false, Status::Unknown),
            (1, true, Status::Broken),
            (2, true, Status::Broken),
            (5, false, Status::Broken),
        ] {
            let d = decision("rejected", "failed", json!({}));
            let r = from_decision(&d, Some(&run(code, "")), pytest);
            assert_eq!(r.status, status, "exit {code}, pytest {pytest}");
        }
    }

    #[test]
    fn an_incomplete_decision_is_unknown_with_its_reasons() {
        let d = decision(
            "incomplete",
            "unverified",
            json!({"reasons": ["bwrap missing"]}),
        );
        let r = from_decision(&d, None, true);
        assert_eq!(
            (r.status, r.reasons.clone(), r.tests_run),
            (Status::Unknown, vec!["bwrap missing".to_owned()], None)
        );
        assert_eq!(r.restored_result("tests/a.py"), Restored::Unknown);
    }
}
