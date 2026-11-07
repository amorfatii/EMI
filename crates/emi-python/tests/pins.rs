//! The pins plugin against real pytest and Hypothesis: what a pinned test
//! draws depends on the test alone.
//!
//! These tests need `python3` and the shared virtualenv (pytest, coverage.py,
//! Hypothesis), built once under the target directory. A machine that cannot
//! provide them fails these tests with the reason; it does not skip them.
// a test that cannot set itself up must fail loudly, not pass
#![allow(clippy::expect_used)]

mod support {
    pub mod venv;
}

use std::path::Path;
use std::process::Command;

use emi_kernel::pins::Pins;
use support::venv::venv;

/// Each test appends what it drew, one line per example, to the log.
const LAWS: &str = r#"import os

from hypothesis import given, settings, strategies as st
from hypothesis.stateful import RuleBasedStateMachine, rule


def log(kind, value):
    with open(os.environ["DRAWN"], "a") as f:
        f.write(f"{kind} {value!r}\n")


@given(st.integers())
def test_module_level(x):
    log("given", x)


@settings(derandomize=False, max_examples=20)
@given(st.text())
def test_with_its_own_settings(s):
    log("own", s)


class Machine(RuleBasedStateMachine):
    @rule(n=st.integers())
    def step(self, n):
        log("machine", n)


TestMachine = Machine.TestCase
"#;

/// Imported by a test that runs first: its literals are what Hypothesis
/// would otherwise mix into the draws of every test after it.
const OTHER: &str = "MAGIC = 987654321\nWORD = 'xyzzy-literal'\n";

fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join("tests")).expect("tests dir");
    std::fs::write(dir.join("other.py"), OTHER).expect("other.py");
    std::fs::write(dir.join("test_laws.py"), LAWS).expect("test_laws.py");
    std::fs::write(
        dir.join("tests/test_first.py"),
        "import other\n\n\ndef test_first():\n    assert other.MAGIC\n",
    )
    .expect("test_first.py");
    std::fs::write(dir.join("pytest.ini"), "[pytest]\npythonpath = .\n").expect("pytest.ini");
    emi_python::pins::install(dir).expect("install the plugin");
}

/// What the pinned laws drew when pytest ran `selection`, and what pytest
/// printed. `extra` is added to the run's environment.
fn pinned_run(dir: &Path, selection: &[&str], extra: &[(&str, &str)]) -> (bool, String, String) {
    let log = dir.join("drawn.log");
    let _ = std::fs::remove_file(&log);
    let out = Command::new(venv().join("bin/python"))
        .args(["-m", "pytest", "-q"])
        .args(Pins::pytest_args())
        .args(selection)
        .envs(Pins::default().env())
        .envs(extra.iter().copied())
        .env("PYTHONPATH", Pins::python_path(&[]))
        .env("DRAWN", &log)
        .current_dir(dir)
        .output()
        .expect("run pytest");
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = std::fs::read_to_string(&log).expect("the laws drew something");
    (out.status.success(), log, printed)
}

/// What the pinned laws drew when pytest ran `selection`; they all pass.
fn drawn(dir: &Path, selection: &[&str]) -> String {
    let (passed, log, printed) = pinned_run(dir, selection, &[]);
    assert!(passed, "{printed}");
    log
}

/// A law that fails for every x of 10 and more, logging each x it is
/// called with.
const FAILING: &str = r#"import os

from hypothesis import given, strategies as st


@given(st.integers(0, 10**6))
def test_small(x):
    with open(os.environ["DRAWN"], "a") as f:
        f.write(f"{x}\n")
    assert x < 10
"#;

#[test]
fn a_failure_is_shrunk_unexplained_and_only_confirmed_when_confirming() {
    let tmp = tempfile::tempdir().expect("tempdir");
    project(tmp.path());
    std::fs::write(tmp.path().join("test_failing.py"), FAILING).expect("law");
    let (passed, shrunk, printed) = pinned_run(tmp.path(), &["test_failing.py"], &[]);
    assert!(!passed, "{printed}");
    assert!(
        printed.contains("x=10,"),
        "shrunk to the smallest: {printed}"
    );
    assert!(!printed.contains("Explanation"), "{printed}");
    let confirm = [(emi_kernel::pins::CONFIRM_ENV, "1")];
    let (passed, confirmed, printed) = pinned_run(tmp.path(), &["test_failing.py"], &confirm);
    assert!(!passed, "{printed}");
    let calls: Vec<i64> = confirmed
        .lines()
        .map(|l| l.parse().expect("a number"))
        .collect();
    // it stops at the first failure: the calls before it passed, and the
    // only calls after it run that same input again, to report it
    let first = calls.iter().position(|x| *x >= 10).expect("a failure");
    assert!(
        calls[first..].iter().all(|x| *x == calls[first]),
        "{calls:?}"
    );
    // and it is the same first failure the shrinking run started from
    let up_to_first: String = confirmed
        .lines()
        .take(first + 1)
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(
        shrunk.starts_with(&up_to_first),
        "{shrunk}\n---\n{confirmed}"
    );
}

#[test]
fn a_pinned_test_draws_the_same_whatever_ran_before_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    project(tmp.path());
    let alone = drawn(tmp.path(), &["test_laws.py"]);
    let again = drawn(tmp.path(), &["test_laws.py"]);
    let after_another = drawn(tmp.path(), &["tests", "test_laws.py"]);
    for kind in ["given", "own", "machine"] {
        assert!(
            alone.lines().any(|l| l.starts_with(kind)),
            "{kind} drew nothing:\n{alone}"
        );
    }
    assert_eq!(alone, again, "two runs of the same selection");
    assert_eq!(
        alone, after_another,
        "a test that imported constants ran first"
    );
    assert!(
        !alone.contains("987654321") && !alone.contains("xyzzy-literal"),
        "a literal of another module was drawn:\n{alone}"
    );
    assert!(
        !tmp.path().join(".hypothesis/examples").exists(),
        "an example database was written"
    );
}
