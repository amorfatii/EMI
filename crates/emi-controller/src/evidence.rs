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

/// The version of this reader, recorded with what it read: a record kept
/// from an older reader may name tests differently and is not comparable.
pub const READER: u64 = 5;

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

/// The words a per-test line ends its test id with.
const OUTCOME_WORDS: &[(&str, Outcome)] = &[
    ("PASSED", Outcome::Passed),
    ("PASS", Outcome::Passed),
    ("FAILED", Outcome::Failed),
    ("FAIL", Outcome::Failed),
    ("ERRORED", Outcome::Errored),
    ("ERROR", Outcome::Errored),
    ("SKIPPED", Outcome::Skipped),
    ("SKIP", Outcome::Skipped),
    ("XFAILED", Outcome::Skipped),
    ("XFAIL", Outcome::Skipped),
    // an unexpected pass is not a clean pass either: the test did not run
    // to its written expectation
    ("XPASSED", Outcome::Skipped),
    ("XPASS", Outcome::Skipped),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    Errored,
    Skipped,
}

/// Where `word` stands alone in `line` at or after `from`: a space before
/// it, and a space or the end of the line after it.
fn standalone(line: &str, word: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(i) = line[at..].find(word) {
        let start = at + i;
        let rest = &line[start + word.len()..];
        if line[..start].ends_with(' ') && (rest.is_empty() || rest.starts_with(' ')) {
            return Some(start);
        }
        at = start + word.len();
    }
    None
}

/// One per-test outcome from a line shaped `<id> <OUTCOME> ...`. The id
/// starts the line, its file path holds no space (a summary line such as
/// `FAILED tests/a.py::t - why` has one before the `::`), and it runs up
/// to the first outcome word that stands alone: a parametrized id may hold
/// spaces (`test_x[a b]`), and a skip reason after the outcome may hold
/// anything. Returns the id and its outcome, or `None` when the line is
/// not a per-test outcome.
fn parse_line(line: &str) -> Option<(&str, Outcome)> {
    let path_end = line.find("::")?;
    if line[..path_end].contains(char::is_whitespace) {
        return None;
    }
    let (at, outcome) = OUTCOME_WORDS
        .iter()
        .filter_map(|(word, outcome)| standalone(line, word, path_end).map(|at| (at, *outcome)))
        .min_by_key(|(at, _)| *at)?;
    Some((line[..at].trim_end(), outcome))
}

/// The file named by a short-summary line for a test file that could not
/// be collected (`ERROR tests/test_a.py - ImportError: ...`): none of its
/// tests ran. A test's own error line names an id with `::` and is read
/// from its outcome line instead.
fn collection_error(line: &str) -> Option<&str> {
    let file = id_of(line.strip_prefix("ERROR ")?);
    (!file.contains("::") && !file.contains(char::is_whitespace) && file.ends_with(".py"))
        .then_some(file)
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
        if let Some(file) = collection_error(line.trim()) {
            ev.errored.insert(file.to_owned());
            continue;
        }
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

/// The exact listing a reporter inside the runner printed at the end:
/// `EMI-OUTCOME <outcome> <id>` per test, closed by `EMI-OUTCOMES <count>`.
/// `None` when the output holds no such listing; `Err` when it holds one
/// that is cut short or does not add up, which is no evidence at all.
fn parse_listing(stdout: &str) -> Option<Result<Evidence, String>> {
    let mut ev = Evidence {
        kind: Kind::Lines,
        ..Evidence::default()
    };
    let (mut rows, mut closed) = (0usize, None);
    for line in stdout.lines() {
        if let Some(count) = line.strip_prefix("EMI-OUTCOMES ") {
            closed = count.trim().parse::<usize>().ok();
        } else if let Some(row) = line.strip_prefix("EMI-OUTCOME ") {
            let (outcome, id) = row.split_once(' ')?;
            rows += 1;
            match outcome {
                "passed" => ev.passed.insert(id.to_owned()),
                "failed" => ev.failed.insert(id.to_owned()),
                "error" => ev.errored.insert(id.to_owned()),
                "skipped" | "xfailed" | "xpassed" => ev.skipped.insert(id.to_owned()),
                _ => {
                    return Some(Err(format!(
                        "the run's outcome listing has an outcome this reader does not know: {outcome}"
                    )));
                }
            };
        }
    }
    match (rows, closed) {
        (0, None) => None,
        (n, Some(count)) if n == count => Some(Ok(ev)),
        _ => Some(Err(
            "the run's outcome listing is cut short or does not add up to its own count".into(),
        )),
    }
}

/// What a check that declares pytest evidence reported: the exact listing
/// of a reporter when there is one, else per-test lines when it printed
/// any, else its pytest summary. `Err` names why the output is not test
/// evidence at all.
pub fn parse(stdout: &str) -> Result<Evidence, String> {
    if let Some(listing) = parse_listing(stdout) {
        return listing;
    }
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
    fn an_id_with_spaces_is_read_whole_and_a_skip_reason_is_not_part_of_it() {
        let ev = parsed(
            "tests/t.py::test_p[a b] PASSED\ntests/t.py::test_p[a c] PASSED\ntests/t.py::test_s SKIPPED (FAILED before, see PASSED)\ntests/t.py::test_f[x - y] FAILED [ 50%]\n",
        );
        assert_eq!(ev.passed.len(), 2, "{ev:?}");
        assert!(ev.skipped.contains("tests/t.py::test_s"), "{ev:?}");
        assert!(ev.failed.contains("tests/t.py::test_f[x - y]"), "{ev:?}");
    }

    #[test]
    fn a_short_summary_line_is_not_a_per_test_outcome() {
        assert!(parse_line("FAILED tests/t.py::test_x - assert 1 == 2").is_none());
        assert!(parse_line("tests/t.py::test_x").is_none());
    }

    #[test]
    fn a_file_that_could_not_be_collected_is_an_error_of_that_file() {
        let ev = parsed(
            "tests/test_a.py::test_x PASSED\nERROR tests/test_b.py - ImportError: no module\nERROR tests/test_a.py::test_y - boom\n",
        );
        assert_eq!(ev.kind, Kind::Lines);
        assert_eq!(ev.errored.iter().collect::<Vec<_>>(), ["tests/test_b.py"]);
        assert!(matches!(judge(&ev), Verdict::Fail(_)));
    }

    #[test]
    fn a_reporters_listing_is_read_exactly_and_must_be_whole() {
        let out = "noise tests/t.py::test_a FAILED\n\nEMI-OUTCOME passed tests/t.py::test_a\nEMI-OUTCOME skipped tests/t.py::test_p[a b]\nEMI-OUTCOME error tests/broken.py\nEMI-OUTCOMES 3\n";
        let ev = parsed(out);
        assert_eq!(ev.kind, Kind::Lines);
        // the listing wins over anything printed while the tests ran
        assert!(ev.failed.is_empty(), "{ev:?}");
        assert!(ev.passed.contains("tests/t.py::test_a"));
        assert!(ev.skipped.contains("tests/t.py::test_p[a b]"));
        assert!(ev.errored.contains("tests/broken.py"));
        // cut before its closing count, or a count that disagrees: nothing
        assert!(parse("EMI-OUTCOME passed tests/t.py::test_a\n").is_err());
        assert!(parse("EMI-OUTCOME passed tests/t.py::test_a\nEMI-OUTCOMES 2\n").is_err());
        // an empty run is a listing of nothing, which establishes nothing
        let none = parsed("EMI-OUTCOMES 0\n");
        assert!(matches!(judge(&none), Verdict::Fail(_)));
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
