use super::*;
use crate::functions::ChangeKind;

fn changed(span: (u32, u32), added: &[u32]) -> ChangedFn {
    ChangedFn {
        file: "src/a.py".into(),
        name: "f".into(),
        old: Some(span),
        new: Some(span),
        // `def f(x):` on its own line: the body starts on the next
        new_body: Some(span.0 + 1),
        kind: ChangeKind::Modified,
        added_lines: added.to_vec(),
        removed_lines: Vec::new(),
    }
}

fn lines(executable: &[u32], executed: &[u32]) -> Lines {
    Lines {
        executable: executable.iter().copied().collect(),
        executed: executed.iter().copied().collect(),
    }
}

/// Ten lines of source; only its length matters to the judgment.
const SOURCE: &str = "import os\n\ndef f(x):\n    a = 1\n    if x:\n        return a\n    return 0\n\nVALUE = f(1)\n\n";

#[test]
fn a_changed_line_that_ran_proves_the_function() {
    // f is lines 3-7; the change wrote line 6; coverage ran 6
    let f = changed((3, 7), &[6]);
    let cov = lines(&[1, 3, 4, 5, 6, 7, 9], &[1, 3, 4, 5, 6, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&cov)), (FnStatus::Exercised, None));
}

#[test]
fn a_changed_line_that_did_not_run_leaves_the_function_unproven() {
    // the function was called, but not the line the change wrote
    let f = changed((3, 7), &[6]);
    let cov = lines(&[1, 3, 4, 5, 6, 7, 9], &[1, 3, 4, 5, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&cov)), (FnStatus::Unexercised, None));
}

#[test]
fn defining_the_function_does_not_prove_it() {
    // a new function: every line is added; only the import-time lines ran
    let f = changed((3, 7), &[3, 4, 5, 6, 7]);
    let cov = lines(&[1, 3, 4, 5, 6, 7, 9], &[1, 3, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&cov)), (FnStatus::Unexercised, None));
}

#[test]
fn a_change_with_no_executable_added_line_is_judged_by_the_body_running() {
    // only the signature changed: it counts when the body ran
    let f = changed((3, 7), &[3]);
    let ran = lines(&[3, 4, 5, 6, 7], &[3, 4]);
    assert_eq!(judge(&f, SOURCE, Some(&ran)).0, FnStatus::Exercised);
    let not_run = lines(&[3, 4, 5, 6, 7], &[3]);
    assert_eq!(judge(&f, SOURCE, Some(&not_run)).0, FnStatus::Unexercised);
    // lines only taken out: the same
    let removed_only = changed((3, 7), &[]);
    assert_eq!(
        judge(&removed_only, SOURCE, Some(&ran)).0,
        FnStatus::Exercised
    );
}

#[test]
fn a_one_line_function_keeps_its_only_line() {
    let src = "x = 1\nf = lambda: 1\n";
    let f = changed((2, 2), &[2]);
    assert_eq!(
        judge(&f, src, Some(&lines(&[1, 2], &[1, 2]))).0,
        FnStatus::Exercised
    );
    assert_eq!(
        judge(&f, src, Some(&lines(&[1, 2], &[1]))).0,
        FnStatus::Unexercised
    );
}

#[test]
fn what_coverage_cannot_say_is_unknown_with_a_reason() {
    let f = changed((3, 7), &[6]);
    let (status, why) = judge(&f, SOURCE, None);
    assert_eq!(status, FnStatus::Unknown);
    assert!(why.is_some_and(|w| w.contains("no entry")));
    // a report of another version of the file: lines past its end
    let stale = lines(&[3, 4, 500], &[3]);
    let (status, why) = judge(&f, SOURCE, Some(&stale));
    assert_eq!(status, FnStatus::Unknown);
    assert!(why.is_some_and(|w| w.contains("another version")));
    // a body with nothing executable
    let nothing = lines(&[1, 9], &[1, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&nothing)).0, FnStatus::Unknown);
    // a function with no span in the candidate
    let mut gone = changed((3, 7), &[6]);
    gone.new = None;
    assert_eq!(
        judge(&gone, SOURCE, Some(&lines(&[3], &[3]))).0,
        FnStatus::Unknown
    );
}

/// A guard taken out of a function changes every call of it; a test that
/// runs the function, though not the line added further down, met the change.
#[test]
fn a_function_the_change_removed_lines_from_is_reached_by_running_it() {
    let source = "def f(a):\n    x = 1\n    if a:\n        x = 2\n    return x\n";
    let lines = Lines {
        executable: [2, 3, 4, 5].into(),
        executed: [2, 3, 5].into(),
    };
    // only an added line (4), never run: not reached
    let added = changed((1, 5), &[4]);
    assert_eq!(judge(&added, source, Some(&lines)).0, FnStatus::Unexercised);
    // the same, with a line removed from the function as well: reached
    let mut removed = changed((1, 5), &[4]);
    removed.removed_lines = vec![2];
    assert_eq!(judge(&removed, source, Some(&lines)).0, FnStatus::Exercised);
}
