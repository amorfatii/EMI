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

use crate::outcomes::Outcomes;
use emi_python::report as pytest;

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
    /// Touched test-runner configuration (a `conftest.py`, a
    /// `pyproject.toml`) that ran in its baseline form.
    pub restored_runner: BTreeSet<String>,
}

impl Scope {
    /// Whether the run held any file in another version than the
    /// candidate's: its failures may then come from the mix, not the code.
    pub fn any_restored(&self) -> bool {
        !self.restored.is_empty() || !self.restored_runner.is_empty()
    }
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
    /// Tests the candidate added that fail on it. They are not regression
    /// evidence (nothing old broke); the action table reads them as
    /// failing added tests.
    pub added_failing: Vec<String>,
    /// Tests whose outcome differed between the baseline's two pinned
    /// runs: they are not regression evidence either way.
    pub quarantined: Vec<String>,
    /// Old tests that failed on the candidate and passed when the same run
    /// was repeated under the same pins, with nothing restored: their
    /// outcome is not decided by the code, so they are neither held nor
    /// broken.
    pub nondeterministic: Vec<String>,
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
            added_failing: Vec::new(),
            quarantined: Vec::new(),
            nondeterministic: Vec::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({"status": self.status.as_str(), "tests_run": self.tests_run,
               "failed": self.failed, "missing": self.missing, "reasons": self.reasons,
               "quarantined": self.quarantined, "nondeterministic": self.nondeterministic})
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
    /// The kept end of what the command wrote to its error stream.
    errors: &'a str,
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
            errors: check
                .and_then(|c| c["stderr_tail"].as_str())
                .unwrap_or_default(),
            reliable: check.is_some_and(|c| c["output_truncated"] == false),
            code: check.and_then(|c| c["exit_code"].as_i64()),
        }
    }
}

/// Why there is no result to read: the run's last error line, when it left
/// one (an import that failed, a plugin that would not load), without the
/// scratch directory the run happened in, which differs from run to run.
fn no_summary(errors: &str) -> String {
    let base = "pytest execution has no complete readable result summary";
    let Some(last) = errors.lines().rev().find(|l| !l.trim().is_empty()) else {
        return base.to_owned();
    };
    let stable: String = last
        .split_whitespace()
        .map(|word| match word.find("/emi-verify-") {
            Some(_) => word.rsplit('/').next().unwrap_or(word),
            None => word,
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{base}: {}", stable.chars().take(300).collect::<String>())
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
    if pytest {
        let Some(summary) = summary else {
            return Regression::unknown(vec![no_summary(run.errors)]);
        };
        if summary.skipped != 0 || summary.xfailed != 0 || summary.xpassed != 0 {
            return Regression::unknown(vec!["pytest skipped tests or used expected-failure outcomes; regression execution is not fully established".into()]);
        }
    }
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
    // a restored file whose tests no longer run does not collect on the
    // candidate in its baseline form: like its failures, the person's
    // question, not a break
    let missing: Vec<String> = missing_all
        .iter()
        .filter(|id| !touched(id, &scope.approved) && !touched(id, &scope.restored))
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
        added_failing: Vec::new(),
        quarantined: Vec::new(),
        nondeterministic: Vec::new(),
    };
    verdict(base, decision, ok, scope)
}

/// The candidate's run named no test at all, where the baseline's ran
/// them in the same environment: the change stopped the suite from
/// starting (a module the tests load no longer imports, a plugin that no
/// longer loads). That is a break the proposer repairs, not a measurement
/// that failed. `None` when the run did not end by itself with a failure
/// status, or its output is cut or holds a summary after all.
pub fn not_started(check: Option<&Value>, base: &Outcomes) -> Option<Regression> {
    let run = Run::of(check);
    let ended_failing = matches!(run.code, Some(c) if c != 0 && c != 126 && c != 127);
    if !ended_failing || !run.reliable || base.passed == 0 || pytest::summary(run.output).is_some()
    {
        return None;
    }
    let why = no_summary(run.errors);
    let detail = why
        .split_once(": ")
        .map_or("no error line was kept", |(_, d)| d);
    let mut r = Regression::unknown(vec![format!(
        "the tests ran at the baseline ({} passed) and do not start on the candidate: {detail}",
        base.passed
    )]);
    r.status = Status::Broken;
    Some(r)
}

/// Why the candidate's per-test report cannot be compared as it stands.
enum Unreadable {
    Why(String),
    /// A collection error, or pytest saying it ran nothing: the per-test
    /// lines are not the whole story.
    NotPerTest,
}

fn unreadable(check: Option<&Value>, cand: &Outcomes, inventory: &Value) -> Option<Unreadable> {
    match (Run::of(check).code, cand.failed.is_empty()) {
        (Some(0), true) | (Some(1), false) => {}
        (Some(0 | 1), _) => {
            return Some(Unreadable::Why(
                "the test command's exit status disagrees with its own per-test report".into(),
            ));
        }
        _ => return Some(Unreadable::NotPerTest),
    }
    let collected = inventory["candidate"]
        .as_u64()
        .filter(|n| cand.reported() < *n)?;
    Some(Unreadable::Why(format!(
        "{} of {collected} collected tests have no outcome in the run's own report",
        collected - cand.reported()
    )))
}

/// The receipt's `regression` when both the baseline's run and the
/// candidate's named their tests one by one: a test counts against the
/// candidate only if it did worse than at the baseline. A test the baseline
/// already skipped, or that already failed there, is not regression
/// evidence; a test that passed there and no longer runs is a break, like
/// one that now fails. Tests the candidate added are not regression
/// evidence: one that fails is kept apart (`added_failing`), to be repaired
/// as a failing added test, and one that is skipped is the change
/// evidence's to report.
pub fn from_outcomes(
    decision: &Value,
    check: Option<&Value>,
    (base, cand): (&Outcomes, &Outcomes),
    scope: &Scope,
) -> Regression {
    if !matches!(
        decision["decision"].as_str(),
        Some("accepted" | "needs_review" | "rejected")
    ) {
        return Regression::unknown(strings(&decision["reasons"]));
    }
    let inventory = &decision["gates"]["verification"]["inventory"];
    match unreadable(check, cand, inventory) {
        Some(Unreadable::Why(why)) => return Regression::unknown(vec![why]),
        Some(Unreadable::NotPerTest) => return from_decision(decision, check, true, scope),
        None => {}
    }
    let added: BTreeSet<String> = strings(&inventory["added"]).into_iter().collect();
    let (added_failing, new_failures): (Vec<&String>, Vec<&String>) = cand
        .failed
        .iter()
        .filter(|id| !base.failed.contains(*id))
        .partition(|id| added.contains(*id));
    let stopped: Vec<String> = cand
        .skipped
        .iter()
        .filter(|id| !base.not_passing(id) && !added.contains(*id))
        .cloned()
        .collect();
    let seen: Vec<String> = new_failures
        .into_iter()
        .cloned()
        .chain(stopped.clone())
        .collect();
    let missing_all = strings(&inventory["missing"]);
    let touched = |id: &String, files: &BTreeSet<String>| files.contains(pytest::file_of(id));
    let mut attributed: Vec<String> = seen.iter().cloned().chain(missing_all.clone()).collect();
    attributed.sort();
    attributed.dedup();
    let draft = Regression {
        status: Status::Held,
        tests_run: Some(cand.passed + cand.failed.len() as u64),
        failed: seen
            .into_iter()
            .filter(|id| !touched(id, &scope.restored))
            .collect(),
        missing: missing_all
            .into_iter()
            .filter(|id| !touched(id, &scope.approved) && !touched(id, &scope.restored))
            .collect(),
        reasons: Vec::new(),
        complete: true,
        attributed,
        // with a test file restored, an added test ran beside files it
        // was not written for: only the change evidence, which runs it on
        // the candidate as it is, can say that it fails
        added_failing: if !scope.any_restored() {
            added_failing.into_iter().cloned().collect()
        } else {
            Vec::new()
        },
        quarantined: Vec::new(),
        nondeterministic: Vec::new(),
    };
    let mut r = verdict(draft, decision, all_verified(decision), scope);
    let already_failing = cand.failed.intersection(&base.failed).count();
    let note = match r.status {
        Status::Broken => counted(
            stopped.iter().filter(|id| r.failed.contains(*id)).count(),
            "of them passed at the baseline and did not run on the candidate",
        ),
        Status::Held => counted(
            already_failing,
            "test(s) already failed at the baseline and are not regression evidence",
        ),
        Status::Unknown => None,
    };
    r.reasons.extend(note);
    r
}

/// `r` without the failures in `cleared`, which a second look did not
/// confirm, and with `note` saying what became of them. The status is
/// settled again from what is left.
pub fn cleared(
    mut r: Regression,
    cleared: &BTreeSet<String>,
    note: String,
    decision: &Value,
    scope: &Scope,
) -> Regression {
    r.failed.retain(|id| !cleared.contains(id));
    r.status = Status::Held;
    let mut r = verdict(r, decision, all_verified(decision), scope);
    r.reasons.insert(0, note);
    r
}

/// `r` with the failures in `ids` taken out as nondeterministic: they
/// failed, and passed when the same run was repeated under the same pins.
/// A failure left after that is still a break; without one, nothing can be
/// said to hold or to be broken.
pub fn not_reproduced(
    mut r: Regression,
    ids: &BTreeSet<String>,
    decision: &Value,
    scope: &Scope,
) -> Regression {
    r.failed.retain(|id| !ids.contains(id));
    r.nondeterministic = ids.iter().cloned().collect();
    r.status = Status::Held;
    verdict(r, decision, all_verified(decision), scope)
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
    if let Some(first) = r.nondeterministic.first().cloned() {
        let why = format!(
            "{} old test(s) failed and then passed when the same run was repeated under the \
             same pins, so their outcome is not decided by the code (first: {first})",
            r.nondeterministic.len()
        );
        return unknown(r, &why);
    }
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
            ..Scope::default()
        }
    }

    fn judge(d: &Value, c: &Value, s: &Scope) -> Regression {
        from_decision(d, Some(c), true, s)
    }

    fn outcomes(passed: u64, failed: &[&str], skipped: &[&str]) -> Outcomes {
        let set = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Outcomes {
            passed,
            failed: set(failed),
            skipped: set(skipped),
        }
    }

    fn compare(d: &Value, code: i64, base: &Outcomes, cand: &Outcomes, s: &Scope) -> Regression {
        from_outcomes(d, Some(&run(code, "")), (base, cand), s)
    }

    #[test]
    fn tests_the_baseline_already_skipped_do_not_block_a_hold() {
        // the controller rejects any run with a skip; the comparison explains it
        let base = outcomes(4, &[], &["tests/a.py::test_s"]);
        let r = compare(
            &decision("rejected", "failed"),
            0,
            &base,
            &base.clone(),
            &Scope::default(),
        );
        assert_eq!((r.status, r.tests_run), (Status::Held, Some(4)));
        assert_eq!(r.restored_result("tests/a.py"), Restored::Passes);
    }

    #[test]
    fn a_test_that_passed_at_the_baseline_and_no_longer_runs_is_a_break() {
        let base = outcomes(5, &[], &[]);
        let cand = outcomes(4, &[], &["tests/a.py::test_x"]);
        let r = compare(
            &decision("rejected", "failed"),
            0,
            &base,
            &cand,
            &Scope::default(),
        );
        assert_eq!(
            (r.status, r.failed.clone()),
            (Status::Broken, vec!["tests/a.py::test_x".to_owned()])
        );
        assert!(r.reasons.iter().any(|m| m.contains("did not run")), "{r:?}");
    }

    #[test]
    fn a_failure_the_baseline_already_had_is_noted_and_a_new_one_breaks() {
        let base = outcomes(4, &["tests/a.py::test_old"], &[]);
        let same = compare(
            &decision("rejected", "failed"),
            1,
            &base,
            &base.clone(),
            &Scope::default(),
        );
        assert_eq!(same.status, Status::Held);
        assert!(same.reasons[0].contains("already failed at the baseline"));
        let cand = outcomes(3, &["tests/a.py::test_old", "tests/b.py::test_new"], &[]);
        let worse = compare(
            &decision("rejected", "failed"),
            1,
            &base,
            &cand,
            &Scope::default(),
        );
        assert_eq!(
            (worse.status, worse.failed.clone()),
            (Status::Broken, vec!["tests/b.py::test_new".to_owned()])
        );
    }

    #[test]
    fn failures_a_second_look_did_not_confirm_are_not_a_break() {
        let base = outcomes(5, &[], &[]);
        let cand = outcomes(3, &["tests/a.py::test_x", "tests/b.py::test_y"], &[]);
        let d = decision("rejected", "failed");
        let broken = compare(&d, 1, &base, &cand, &Scope::default());
        assert_eq!(broken.status, Status::Broken);
        let one: BTreeSet<String> = ["tests/a.py::test_x".to_owned()].into();
        let still = cleared(broken.clone(), &one, "note".into(), &d, &Scope::default());
        assert_eq!(
            (
                still.status,
                still.failed.clone(),
                still.reasons[0].as_str()
            ),
            (
                Status::Broken,
                vec!["tests/b.py::test_y".to_owned()],
                "note"
            )
        );
        let both: BTreeSet<String> = cand.failed.clone();
        let held = cleared(broken, &both, "note".into(), &d, &Scope::default());
        assert_eq!(
            (held.status, held.failed.len(), held.reasons.clone()),
            (Status::Held, 0, vec!["note".to_owned()])
        );
    }

    #[test]
    fn a_failure_that_does_not_reproduce_is_neither_held_nor_broken() {
        let base = outcomes(5, &[], &[]);
        let cand = outcomes(3, &["tests/a.py::test_x", "tests/b.py::test_y"], &[]);
        let d = decision("rejected", "failed");
        let broken = compare(&d, 1, &base, &cand, &Scope::default());
        // one failure reproduced: the break stands, the other is named
        let one: BTreeSet<String> = ["tests/a.py::test_x".to_owned()].into();
        let still = not_reproduced(broken.clone(), &one, &d, &Scope::default());
        assert_eq!(still.status, Status::Broken);
        assert_eq!(still.failed, ["tests/b.py::test_y"]);
        assert_eq!(still.nondeterministic, ["tests/a.py::test_x"]);
        // none reproduced: nothing can be said, and the reason says why
        let both: BTreeSet<String> = cand.failed.clone();
        let unknown = not_reproduced(broken, &both, &d, &Scope::default());
        assert_eq!(unknown.status, Status::Unknown);
        assert!(unknown.failed.is_empty());
        assert!(
            unknown.reasons[0].contains("not decided by the code"),
            "{:?}",
            unknown.reasons
        );
        assert_eq!(
            unknown.to_value()["nondeterministic"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn an_added_test_beside_restored_files_is_left_to_the_change_evidence() {
        let base = outcomes(5, &[], &[]);
        let mut d = decision("rejected", "failed");
        d["gates"]["verification"]["inventory"] = json!({"state": "pass", "baseline": 5,
            "candidate": 6, "missing": [], "added": ["tests/n.py::test_new"]});
        let failing = outcomes(5, &["tests/n.py::test_new"], &[]);
        let r = compare(&d, 1, &base, &failing, &scope(&["tests/t.py"], &[]));
        assert_eq!((r.status, r.added_failing.len()), (Status::Held, 0));
        let helper = Scope {
            restored_runner: ["tests/conftest.py".to_owned()].into(),
            ..Scope::default()
        };
        let r = compare(&d, 1, &base, &failing, &helper);
        assert_eq!((r.status, r.added_failing.len()), (Status::Held, 0));
    }

    #[test]
    fn a_suite_that_ran_at_the_baseline_and_does_not_start_is_broken() {
        let base = outcomes(5, &[], &[]);
        let died = json!({"exit_code": 4, "output_truncated": false, "stdout_tail": "",
            "stderr_tail": "Traceback\nImportError: cannot import name 'f' from 'pkg'\n"});
        let r = not_started(Some(&died), &base);
        assert_eq!(
            r.as_ref().map(|r| (r.status, r.reasons.clone())),
            Some((
                Status::Broken,
                vec!["the tests ran at the baseline (5 passed) and do not start on the candidate: ImportError: cannot import name 'f' from 'pkg'".to_owned()]
            ))
        );
        // not when the run was killed, could not be started, was cut, had
        // a summary after all, or the baseline ran nothing either
        let mut cut = died.clone();
        cut["output_truncated"] = json!(true);
        let mut summed = died.clone();
        summed["stdout_tail"] = json!("1 failed, 4 passed in 1s\n");
        let code = |c: Value| {
            let mut v = died.clone();
            v["exit_code"] = c;
            v
        };
        for other in [
            cut,
            summed,
            code(json!(null)),
            code(json!(127)),
            code(json!(0)),
        ] {
            assert_eq!(not_started(Some(&other), &base), None, "{other}");
        }
        assert_eq!(not_started(Some(&died), &outcomes(0, &[], &[])), None);
        assert_eq!(not_started(None, &base), None);
    }

    #[test]
    fn an_added_test_that_is_skipped_is_not_a_regression_and_one_that_fails_is() {
        let base = outcomes(5, &[], &[]);
        let mut d = decision("rejected", "failed");
        d["gates"]["verification"]["inventory"] = json!({"state": "pass", "baseline": 5,
            "candidate": 6, "missing": [], "added": ["tests/n.py::test_new"]});
        let skipped = outcomes(5, &[], &["tests/n.py::test_new"]);
        assert_eq!(
            compare(&d, 0, &base, &skipped, &Scope::default()).status,
            Status::Held
        );
        // nothing old broke: the failing added test is kept apart
        let failing = outcomes(5, &["tests/n.py::test_new"], &[]);
        let r = compare(&d, 1, &base, &failing, &Scope::default());
        assert_eq!(
            (r.status, r.failed.len(), r.added_failing.clone()),
            (Status::Held, 0, vec!["tests/n.py::test_new".to_owned()])
        );
    }

    #[test]
    fn a_report_that_disagrees_with_the_exit_or_the_collection_is_unknown() {
        let base = outcomes(5, &[], &[]);
        let d = decision("rejected", "failed");
        let failing = outcomes(4, &["tests/a.py::test_x"], &[]);
        for (code, cand) in [(0, &failing), (1, &base)] {
            assert_eq!(
                compare(&d, code, &base, cand, &Scope::default()).status,
                Status::Unknown,
                "exit {code}"
            );
        }
        // five collected, three reported: an early exit after collection
        let short = outcomes(3, &[], &[]);
        let r = compare(
            &decision("accepted", "verified"),
            0,
            &base,
            &short,
            &Scope::default(),
        );
        assert_eq!(r.status, Status::Unknown);
        assert!(r.reasons[0].contains("2 of 5"), "{r:?}");
    }

    #[test]
    fn a_new_failure_in_a_restored_file_is_still_the_persons_question() {
        let base = outcomes(5, &[], &[]);
        let cand = outcomes(4, &["tests/a.py::test_x"], &[]);
        let r = compare(
            &decision("rejected", "failed"),
            1,
            &base,
            &cand,
            &scope(&["tests/a.py"], &[]),
        );
        assert_eq!(r.status, Status::Held);
        assert_eq!(r.restored_result("tests/a.py"), Restored::Fails);
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
    fn collected_tests_do_not_prove_complete_execution() {
        let d = decision("accepted", "verified");
        for out in [
            "",
            "1 passed, 4 skipped in 1s\n",
            "1 passed, 4 xfailed in 1s\n",
            "1 passed, 4 xpassed in 1s\n",
        ] {
            assert_eq!(
                judge(&d, &run(0, out), &Scope::default()).status,
                Status::Unknown,
                "{out}"
            );
        }
        let mut cut = run(0, "5 passed in 1s\n");
        cut["output_truncated"] = json!(true);
        assert_eq!(judge(&d, &cut, &Scope::default()).status, Status::Unknown);
    }

    #[test]
    fn a_run_without_a_summary_says_what_the_run_last_complained_of() {
        let mut check = run(1, "");
        check["stderr_tail"] = json!(
            "Traceback\n  File \"/tmp/emi-verify-12-34/run-0/src/a.py\", line 3\nImportError: no module named x\n"
        );
        let r = judge(&decision("rejected", "failed"), &check, &Scope::default());
        assert_eq!(r.status, Status::Unknown);
        assert!(
            r.reasons[0].ends_with("ImportError: no module named x"),
            "{r:?}"
        );
        // a line that names the scratch directory keeps only the file
        assert_eq!(
            no_summary("x /tmp/emi-verify-1-2/run-0/src/a.py y"),
            "pytest execution has no complete readable result summary: x a.py y"
        );
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
        // the baseline form of an edited file does not collect on the
        // candidate: the person's question, as its failures are
        let r = judge(
            &d,
            &run(0, "4 passed in 1s\n"),
            &scope(&["tests/a.py"], &[]),
        );
        assert_eq!(
            (r.status, r.restored_result("tests/a.py")),
            (Status::Held, Restored::Fails)
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
