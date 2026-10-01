//! Regression evidence: did the behaviour the baseline's tests establish
//! survive in the candidate? The evidence is the old tests and the
//! test-runner configuration the candidate did not touch, plus the
//! candidate's version of any touched file a person approved.
//!
//! A touched old test file is run in its baseline form (the controller
//! restores it). If that form fails, the failure is a question for the
//! person, not a broken regression: the entry says `fails` and waits for an
//! approval. This module reads the controller's decision and the check's
//! recorded output into the receipt's `regression` object, attributing each
//! failed test to its file to tell the two apart.

use std::collections::BTreeSet;

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

/// Which files the run treated as what.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Touched old test files that ran in their baseline form.
    pub restored: BTreeSet<String>,
    /// Touched files a person approved: the candidate's version ran.
    pub approved: BTreeSet<String>,
    /// Added test-runner configuration nobody approved: it ran as well.
    pub added_runner: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Regression {
    pub status: Status,
    /// Tests that ran to a result; `None` when the output did not say.
    pub tests_run: Option<u64>,
    /// Tests of the regression evidence that failed.
    pub failed: Vec<String>,
    /// Baseline tests missing from the candidate's run, other than those of
    /// files a person approved changing.
    pub missing: Vec<String>,
    pub reasons: Vec<String>,
    /// Whether `attributed` names every test that did not pass. Without
    /// that, a file with no failure listed is not known to pass.
    pub complete: bool,
    /// Every failed or missing test id the run showed, restored files'
    /// included.
    pub attributed: Vec<String>,
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
            attributed: Vec::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({"status": self.status.as_str(), "tests_run": self.tests_run,
               "failed": self.failed, "missing": self.missing, "reasons": self.reasons})
    }

    /// Whether the baseline version of the test file `path` passes on the
    /// candidate.
    pub fn restored_result(&self, path: &str) -> Restored {
        let named = self.attributed.iter().any(|id| pytest::file_of(id) == path);
        match (named, self.complete) {
            (true, _) => Restored::Fails,
            (false, true) => Restored::Passes,
            (false, false) => Restored::Unknown,
        }
    }

    /// Whether the run was clean everywhere: the most that can be said for
    /// the baseline form of a configuration file, whose effect no single
    /// test is attributed to.
    pub fn clean(&self) -> Restored {
        if self.status == Status::Held && self.complete && self.attributed.is_empty() {
            Restored::Passes
        } else {
            Restored::Unknown
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

/// What the check printed and how far to trust it.
struct Run<'a> {
    output: &'a str,
    /// The kept output is the whole of it: the summary at its end is the
    /// real one.
    reliable: bool,
    code: Option<i64>,
}

impl<'a> Run<'a> {
    fn of(check: Option<&'a Value>) -> Run<'a> {
        Run {
            output: check
                .and_then(|c| c["stdout_tail"].as_str())
                .unwrap_or_default(),
            reliable: check.is_some_and(|c| c["output_truncated"] == false),
            code: check.and_then(|c| c["exit_code"].as_i64()),
        }
    }
}

fn counted(n: usize, what: &str) -> Option<String> {
    (n > 0).then(|| format!("{n} {what}"))
}

/// The receipt's `regression` from the controller's decision, the recorded
/// run of the test check (`None` when it left none), whether the test
/// command is pytest, and which files the run treated as what.
pub fn from_decision(
    decision: &Value,
    check: Option<&Value>,
    pytest: bool,
    scope: &Scope,
) -> Regression {
    if !matches!(
        decision["decision"].as_str(),
        Some("accepted" | "needs_review" | "rejected")
    ) {
        return Regression::unknown(strings(&decision["reasons"]));
    }
    let run = Run::of(check);
    let ok = all_verified(decision);
    if let Some(why) = (!ok)
        .then(|| not_about_the_tests(run.code, pytest))
        .flatten()
    {
        let mut all = vec![why];
        all.extend(strings(&decision["reasons"]));
        return Regression::unknown(all);
    }
    let summary = run.reliable.then(|| pytest::summary(run.output)).flatten();
    let seen = if ok {
        Vec::new()
    } else {
        pytest::failures(run.output)
    };
    let inventory = &decision["gates"]["verification"]["inventory"];
    let missing_all = strings(&inventory["missing"]);
    let complete =
        ok || summary.is_some_and(|s| !seen.is_empty() && s.failed + s.errors == seen.len() as u64);
    let touched = |id: &String, files: &BTreeSet<String>| files.contains(pytest::file_of(id));
    let failed: Vec<String> = seen
        .iter()
        .filter(|id| !touched(id, &scope.restored))
        .cloned()
        .collect();
    let missing: Vec<String> = missing_all
        .iter()
        .filter(|id| !touched(id, &scope.approved))
        .cloned()
        .collect();
    let mut attributed: Vec<String> = seen.into_iter().chain(missing_all).collect();
    attributed.sort();
    attributed.dedup();
    let base = Regression {
        status: Status::Held,
        tests_run: summary.map(|s| s.ran()),
        failed,
        missing,
        reasons: Vec::new(),
        complete,
        attributed,
    };
    verdict(base, decision, ok, scope)
}

/// Settle the status from what was attributed.
fn verdict(mut r: Regression, decision: &Value, ok: bool, scope: &Scope) -> Regression {
    let inventory_state = decision["gates"]["verification"]["inventory"]["state"].as_str();
    if !r.failed.is_empty() || !r.missing.is_empty() {
        r.status = Status::Broken;
        r.reasons = [
            counted(
                r.failed.len(),
                "test(s) failed among those the candidate did not change",
            ),
            counted(
                r.missing.len(),
                "baseline test(s) are missing from the candidate's run",
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        return r;
    }
    let unknown = |mut r: Regression, why: &str| {
        r.status = Status::Unknown;
        r.reasons = vec![why.to_owned()];
        r
    };
    if !r.complete {
        return unknown(
            r,
            "the failed tests could not be attributed to files (the failure list may be cut), so \
             nothing can be said to hold",
        );
    }
    match inventory_state {
        Some("unverified") => return unknown(r, "the test inventory could not be taken"),
        Some("nothing_to_compare") => return unknown(r, "the baseline lists no tests to hold"),
        _ => {}
    }
    if r.tests_run == Some(0) {
        return unknown(r, "the test command ran no test");
    }
    if decision["decision"] == "rejected" && ok && inventory_state != Some("fail") {
        return unknown(
            r,
            "the controller rejected the candidate for a reason this check does not read",
        );
    }
    r.reasons = scope
        .added_runner
        .iter()
        .map(|p| format!("ran with added test-runner configuration nobody approved yet: {p}"))
        .collect();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(kind: &str, r1: &str) -> Value {
        json!({"decision": kind, "reasons": ["why"],
            "requirements": [{"id": "R1", "state": r1}],
            "gates": {"verification": {"inventory": {"state": "pass", "baseline": 5, "candidate": 5, "missing": []}}}})
    }

    fn run(code: i64, out: &str) -> Value {
        json!({"exit_code": code, "stdout_tail": out, "output_truncated": false})
    }

    fn scope(restored: &[&str], approved: &[&str]) -> Scope {
        let set = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Scope {
            restored: set(restored),
            approved: set(approved),
            added_runner: Vec::new(),
        }
    }

    fn judge(d: &Value, c: &Value, s: &Scope) -> Regression {
        from_decision(d, Some(c), true, s)
    }

    const FAIL_A: &str = "..F\nFAILED tests/a.py::test_x - assert\n1 failed, 4 passed in 1s\n";

    #[test]
    fn accepted_and_needs_review_hold_with_the_count() {
        for kind in ["accepted", "needs_review"] {
            let r = judge(
                &decision(kind, "verified"),
                &run(0, "5 passed in 1s\n"),
                &Scope::default(),
            );
            assert_eq!((r.status, r.tests_run), (Status::Held, Some(5)), "{kind}");
            assert_eq!(r.restored_result("tests/a.py"), Restored::Passes);
        }
    }

    #[test]
    fn success_that_ran_nothing_or_compared_nothing_is_not_a_hold() {
        let d = decision("accepted", "verified");
        assert_eq!(
            judge(&d, &run(0, "1 skipped in 1s\n"), &Scope::default()).status,
            Status::Unknown
        );
        for state in ["nothing_to_compare", "unverified"] {
            let mut d = decision("accepted", "verified");
            d["gates"]["verification"]["inventory"]["state"] = json!(state);
            assert_eq!(
                judge(&d, &run(0, "5 passed in 1s\n"), &Scope::default()).status,
                Status::Unknown,
                "{state}"
            );
        }
    }

    #[test]
    fn a_failure_among_untouched_tests_is_broken() {
        let r = judge(
            &decision("rejected", "failed"),
            &run(1, FAIL_A),
            &Scope::default(),
        );
        assert_eq!(
            (r.status, r.failed.clone()),
            (Status::Broken, vec!["tests/a.py::test_x".to_owned()])
        );
        assert_eq!(r.tests_run, Some(5));
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
        assert_eq!(r.restored_result("tests/b.py"), Restored::Passes);
    }

    #[test]
    fn a_failure_in_a_restored_file_is_a_question_not_a_break() {
        let r = judge(
            &decision("rejected", "failed"),
            &run(1, FAIL_A),
            &scope(&["tests/a.py"], &[]),
        );
        assert_eq!(r.status, Status::Held, "the untouched evidence holds");
        assert!(r.failed.is_empty());
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
        assert_eq!(r.restored_result("tests/b.py"), Restored::Passes);
    }

    #[test]
    fn once_approved_the_candidates_version_counts_as_evidence() {
        let r = judge(
            &decision("rejected", "failed"),
            &run(1, FAIL_A),
            &scope(&[], &["tests/a.py"]),
        );
        assert_eq!(
            r.status,
            Status::Broken,
            "the approved file's own failure is a break"
        );
    }

    #[test]
    fn an_unrelated_failure_beside_a_restored_one_is_broken() {
        let out = "FAILED tests/a.py::test_x - e\nFAILED tests/b.py::test_y - e\n2 failed, 3 passed in 1s\n";
        let r = judge(
            &decision("rejected", "failed"),
            &run(1, out),
            &scope(&["tests/a.py"], &[]),
        );
        assert_eq!(
            (r.status, r.failed.clone()),
            (Status::Broken, vec!["tests/b.py::test_y".to_owned()])
        );
    }

    #[test]
    fn failures_that_cannot_be_attributed_are_unknown_never_held() {
        // the summary counts 3 failures, only one id survived the tail
        let out = "FAILED tests/a.py::test_x - e\n3 failed, 4 passed in 1s\n";
        let r = judge(
            &decision("rejected", "failed"),
            &run(1, out),
            &scope(&["tests/a.py"], &[]),
        );
        assert_eq!(r.status, Status::Unknown);
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
        assert_eq!(r.restored_result("tests/b.py"), Restored::Unknown);
        let noise = judge(
            &decision("rejected", "failed"),
            &run(1, "noise"),
            &Scope::default(),
        );
        assert_eq!(noise.status, Status::Unknown);
        let mut cut = run(1, FAIL_A);
        cut["output_truncated"] = json!(true);
        assert_eq!(
            judge(
                &decision("rejected", "failed"),
                &cut,
                &scope(&["tests/a.py"], &[])
            )
            .status,
            Status::Unknown
        );
    }

    #[test]
    fn missing_tests_break_unless_their_file_change_was_approved() {
        let mut d = decision("rejected", "verified");
        d["gates"]["verification"]["inventory"] = json!({"state": "fail", "baseline": 5, "candidate": 4, "missing": ["tests/a.py::test_gone"]});
        let r = judge(&d, &run(0, "4 passed in 1s\n"), &Scope::default());
        assert_eq!(
            (r.status, r.missing.clone()),
            (Status::Broken, vec!["tests/a.py::test_gone".to_owned()])
        );
        let r = judge(
            &d,
            &run(0, "4 passed in 1s\n"),
            &scope(&[], &["tests/a.py"]),
        );
        assert_eq!(
            r.status,
            Status::Held,
            "the person approved that file as the candidate left it"
        );
    }

    #[test]
    fn exit_statuses_that_say_nothing_about_the_tests_are_unknown() {
        for (code, pytest, status) in [
            (5, true, Status::Unknown),
            (4, true, Status::Unknown),
            (3, true, Status::Unknown),
            (127, false, Status::Unknown),
            (1, true, Status::Unknown),
            (5, false, Status::Unknown),
        ] {
            let r = from_decision(
                &decision("rejected", "failed"),
                Some(&run(code, "")),
                pytest,
                &Scope::default(),
            );
            assert_eq!(r.status, status, "exit {code}, pytest {pytest}");
        }
    }

    #[test]
    fn an_incomplete_decision_is_unknown_with_its_reasons() {
        let mut d = decision("incomplete", "unverified");
        d["reasons"] = json!(["bwrap missing"]);
        let r = from_decision(&d, None, true, &Scope::default());
        assert_eq!(
            (r.status, r.reasons.clone(), r.tests_run),
            (Status::Unknown, vec!["bwrap missing".to_owned()], None)
        );
        assert_eq!(r.restored_result("tests/a.py"), Restored::Unknown);
    }

    #[test]
    fn an_unapproved_added_runner_file_is_named_in_the_reasons() {
        let s = Scope {
            added_runner: vec!["conftest.py".into()],
            ..Scope::default()
        };
        let r = judge(
            &decision("needs_review", "verified"),
            &run(0, "5 passed in 1s\n"),
            &s,
        );
        assert_eq!(r.status, Status::Held);
        assert!(r.reasons[0].contains("conftest.py"));
    }
}
