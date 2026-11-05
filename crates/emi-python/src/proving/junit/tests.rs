use super::*;

/// Recorded from pytest 9.1 (`--continue-on-collection-errors`): a file
/// that failed to import, a failing test, parameters (one with `::` and a
/// space), a skip, an expected failure, a fixture that raised and a pass.
const RECORDED: &str = r#"<?xml version="1.0" encoding="utf-8"?><testsuites name="pytest tests"><testsuite name="pytest" errors="2" failures="1" skipped="2" tests="9" time="0.037" timestamp="2026-10-01T16:14:28.606305+02:00" hostname="host"><testcase classname="" name="tests.test_b" time="0.000"><error message="collection failure">ImportError while importing test module
E   ModuleNotFoundError: No module named 'nothere'</error></testcase><testcase classname="tests.test_a.TestK" name="test_m" time="0.000" /><testcase classname="tests.test_a.TestK" name="test_fail" time="0.001"><failure message="assert 2 == 3&#10; +  where 2 = f(1)">E   assert 2 == 3</failure></testcase><testcase classname="tests.test_a" name="test_p[x y]" time="0.000" /><testcase classname="tests.test_a" name="test_p[z.w::q]" time="0.000" /><testcase classname="tests.test_a" name="test_skip" time="0.000"><skipped type="pytest.skip" message="unconditional skip">tests/test_a.py:8: unconditional skip</skipped></testcase><testcase classname="tests.test_a" name="test_xf" time="0.000"><skipped type="pytest.xfail" message="" /></testcase><testcase classname="tests.test_a" name="test_err" time="0.000"><error message="failed on setup with &quot;RuntimeError: x&quot;">RuntimeError: x</error></testcase><testcase classname="tests.test_c" name="test_c" time="0.000" /></testsuite></testsuites>"#;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn seen(exit: RunExit, xml: &str, wanted: &[&str]) -> BTreeMap<String, Observed> {
    observe(&exit, parse(xml), &ids(wanted))
}

fn unknown_with(v: &Observed, text: &str) -> bool {
    matches!(v, Observed::Unknown(why) if why.contains(text))
}

#[test]
fn addresses_follow_what_pytest_writes() {
    let table = [
        ("tests/test_a.py::test_x", ("tests.test_a", "test_x")),
        (
            "tests/test_a.py::TestK::test_m",
            ("tests.test_a.TestK", "test_m"),
        ),
        (
            "tests/test_a.py::Outer::Inner::test_m",
            ("tests.test_a.Outer.Inner", "test_m"),
        ),
        ("test_top.py::test_x", ("test_top", "test_x")),
        ("pkg/sub/test_a.py::test_x", ("pkg.sub.test_a", "test_x")),
        // `::` and `.` inside parameters stay in the name
        (
            "tests/test_a.py::test_p[z.w::q]",
            ("tests.test_a", "test_p[z.w::q]"),
        ),
        (
            "tests/test_a.py::TestK::test_p[a/b]",
            ("tests.test_a.TestK", "test_p[a/b]"),
        ),
    ];
    for (id, (class, name)) in table {
        assert_eq!(address(id), (class.to_owned(), name.to_owned()), "{id}");
    }
}

#[test]
fn a_recorded_report_gives_each_test_its_outcome() {
    let got = seen(
        RunExit::Code(1),
        RECORDED,
        &[
            "tests/test_a.py::TestK::test_m",
            "tests/test_a.py::TestK::test_fail",
            "tests/test_a.py::test_p[x y]",
            "tests/test_a.py::test_p[z.w::q]",
            "tests/test_a.py::test_skip",
            "tests/test_a.py::test_xf",
            "tests/test_a.py::test_err",
            "tests/test_b.py::test_b",
            "tests/test_c.py::test_c",
        ],
    );
    let want = [
        ("tests/test_a.py::TestK::test_m", Observed::Passed),
        ("tests/test_a.py::TestK::test_fail", Observed::Failed),
        ("tests/test_a.py::test_p[x y]", Observed::Passed),
        ("tests/test_a.py::test_p[z.w::q]", Observed::Passed),
        ("tests/test_a.py::test_skip", Observed::Skipped),
        ("tests/test_a.py::test_xf", Observed::Skipped),
        ("tests/test_a.py::test_err", Observed::Errored),
        // the file that did not import reports one error and no tests
        ("tests/test_b.py::test_b", Observed::Errored),
        ("tests/test_c.py::test_c", Observed::Passed),
    ];
    assert_eq!(got.len(), want.len());
    for (id, outcome) in want {
        assert_eq!(got[id], outcome, "{id}");
    }
}

#[test]
fn a_test_the_run_did_not_report_is_unknown() {
    let got = seen(
        RunExit::Code(1),
        RECORDED,
        &["tests/test_a.py::test_never_ran"],
    );
    assert!(unknown_with(
        &got["tests/test_a.py::test_never_ran"],
        "did not report"
    ));
}

#[test]
fn malformed_truncated_and_foreign_reports_are_errors() {
    let truncated = &RECORDED[..RECORDED.len() / 2];
    let no_suite = r#"<testsuites></testsuites>"#;
    let html = "<html><body>ok</body></html>";
    let bad = [
        ("", "empty"),
        ("not xml at all", "text"),
        (truncated, "truncated"),
        (no_suite, "no testsuite"),
        (html, "wrong root"),
        // a DTD is how XML entity bombs are delivered
        (
            r#"<!DOCTYPE x [<!ENTITY a "aaaa">]><testsuites><testsuite tests="0" failures="0" errors="0" skipped="0"/></testsuites>"#,
            "dtd",
        ),
    ];
    for (xml, what) in bad {
        assert!(parse(xml).is_err(), "{what} must not parse");
    }
}

#[test]
fn a_report_that_contradicts_itself_is_an_error() {
    let forged = [
        // says everything passed, shows a failure
        (
            r#"<testsuites><testsuite tests="1" failures="0" errors="0" skipped="0"><testcase classname="t" name="a"><failure message="x"/></testcase></testsuite></testsuites>"#,
            "failure hidden from the totals",
        ),
        // says one failed, shows none
        (
            r#"<testsuites><testsuite tests="1" failures="1" errors="0" skipped="0"><testcase classname="t" name="a"/></testsuite></testsuites>"#,
            "failure claimed without a case",
        ),
        // fewer cases than stated
        (
            r#"<testsuites><testsuite tests="3" failures="0" errors="0" skipped="0"><testcase classname="t" name="a"/></testsuite></testsuites>"#,
            "cases missing",
        ),
        // a total missing or not a number
        (
            r#"<testsuites><testsuite tests="1" failures="0" skipped="0"><testcase classname="t" name="a"/></testsuite></testsuites>"#,
            "errors total missing",
        ),
        (
            r#"<testsuites><testsuite tests="x" failures="0" errors="0" skipped="0"><testcase classname="t" name="a"/></testsuite></testsuites>"#,
            "tests total not a number",
        ),
        // a case without a name
        (
            r#"<testsuites><testsuite tests="1" failures="0" errors="0" skipped="0"><testcase classname="t"/></testsuite></testsuites>"#,
            "case without a name",
        ),
    ];
    for (xml, what) in forged {
        assert!(parse(xml).is_err(), "{what} must not parse");
    }
}

/// A report that is consistent with itself but not with the run.
#[test]
fn the_exit_code_and_the_report_must_agree() {
    let passing = r#"<testsuites><testsuite tests="1" failures="0" errors="0" skipped="0"><testcase classname="tests.test_a" name="test_x"/></testsuite></testsuites>"#;
    let failing = r#"<testsuites><testsuite tests="1" failures="1" errors="0" skipped="0"><testcase classname="tests.test_a" name="test_x"><failure message="boom"/></testcase></testsuite></testsuites>"#;
    let id = "tests/test_a.py::test_x";
    let table = [
        (RunExit::Code(0), passing, Observed::Passed),
        (RunExit::Code(1), failing, Observed::Failed),
    ];
    for (exit, xml, want) in table {
        assert_eq!(seen(exit, xml, &[id])[id], want);
    }
    // a passing report from a run that failed, and the reverse: forged
    // or truncated by something, so neither is believed
    let mismatched = [
        (RunExit::Code(1), passing, "exited 1 but"),
        (RunExit::Code(0), failing, "exited 0 but"),
        (RunExit::Code(2), passing, "code 2"),
        (RunExit::Code(5), passing, "code 5"),
        (RunExit::Code(4), failing, "code 4"),
        (RunExit::Code(-1), passing, "code -1"),
    ];
    for (exit, xml, text) in mismatched {
        let got = seen(exit.clone(), xml, &[id]);
        assert!(unknown_with(&got[id], text), "{exit:?}: {:?}", got[id]);
    }
}

#[test]
fn a_run_that_did_not_finish_leaves_every_test_unknown() {
    let id = "tests/test_a.py::test_x";
    for why in [
        "timed out after 5 ms",
        "the run left processes behind",
        "killed by signal Some(9)",
    ] {
        let got = seen(RunExit::Failed(why.into()), RECORDED, &[id]);
        assert!(unknown_with(&got[id], why), "{why}");
    }
    let got = observe(
        &RunExit::Code(0),
        Err("the run wrote no report".into()),
        &ids(&[id]),
    );
    assert!(unknown_with(&got[id], "wrote no report"));
}

#[test]
fn tests_the_report_cannot_tell_apart_are_unknown() {
    // `pkg/a.py` and `pkg.a.py` are both written as classname `pkg.a`
    let xml = r#"<testsuites><testsuite tests="1" failures="0" errors="0" skipped="0"><testcase classname="pkg.a" name="test_x"/></testsuite></testsuites>"#;
    let clash = ["pkg/a.py::test_x", "pkg.a.py::test_x"];
    let got = observe(&RunExit::Code(0), parse(xml), &ids(&clash));
    for id in clash {
        assert!(unknown_with(&got[id], "same name"), "{id}: {:?}", got[id]);
    }
}

#[test]
fn a_test_listed_twice_by_the_report_is_unknown() {
    let xml = r#"<testsuites><testsuite tests="2" failures="1" errors="0" skipped="0"><testcase classname="t" name="x"/><testcase classname="t" name="x"><failure message="a"/></testcase></testsuite></testsuites>"#;
    let id = "t.py::x";
    let got = observe(&RunExit::Code(1), parse(xml), &ids(&[id]));
    assert!(unknown_with(&got[id], "more than once"));
}

/// Recorded: `pytest tests/test_b.py::test_b` where test_b.py cannot be
/// imported exits 4 ("found no collectors") with this report.
const NO_COLLECTORS: &str = r#"<?xml version="1.0" encoding="utf-8"?><testsuites name="pytest tests"><testsuite name="pytest" errors="1" failures="0" skipped="0" tests="1" time="0.010"><testcase classname="" name="tests.test_b" time="0.000"><error message="collection failure">ModuleNotFoundError: No module named 'nothere'</error></testcase></testsuite></testsuites>"#;

#[test]
fn exit_four_with_an_import_error_in_the_report_means_the_file_errored() {
    let id = "tests/test_b.py::test_b";
    assert_eq!(
        seen(RunExit::Code(4), NO_COLLECTORS, &[id])[id],
        Observed::Errored
    );
    // the same exit code without that error in the report is a usage error
    let failing = r#"<testsuites><testsuite tests="1" failures="1" errors="0" skipped="0"><testcase classname="tests.test_b" name="test_b"><failure message="x"/></testcase></testsuite></testsuites>"#;
    let got = seen(RunExit::Code(4), failing, &[id]);
    assert!(unknown_with(&got[id], "code 4"), "{:?}", got[id]);
    let empty =
        r#"<testsuites><testsuite tests="0" failures="0" errors="0" skipped="0"/></testsuites>"#;
    let got = seen(RunExit::Code(4), empty, &[id]);
    assert!(unknown_with(&got[id], "code 4"), "{:?}", got[id]);
}

#[test]
fn a_report_with_no_test_proves_nothing() {
    let xml =
        r#"<testsuites><testsuite tests="0" failures="0" errors="0" skipped="0"/></testsuites>"#;
    let id = "t.py::x";
    let got = observe(&RunExit::Code(0), parse(xml), &ids(&[id]));
    assert!(unknown_with(&got[id], "lists no test"));
}

#[test]
fn a_failure_and_a_teardown_error_together_count_as_failed() {
    let xml = r#"<testsuites><testsuite tests="1" failures="1" errors="1" skipped="0"><testcase classname="t" name="x"><failure message="a"/><error message="teardown"/></testcase></testsuite></testsuites>"#;
    let got = observe(&RunExit::Code(1), parse(xml), &ids(&["t.py::x"]));
    assert_eq!(got["t.py::x"], Observed::Failed);
}

#[test]
fn a_collection_error_in_another_file_does_not_make_this_one_errored() {
    // test_b failed to import; the requested test is in test_c, which the
    // report never mentions
    let got = seen(RunExit::Code(1), RECORDED, &["tests/test_d.py::test_d"]);
    assert!(unknown_with(
        &got["tests/test_d.py::test_d"],
        "did not report"
    ));
}
