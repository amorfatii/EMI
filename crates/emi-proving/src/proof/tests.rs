use emi_panel::Lang;

use super::*;
use emi_diff::functions::ChangeKind;

fn changed(span: (u32, u32), added: &[u32]) -> ChangedFn {
    ChangedFn {
        file: "src/a.py".into(),
        name: "f".into(),
        lang: Lang::Python,
        old: Some(span),
        new: Some(span),
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

/// Ten lines of source; the content only matters to the header scan.
const SOURCE: &str = "import os\n\ndef f(x):\n    a = 1\n    if x:\n        return a\n    return 0\n\nVALUE = f(1)\n\n";

#[test]
fn headers_end_after_decorators_and_multiline_signatures() {
    let src: Vec<&str> = vec![
        "@decorator(",            // 1
        "    arg=1,",             // 2
        ")",                      // 3
        "@other",                 // 4
        "def f(",                 // 5
        "    a: dict[str, int],", // 6
        "    b='(',",             // 7
        ") -> None:  # (",        // 8
        "    pass",               // 9
    ];
    assert_eq!(header_end(&src, 1), 8);
    assert_eq!(header_end(&src, 5), 8);
    let plain = ["def g(x):", "    return x"];
    assert_eq!(header_end(&plain, 1), 1);
    let one = ["def g(x): return x"];
    assert_eq!(header_end(&one, 1), 1);
    // text that says nothing about a header leaves the start alone
    assert_eq!(header_end(&[], 3), 3);
}

#[test]
fn a_changed_line_that_ran_proves_the_function() {
    // f is lines 3-7; the change wrote line 6; coverage ran 6
    let f = changed((3, 7), &[6]);
    let cov = lines(&[1, 3, 4, 5, 6, 7, 9], &[1, 3, 4, 5, 6, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&cov)), (FnStatus::Proven, None));
}

#[test]
fn a_changed_line_that_did_not_run_leaves_the_function_unproven() {
    // the function was called, but not the line the change wrote
    let f = changed((3, 7), &[6]);
    let cov = lines(&[1, 3, 4, 5, 6, 7, 9], &[1, 3, 4, 5, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&cov)), (FnStatus::Unproven, None));
}

#[test]
fn defining_the_function_does_not_prove_it() {
    // a new function: every line is added; only the import-time lines ran
    let f = changed((3, 7), &[3, 4, 5, 6, 7]);
    let cov = lines(&[1, 3, 4, 5, 6, 7, 9], &[1, 3, 9]);
    assert_eq!(judge(&f, SOURCE, Some(&cov)), (FnStatus::Unproven, None));
}

#[test]
fn a_change_with_no_executable_added_line_is_judged_by_the_body_running() {
    // only the signature changed: it counts when the body ran
    let f = changed((3, 7), &[3]);
    let ran = lines(&[3, 4, 5, 6, 7], &[3, 4]);
    assert_eq!(judge(&f, SOURCE, Some(&ran)).0, FnStatus::Proven);
    let not_run = lines(&[3, 4, 5, 6, 7], &[3]);
    assert_eq!(judge(&f, SOURCE, Some(&not_run)).0, FnStatus::Unproven);
    // lines only taken out: the same
    let removed_only = changed((3, 7), &[]);
    assert_eq!(judge(&removed_only, SOURCE, Some(&ran)).0, FnStatus::Proven);
}

#[test]
fn a_one_line_function_keeps_its_only_line() {
    let src = "x = 1\nf = lambda: 1\n";
    let f = changed((2, 2), &[2]);
    assert_eq!(
        judge(&f, src, Some(&lines(&[1, 2], &[1, 2]))).0,
        FnStatus::Proven
    );
    assert_eq!(
        judge(&f, src, Some(&lines(&[1, 2], &[1]))).0,
        FnStatus::Unproven
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
