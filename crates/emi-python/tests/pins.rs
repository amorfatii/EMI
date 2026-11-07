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

/// What the pinned laws drew when pytest ran `selection`.
fn drawn(dir: &Path, selection: &[&str]) -> String {
    let log = dir.join("drawn.log");
    let _ = std::fs::remove_file(&log);
    let out = Command::new(venv().join("bin/python"))
        .args(["-m", "pytest", "-q"])
        .args(Pins::pytest_args())
        .args(selection)
        .envs(Pins::default().env())
        .env("PYTHONPATH", Pins::python_path(&[]))
        .env("DRAWN", &log)
        .current_dir(dir)
        .output()
        .expect("run pytest");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&log).expect("the laws drew something")
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
