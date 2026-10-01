//! Execution evidence for checks that run tests: what the run itself
//! reported, test by test, besides its exit status.
//!
//! A check that declares `"evidence": {"format": "pytest"}` must show its
//! work. Two shapes are read from the run's own stdout:
//!
//! - per-test lines, `<id> <OUTCOME>` where the id holds `::` (what
//!   `pytest -v` prints, and what the `runner_evidence` stand-in prints:
//!   `tests/test_app.py::test_one PASSED`);
//! - otherwise a pytest summary (`3 passed in 0.1s`, with `FAILED` lines
//!   naming the failures), the weaker form: it says how many passed,
//!   never which.
//!
//! A passing exit status with neither is not evidence: code that ends the
//! test process with status 0 before the tests run, or that makes a test
//! skip, would otherwise pass. Skipped and expected-failure outcomes are
//! not passes either: a test that did not run verifies nothing.

use std::collections::BTreeSet;

/// How the run reported its tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// One outcome per test id.
    Lines,
    /// Counts plus failure ids.
    #[default]
    Summary,
}

/// What the run reported about its tests.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub kind: Kind,
    /// Per-test outcomes (lines form). The summary form names only its
    /// failures; passed tests are counted in `summary_passed`.
    pub passed: BTreeSet<String>,
    pub failed: BTreeSet<String>,
    pub errored: BTreeSet<String>,
    pub skipped: BTreeSet<String>,
    /// Tests passed according to the summary line (summary form only).
    pub summary_passed: u64,
    /// Tests failed or errored according to the summary line.
    pub summary_failed: u64,
    /// Tests skipped or expected-failed according to the summary line.
    pub summary_skipped: u64,
}

/// Whether the run's evidence establishes that its tests ran and passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail(String),
}

/// The outcome word of a per-test line, uppercased by the caller.
fn line_outcome(word: &str) -> Option<Outcome> {
    match word {
        "PASSED" | "PASS" => Some(Outcome::Passed),
        "FAILED" | "FAIL" => Some(Outcome::Failed),
        "ERROR" | "ERRORED" => Some(Outcome::Errored),
        "SKIPPED" | "SKIP" | "XFAIL" | "XFAILED" => Some(Outcome::Skipped),
        // an unexpected pass is not a clean pass either: the test did not
        // run to its written expectation
        "XPASS" | "XPASSED" => Some(Outcome::Skipped),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    Errored,
    Skipped,
}

/// One per-test outcome from a line shaped `<id> <OUTCOME> ...`: the id is
/// the first whitespace-separated token and holds `::` (a warning or
/// summary line has none, or a space before it). Returns the id and its
/// outcome, or `None` when the line is not a per-test outcome.
fn parse_line(line: &str) -> Option<(&str, Outcome)> {
    let mut words = line.split_whitespace();
    let id = words.next()?;
    let outcome = words.next()?;
    if !id.contains("::") {
        return None;
    }
    // parametrized ids hold brackets and spaces (`test_x[1 - 2]`); the
    // outcome word still follows the id's closing bracket, so rejoin when
    // the second token opens one
    if outcome.starts_with('[') {
        let rest = line[id.len()..].trim_start();
        let after = rest.find(']')?;
        let word = rest[after + 1..].split_whitespace().next()?;
        return line_outcome(word.to_uppercase().as_str()).map(|o| (id, o));
    }
    line_outcome(outcome.to_uppercase().as_str()).map(|o| (id, o))
}

/// Per-test outcomes in `stdout`, or `None` when it holds no per-test
/// line at all (a collection listing names ids with no outcome).
fn parse_lines(stdout: &str) -> Option<Evidence> {
    let mut ev = Evidence {
        kind: Kind::Lines,
        ..Evidence::default()
    };
    // an id observed both passing and otherwise did not cleanly pass
    let mut bad: BTreeSet<String> = BTreeSet::new();
    for line in stdout.lines() {
        let Some((id, outcome)) = parse_line(line.trim()) else {
            continue;
        };
        let id = id.to_owned();
        match outcome {
            Outcome::Passed => {
                if !bad.contains(&id) {
                    ev.passed.insert(id);
                }
            }
            Outcome::Failed => {
                ev.passed.remove(&id);
                ev.failed.insert(id.clone());
                bad.insert(id);
            }
            Outcome::Errored => {
                ev.passed.remove(&id);
                ev.errored.insert(id.clone());
                bad.insert(id);
            }
            Outcome::Skipped => {
                ev.passed.remove(&id);
                ev.skipped.insert(id.clone());
                bad.insert(id);
            }
        }
    }
    if ev.passed.is_empty()
        && ev.failed.is_empty()
        && ev.errored.is_empty()
        && ev.skipped.is_empty()
    {
        return None;
    }
    Some(ev)
}

/// Counts from a pytest summary line (`2 failed, 210 passed in 3.1s`).
#[derive(Clone, Copy, Debug, Default)]
struct Summary {
    passed: u64,
    failed: u64,
    errors: u64,
    skipped: u64,
}

/// Words in the summary line that count nothing reported here.
const IGNORED: &[&str] = &[
    "deselected",
    "warning",
    "warnings",
    "rerun",
    "subtests passed",
];

fn count(part: &str, summary: &mut Summary) -> Option<()> {
    let (n, word) = part.trim().split_once(' ')?;
    let n: u64 = n.parse().ok()?;
    match word.trim() {
        "passed" => summary.passed = n,
        "failed" => summary.failed = n,
        "error" | "errors" => summary.errors = n,
        "xfailed" => summary.skipped += n,
        "xpassed" => summary.skipped += n,
        "skipped" => summary.skipped += n,
        w if IGNORED.contains(&w) => {}
        _ => return None,
    }
    Some(())
}

fn summary_line(line: &str) -> Option<Summary> {
    let inner = line.trim().trim_matches('=').trim();
    let (counts, _time) = inner.rsplit_once(" in ")?;
    if counts == "no tests ran" {
        return Some(Summary::default());
    }
    let mut summary = Summary::default();
    for part in counts.split(", ") {
        count(part, &mut summary)?;
    }
    Some(summary)
}

/// Ids named on `FAILED` and `ERROR` lines of the short test summary.
fn failure_ids(stdout: &str) -> Vec<String> {
    let mut ids: Vec<String> = stdout
        .lines()
        .filter_map(|l| {
            let rest = l
                .strip_prefix("FAILED ")
                .or_else(|| l.strip_prefix("ERROR "))?;
            let id = id_of(rest);
            (!id.is_empty()).then(|| id.to_owned())
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The id at the start of a summary line: it ends at the first ` - ` that
/// is not inside the brackets of a parametrized test's name.
fn id_of(rest: &str) -> &str {
    let mut depth = 0u32;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 && rest[i..].starts_with(" - ") => return rest[..i].trim(),
            _ => {}
        }
    }
    rest.trim()
}

/// Pytest-summary evidence in `stdout`, or `None` when it holds no
/// summary line. Failures the tail cut off still count through the
/// summary's own numbers, even unnamed.
fn parse_summary(stdout: &str) -> Option<Evidence> {
    let summary = stdout.lines().rev().find_map(summary_line)?;
    let mut ev = Evidence {
        kind: Kind::Summary,
        summary_passed: summary.passed,
        summary_failed: summary.failed + summary.errors,
        summary_skipped: summary.skipped,
        ..Evidence::default()
    };
    for id in failure_ids(stdout) {
        ev.failed.insert(id);
    }
    Some(ev)
}

/// What a check that declares pytest evidence reported: per-test lines
/// when it printed any (the stronger form), else its pytest summary.
/// `Err` names why the output is not test evidence at all.
pub fn parse(stdout: &str) -> Result<Evidence, String> {
    if let Some(ev) = parse_lines(stdout) {
        return Ok(ev);
    }
    if let Some(ev) = parse_summary(stdout) {
        return Ok(ev);
    }
    Err("the run printed no per-test outcome and no pytest summary, so its exit status alone says whether the tests ran".into())
}

/// Whether the evidence establishes passing tests: no failure, no error,
/// no skip, and at least one pass. The summary form judges by its own
/// counts (a cut tail may hide failure ids, never the numbers); the
/// lines form judges by the named outcomes.
pub fn judge(ev: &Evidence) -> Verdict {
    let (failed, errored, skipped, ran) = match ev.kind {
        Kind::Lines => (
            ev.failed.len(),
            ev.errored.len(),
            ev.skipped.len(),
            !ev.passed.is_empty(),
        ),
        Kind::Summary => (
            ev.summary_failed as usize,
            0,
            ev.summary_skipped as usize,
            ev.summary_passed > 0,
        ),
    };
    let mut problems = Vec::new();
    if failed > 0 {
        problems.push(format!("{failed} test(s) failed in the run's own report"));
    }
    if errored > 0 {
        problems.push(format!("{errored} test(s) errored in the run's own report"));
    }
    if skipped > 0 {
        problems.push(format!(
            "{skipped} test(s) skipped or expected-failed instead of passing"
        ));
    }
    if problems.is_empty() {
        if ran {
            return Verdict::Pass;
        }
        problems.push("the run reported no passing test".into());
    }
    Verdict::Fail(problems.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(out: &str) -> Evidence {
        parse(out).unwrap_or_else(|e| panic!("evidence for {out:?}: {e}"))
    }

    #[test]
    fn per_test_lines_are_read_with_their_outcomes() {
        let ev = parsed("tests/test_app.py::test_one PASSED\ntests/test_app.py::test_two PASSED\n");
        assert_eq!(ev.kind, Kind::Lines);
        assert_eq!(judge(&ev), Verdict::Pass);
    }

    #[test]
    fn a_skipped_line_is_not_a_pass() {
        let ev =
            parsed("tests/test_app.py::test_one PASSED\ntests/test_app.py::test_two SKIPPED\n");
        assert!(matches!(judge(&ev), Verdict::Fail(_)));
    }

    #[test]
    fn an_early_exit_with_no_outcome_is_no_evidence() {
        assert!(parse("").is_err());
        let ev = parsed("....\nno tests ran in 0.01s\n");
        assert!(matches!(judge(&ev), Verdict::Fail(_)));
    }

    #[test]
    fn a_pytest_verbose_line_with_progress_reads() {
        let ev = parsed(
            "tests/test_a.py::test_x PASSED [ 33%]\ntests/test_a.py::test_y FAILED [ 66%]\n",
        );
        assert_eq!(ev.kind, Kind::Lines);
        assert!(matches!(judge(&ev), Verdict::Fail(_)));
    }

    #[test]
    fn a_clean_summary_passes_and_a_skipped_one_does_not() {
        let ok = parsed("....\n3 passed in 0.10s\n");
        assert_eq!(ok.kind, Kind::Summary);
        assert_eq!(judge(&ok), Verdict::Pass);
        let skipped = parsed("..s\n2 passed, 1 skipped in 0.10s\n");
        assert!(matches!(judge(&skipped), Verdict::Fail(_)));
    }

    #[test]
    fn collection_listings_alone_are_no_evidence() {
        let out = "tests/test_app.py::test_one\ntests/test_app.py::test_two\n3 tests collected in 0.01s\n";
        assert!(parse(out).is_err());
    }
}
