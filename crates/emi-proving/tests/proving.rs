//! The measurement against real pytest and coverage.py in sandboxed runs,
//! on fixture repositories made for each case.
//!
//! These tests need `python3`, bubblewrap and a virtualenv holding pytest
//! and coverage.py. The virtualenv is built once, on the first run, under
//! the target directory (that needs network access once), and linked into
//! every fixture like `--link .venv`. A machine that cannot provide them
//! fails these tests with the reason; it does not skip them.
// a test that cannot set itself up must fail loudly, not pass
#![allow(clippy::expect_used)]

mod support {
    pub mod venv;
}

use std::path::Path;
use std::process::Command;
use std::time::Duration;
use support::venv::venv;

use emi_proving::{Candidate, ChangeEvidence, Language, Request, Status, measure};
use serde_json::{Value, json};

fn run(dir: &Path, program: &str, args: &[&str]) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git(repo: &Path, args: &[&str]) {
    let mut all = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
    ];
    all.extend(args);
    run(repo, "git", &all);
}

fn head(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repository with a commit of `BASE` files; `.venv` links to the shared
/// virtualenv and is ignored, as in a real checkout.
struct Fixture {
    dir: tempfile::TempDir,
}

const BASE: &[(&str, &str)] = &[
    (
        ".gitignore",
        ".venv\n__pycache__/\n.emi-proving/\n.coverage\n",
    ),
    ("pytest.ini", "[pytest]\npythonpath = src\n"),
    (
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n",
    ),
    (
        "tests/test_calc.py",
        "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    ),
];

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        let fixture = Fixture { dir };
        fixture.write(BASE);
        std::os::unix::fs::symlink(venv(), fixture.path().join(".venv")).expect("link .venv");
        fixture.commit("baseline");
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let path = self.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }
    }

    fn commit(&self, message: &str) -> String {
        git(self.path(), &["add", "-A"]);
        git(self.path(), &["commit", "-q", "-m", message]);
        head(self.path())
    }

    /// The request for the last commit against the one before it.
    fn request(&self) -> Request {
        let mut req = Request::new(
            self.path(),
            "HEAD~1",
            [".venv/bin/python", "-m", "pytest", "-q"]
                .map(str::to_owned)
                .to_vec(),
        );
        req.candidate = Candidate::Ref("HEAD".into());
        req.source = "src".into();
        req.link = vec![".venv".into()];
        req.timeout = Duration::from_secs(120);
        req
    }
}

fn change(req: &Request) -> ChangeEvidence {
    measure(req).unwrap_or_else(|e| panic!("measure failed: {e}"))
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["id"].as_str().map(str::to_owned))
        .collect()
}

fn function<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["function"] == name)
        .unwrap_or_else(|| panic!("no function {name} in {v}"))
}

const ADD_FIXED: &str = "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n";

#[test]
fn a_test_that_fails_at_the_baseline_and_passes_on_the_candidate_proves_the_change() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
    ]);
    fx.commit("handle None");
    let req = fx.request();
    let evidence = change(&req);
    let v = evidence.to_json();
    assert_eq!(v["status"], "proven", "{v}");
    assert_eq!(
        v["proving_tests"],
        json!([{"id": "tests/test_calc.py::test_add_none", "baseline_result": "failed"}])
    );
    assert_eq!(v["redundant_tests"], json!([]));
    assert_eq!(v["unknown_tests"], json!([]));
    assert_eq!(function(&v, "add")["status"], "proven");
    assert_eq!(v["functions"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(evidence.isolation.as_deref(), Some("bwrap"));
    // the same inputs give the same object
    assert_eq!(change(&req).to_json(), v);
}

#[test]
fn the_repository_may_be_named_through_a_symbolic_link() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
    ]);
    fx.commit("handle None");
    let alias = tempfile::tempdir().expect("tempdir");
    let named = alias.path().join("checkout");
    std::os::unix::fs::symlink(fx.path(), &named).expect("symlink");
    let mut req = fx.request();
    req.repo = named;
    let v = change(&req).to_json();
    assert_eq!(v["status"], "proven", "{v}");
}

#[test]
fn a_test_that_already_passes_at_the_baseline_is_redundant() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n\n\ndef test_add_again():\n    assert add(2, 2) == 4\n",
        ),
    ]);
    fx.commit("handle None, and a test that adds nothing");
    let v = change(&fx.request()).to_json();
    assert_eq!(
        ids(&v["redundant_tests"]),
        vec!["tests/test_calc.py::test_add_again"]
    );
    assert_eq!(
        ids(&v["proving_tests"]),
        vec!["tests/test_calc.py::test_add_none"]
    );
    assert_eq!(v["status"], "proven", "{v}");
}

#[test]
fn a_test_for_a_function_that_does_not_exist_yet_errors_at_the_baseline() {
    let fx = Fixture::new();
    fx.write(&[
        (
            "src/calc.py",
            "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n",
        ),
        (
            "tests/test_mul.py",
            "from calc import mul\n\n\ndef test_mul():\n    assert mul(2, 3) == 6\n\n\ndef test_mul_zero():\n    assert mul(0, 3) == 0\n",
        ),
    ]);
    fx.commit("add mul");
    let v = change(&fx.request()).to_json();
    assert_eq!(
        v["proving_tests"],
        json!([
            {"id": "tests/test_mul.py::test_mul", "baseline_result": "errored"},
            {"id": "tests/test_mul.py::test_mul_zero", "baseline_result": "errored"},
        ]),
        "{v}"
    );
    assert_eq!(function(&v, "mul")["status"], "proven");
    assert_eq!(v["status"], "proven");
}

#[test]
fn an_import_error_does_not_make_unrelated_tests_proving() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", "def add(a, b):\n    return a + b\n\ndef mul(a, b):\n    return a * b\n"),
        ("tests/test_new.py", "from calc import mul, add\n\ndef test_mul():\n    assert mul(2, 3) == 6\n\ndef test_unrelated():\n    assert add(1, 2) == 3\n"),
    ]);
    fx.commit("new function and unrelated test in the same file");
    let v = change(&fx.request()).to_json();
    assert_eq!(ids(&v["proving_tests"]), ["tests/test_new.py::test_mul"]);
    assert_eq!(
        ids(&v["redundant_tests"]),
        ["tests/test_new.py::test_unrelated"]
    );
    assert_eq!(function(&v, "mul")["status"], "proven", "{v}");
}

#[test]
fn a_changed_function_no_proving_test_reaches_is_unproven() {
    let fx = Fixture::new();
    fx.write(&[
        (
            "src/calc.py",
            "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n\n\ndef sub(a, b):\n    if b is None:\n        return a\n    return a - b\n",
        ),
        // never imported by any test, so only a coverage run told to look
        // at its directory can say that nothing ran in it
        ("src/unused.py", "def helper():\n    return 1\n"),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
    ]);
    fx.commit("handle None in add and sub");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["status"], "partly_proven", "{v}");
    assert_eq!(function(&v, "add")["status"], "proven");
    assert_eq!(function(&v, "sub")["status"], "unproven");
    assert_eq!(function(&v, "helper")["status"], "unproven");
}

#[test]
fn a_change_with_no_added_test_leaves_every_changed_function_unproven() {
    let fx = Fixture::new();
    fx.write(&[("src/calc.py", ADD_FIXED)]);
    fx.commit("handle None, with no test");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["status"], "unproven", "{v}");
    assert_eq!(v["proving_tests"], json!([]));
    assert_eq!(function(&v, "add")["status"], "unproven");
}

#[test]
fn an_old_test_in_a_moved_file_is_not_an_added_test() {
    let fx = Fixture::new();
    git(
        fx.path(),
        &["mv", "tests/test_calc.py", "tests/test_arithmetic.py"],
    );
    fx.commit("move the tests");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["status"], "none", "{v}");
    for list in ["proving_tests", "redundant_tests", "unknown_tests"] {
        assert_eq!(v[list], json!([]), "{list}: {v}");
    }
}

#[test]
fn a_change_outside_functions_has_no_change_to_prove() {
    let fx = Fixture::new();
    fx.write(&[("tests/test_calc.py", "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_more():\n    assert add(5, 5) == 10\n")]);
    fx.commit("a test, nothing else");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["status"], "none", "{v}");
    assert_eq!(v["functions"], json!([]));
    assert_eq!(
        ids(&v["redundant_tests"]),
        vec!["tests/test_calc.py::test_add_more"]
    );
}

#[test]
fn a_test_that_fails_on_the_candidate_too_is_unknown_not_proving() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_wrong():\n    assert add(1, 1) == 3\n",
        ),
    ]);
    fx.commit("a wrong test");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["proving_tests"], json!([]));
    let unknown = &v["unknown_tests"][0];
    assert_eq!(unknown["id"], "tests/test_calc.py::test_wrong");
    assert!(
        unknown["reason"]
            .as_str()
            .is_some_and(|r| r.contains("candidate")),
        "{v}"
    );
    // the change cannot be called unproven while a test may yet prove it
    assert_eq!(function(&v, "add")["status"], "unknown");
    assert_eq!(v["status"], "unknown");
}

#[test]
fn a_test_that_does_not_finish_in_time_is_unknown() {
    let fx = Fixture::new();
    fx.write(&[(
        "tests/test_slow.py",
        "import time\n\n\ndef test_slow():\n    time.sleep(120)\n",
    )]);
    fx.commit("a test that hangs");
    let mut req = fx.request();
    req.timeout = Duration::from_secs(4);
    let v = change(&req).to_json();
    assert_eq!(v["proving_tests"], json!([]), "{v}");
    assert_eq!(v["redundant_tests"], json!([]), "{v}");
    assert_eq!(
        v["unknown_tests"],
        json!([{
            "id": "tests/test_slow.py::test_slow",
            "reason": "at the baseline: timed out (limit 4 s)",
        }])
    );
}

#[test]
fn a_skipped_test_is_unknown() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "import pytest\nfrom calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\n@pytest.mark.skip(reason='later')\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
    ]);
    fx.commit("a skipped test");
    let v = change(&fx.request()).to_json();
    assert_eq!(
        ids(&v["unknown_tests"]),
        vec!["tests/test_calc.py::test_add_none"]
    );
    assert_eq!(v["proving_tests"], json!([]));
    assert_eq!(v["redundant_tests"], json!([]));
}

/// A candidate that rewrites the report so a failing test looks as if it
/// passed: the run still exits 1, so the two cannot both be true.
#[test]
fn a_report_forged_by_the_candidate_is_not_believed() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
        (
            "conftest.py",
            "import pytest\n\n\n@pytest.hookimpl(trylast=True)\ndef pytest_sessionfinish(session):\n    path = session.config.option.xmlpath\n    if path:\n        with open(path, 'w') as f:\n            f.write('<testsuites><testsuite tests=\"1\" failures=\"0\" errors=\"0\" skipped=\"0\"><testcase classname=\"tests.test_calc\" name=\"test_add_none\"/></testsuite></testsuites>')\n",
        ),
    ]);
    fx.commit("forge the report");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["proving_tests"], json!([]), "{v}");
    assert_eq!(v["redundant_tests"], json!([]), "{v}");
    assert_eq!(
        ids(&v["unknown_tests"]),
        vec!["tests/test_calc.py::test_add_none"],
        "{v}"
    );
}

#[test]
fn a_changed_file_that_does_not_parse_keeps_the_change_from_being_proven() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        ("src/broken.py", "def oops(:\n    pass\n"),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
    ]);
    fx.commit("a file that is not Python");
    let v = change(&fx.request()).to_json();
    assert_eq!(function(&v, "add")["status"], "proven");
    assert_eq!(v["status"], "partly_proven", "{v}");
    let notes = v["not_measured"].as_array().expect("not_measured");
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().is_some_and(|s| s.contains("src/broken.py"))),
        "{v}"
    );
}

#[test]
fn the_working_tree_is_a_candidate_too() {
    let fx = Fixture::new();
    fx.write(&[
        ("src/calc.py", ADD_FIXED),
        (
            "tests/test_calc.py",
            "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_add_none():\n    assert add(None, 2) == 2\n",
        ),
    ]);
    // nothing is committed
    let mut req = fx.request();
    req.baseline = "HEAD".into();
    req.candidate = Candidate::Worktree;
    let v = change(&req).to_json();
    assert_eq!(v["status"], "proven", "{v}");
    assert_eq!(
        ids(&v["proving_tests"]),
        vec!["tests/test_calc.py::test_add_none"]
    );
}

#[test]
fn another_language_is_unknown_with_a_reason_and_runs_nothing() {
    let fx = Fixture::new();
    fx.write(&[(
        "main.go",
        "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
    )]);
    fx.commit("go code");
    let mut req = fx.request();
    req.language = Language::from_name("go");
    req.test_command = ["go", "test", "./..."].map(str::to_owned).to_vec();
    let evidence = change(&req);
    let v = evidence.to_json();
    assert_eq!(evidence.status, Status::Unknown);
    assert_eq!(v["status"], "unknown");
    assert_eq!(v["proving_tests"], json!([]));
    let notes = v["not_measured"].as_array().expect("not_measured");
    assert!(
        notes.iter().any(|n| n
            .as_str()
            .is_some_and(|s| s.contains("`go` is not supported"))),
        "{v}"
    );
    // the functions the change touched are still listed, as unknown
    assert_eq!(function(&v, "add")["status"], "unknown");
    assert_eq!(evidence.isolation, None, "nothing was run");
}

#[test]
fn a_test_command_that_is_not_pytest_is_unknown_with_a_reason() {
    let fx = Fixture::new();
    fx.write(&[("src/calc.py", ADD_FIXED)]);
    fx.commit("a change");
    let mut req = fx.request();
    req.test_command = ["make", "test"].map(str::to_owned).to_vec();
    let evidence = change(&req);
    let v = evidence.to_json();
    assert_eq!(v["status"], "unknown");
    assert!(
        v["not_measured"][0]
            .as_str()
            .is_some_and(|s| s.contains("not a pytest invocation")),
        "{v}"
    );
}

#[test]
fn a_collection_error_at_the_candidate_is_unknown_with_a_reason() {
    let fx = Fixture::new();
    fx.write(&[("tests/test_broken.py", "def test_x(:\n    pass\n")]);
    fx.commit("a test file that does not import");
    let v = change(&fx.request()).to_json();
    assert_eq!(v["status"], "unknown", "{v}");
    let reason = v["not_measured"][0].as_str().unwrap_or_default();
    assert!(
        reason.starts_with("listing the tests at the candidate exited 2"),
        "{v}"
    );
    assert!(reason.contains("SyntaxError"), "{v}");
    // no scratch path, so the same inputs give the same receipt
    assert!(!reason.contains("emi-profile"), "{v}");
    assert_eq!(change(&fx.request()).to_json(), v);
}

#[test]
fn an_empty_command_or_a_bad_ref_is_an_error() {
    let fx = Fixture::new();
    let mut req = fx.request();
    req.test_command = Vec::new();
    assert!(measure(&req).is_err());
    let mut req = fx.request();
    req.baseline = "no-such-ref".into();
    assert!(measure(&req).is_err());
}

#[test]
fn the_strict_rule_is_declared_not_measured() {
    let fx = Fixture::new();
    fx.write(&[("src/calc.py", ADD_FIXED)]);
    fx.commit("handle None");
    let mut req = fx.request();
    req.strict = true;
    let v = change(&req).to_json();
    let notes = v["not_measured"].as_array().expect("not_measured");
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().is_some_and(|s| s.contains("stricter rule"))),
        "{v}"
    );
}
