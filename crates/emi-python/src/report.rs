//! Reading what pytest prints at the end of a run: the summary line's
//! counts and the ids its short summary names as failed. The one reader of
//! that text: the check's evidence and the regression's both use it.

/// Counts from pytest's last line (`2 failed, 210 passed in 3.1s`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub passed: u64,
    pub failed: u64,
    pub errors: u64,
    pub xfailed: u64,
    pub xpassed: u64,
    pub skipped: u64,
}

impl Summary {
    /// Tests that ran to a result, skipped and deselected ones excluded.
    pub fn ran(&self) -> u64 {
        self.passed + self.failed + self.errors + self.xfailed + self.xpassed
    }
}

/// Words pytest uses in the summary line that count nothing we report.
const IGNORED: &[&str] = &[
    "skipped",
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
        "xfailed" => summary.xfailed = n,
        "xpassed" => summary.xpassed = n,
        "skipped" => summary.skipped = n,
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

/// The summary pytest prints last, if the output holds one.
pub fn summary(output: &str) -> Option<Summary> {
    output.lines().rev().find_map(summary_line)
}

/// Ids named on `FAILED` and `ERROR` lines of the short test summary,
/// sorted and without repeats. A collection error names a file, not a test.
pub fn failures(output: &str) -> Vec<String> {
    let mut ids: Vec<String> = output
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
pub fn id_of(rest: &str) -> &str {
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

pub use crate::proving::pytest::file_of;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_line_is_read_in_pytests_forms() {
        let cases = [
            ("212 passed in 3.10s", 212),
            ("2 failed, 210 passed, 1 skipped in 3.1s", 212),
            ("=== 3 passed, 1 warning in 0.05s ===", 3),
            ("1 failed, 1 error in 0.1s (0:00:00)", 2),
            ("2 passed, 1 xfailed, 1 xpassed in 1.00s", 4),
        ];
        for (line, ran) in cases {
            let text = format!("....\n{line}\n");
            assert_eq!(summary(&text).map(|s| s.ran()), Some(ran), "{line}");
        }
        assert_eq!(summary("no tests ran in 0.01s\n").map(|s| s.ran()), Some(0));
        assert_eq!(summary("1 skipped in 0.01s\n").map(|s| s.ran()), Some(0));
    }

    #[test]
    fn text_that_is_not_a_summary_gives_none() {
        for text in ["", "ok in time\n", "3 mysteries in 1s\n", "Traceback\n"] {
            assert_eq!(summary(text), None, "{text:?}");
        }
    }

    #[test]
    fn failed_and_error_lines_give_sorted_ids() {
        let text = "FAILED tests/b.py::test_y[1 - 2] - assert 1\n\
                    FAILED tests/a.py::test_x - AssertionError\n\
                    ERROR tests/c.py - ImportError: no\n\
                    FAILED tests/a.py::test_x - again\n\
                    PASSED tests/a.py::test_z\n";
        assert_eq!(
            failures(text),
            [
                "tests/a.py::test_x",
                "tests/b.py::test_y[1 - 2]",
                "tests/c.py"
            ]
        );
        assert_eq!(file_of("tests/a.py::test_x[1]"), "tests/a.py");
        assert_eq!(file_of("tests/c.py"), "tests/c.py");
    }
}
