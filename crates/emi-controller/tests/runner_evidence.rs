//! What a passing check is worth when the controller sees only an exit
//! status and an inventory of test ids: the stand-in runner below behaves
//! like pytest where it matters here (collection lists every test, skipped
//! tests are collected, a run whose tests all pass or skip exits 0).

use std::path::Path;
use std::process::Command;

use emi_controller::{Candidate, Store, create, verify};
use serde_json::{Value, json};
use tempfile::TempDir;

const APP: &str = "DISABLED = False\n\n\ndef add(a, b):\n    return a - b\n";
const FIXED_APP: &str = "DISABLED = False\n\n\ndef add(a, b):\n    return a + b\n";
const TESTS: &str = "import app\n\n\ndef test_one():\n    assert app.add(1, 2) == 3\n\n\ndef test_two():\n    assert app.add(0, 0) == 0\n\n\ndef test_three():\n    if app.DISABLED:\n        skip()\n    assert app.add(2, 2) == 4\n";

/// Lists test ids with `--collect-only` and otherwise runs them. It imports
/// the candidate's `app` first, as a real runner imports what tests import.
const RUNNER: &str = r#"import builtins, glob, re, sys

sys.path.insert(0, ".")


class Skipped(Exception):
    pass


def skip():
    raise Skipped()


builtins.skip = skip
import app  # noqa: E402,F401

tests = []
for path in sorted(glob.glob("tests/test_*.py")):
    scope = {}
    exec(compile(open(path).read(), path, "exec"), scope)
    for name, fn in scope.items():
        if name.startswith("test_") and callable(fn):
            tests.append((f"{path}::{name}", fn))
if "--collect-only" in sys.argv:
    for test_id, _ in tests:
        print(test_id)
    print(f"{len(tests)} tests collected")
    sys.exit(0)
failed = 0
for test_id, fn in tests:
    try:
        fn()
        print(test_id, "PASSED")
    except Skipped:
        print(test_id, "SKIPPED")
    except AssertionError:
        failed += 1
        print(test_id, "FAILED")
sys.exit(1 if failed else 0)
"#;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(dir)).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(path, text).unwrap_or_else(|e| panic!("{e}"));
}

struct Task {
    repo: TempDir,
    _authority: TempDir,
    store: Store,
}

impl Task {
    /// A repository holding the buggy `app`, its tests and the runner, and
    /// a task whose check runs the runner. `inventory` adds the runner's
    /// listing as the contract's test inventory; `check` replaces the
    /// check's command.
    fn new(inventory: Option<Value>, check: Option<Value>) -> Task {
        let repo = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let authority = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        git(repo.path(), &["init", "-q", "-b", "main"]);
        write(repo.path(), "app.py", APP);
        write(repo.path(), "tests/test_app.py", TESTS);
        write(repo.path(), "runner.py", RUNNER);
        write(repo.path(), "skip.txt", "");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "base"]);
        let mut spec = json!({
            "task_id": "evidence", "repo": repo.path(), "baseline": "HEAD",
            "objective": "make add() add",
            "requirements": [{"id": "R1", "acceptance": "add adds", "checks": ["suite"]}],
            "checks": {"suite": {"command": ["python3", "runner.py"],
                                 "success": {"exit_code": 0}, "timeout_s": 60,
                                 "evidence": {"format": "pytest"}}},
            "verification": {"trusted": ["tests/*"]},
        });
        if let Some(inventory) = inventory {
            spec["verification"]["inventory"] = inventory;
        }
        if let Some(check) = check {
            spec["checks"]["suite"] = check;
        }
        create(authority.path(), &spec).unwrap_or_else(|e| panic!("{e}"));
        let store = Store::open(authority.path(), "evidence", repo.path())
            .unwrap_or_else(|e| panic!("{e}"));
        Task {
            repo,
            _authority: authority,
            store,
        }
    }

    /// Commit `app` as the candidate and verify it.
    fn verify_app(&self, app: &str) -> Value {
        write(self.repo.path(), "app.py", app);
        self.verify_commit()
    }

    fn verify_commit(&self) -> Value {
        git(self.repo.path(), &["add", "-A"]);
        git(self.repo.path(), &["commit", "-q", "-m", "candidate"]);
        verify(&self.store, &Candidate::Ref("HEAD".into())).unwrap_or_else(|e| panic!("{e}"))
    }
}

fn pytest_listing() -> Value {
    json!({"command": ["python3", "runner.py", "--collect-only", "-q"], "format": "pytest"})
}

#[test]
fn an_honest_fix_is_accepted_with_a_complete_inventory() {
    // the control for the cases below: the fixture's fix passes everything
    let task = Task::new(Some(pytest_listing()), None);
    let d = task.verify_app(FIXED_APP);
    assert_eq!(d["decision"], "accepted", "{d}");
    assert_eq!(
        d["gates"]["verification"]["inventory"]["baseline"], 3,
        "{d}"
    );
}

#[test]
fn an_exit_before_collection_ends_leaves_the_inventory_short_and_rejects() {
    let task = Task::new(Some(pytest_listing()), None);
    // the exit is dressed so that no marker of the guardrails matches it
    let exits = format!("{FIXED_APP}\nimport os\n\ngetattr(os, '_exit')(0)\n");
    let d = task.verify_app(&exits);
    assert_eq!(d["decision"], "rejected", "{d}");
    assert_eq!(
        d["gates"]["verification"]["inventory"]["state"], "fail",
        "{d}"
    );
    assert_eq!(
        d["gates"]["verification"]["inventory"]["missing"]
            .as_array()
            .map(Vec::len),
        Some(3),
        "{d}"
    );
}

#[test]
fn without_an_inventory_an_early_exit_passes_and_the_decision_says_so() {
    // the known limit of an exit status as the only evidence (docs/tasks.md):
    // what the controller can do here is state it in the decision; the
    // check declares no evidence, so the exit code stands alone
    let plain = json!({"command": ["python3", "runner.py"],
        "success": {"exit_code": 0}, "timeout_s": 60});
    let task = Task::new(None, Some(plain));
    let exits = "import os\n\ngetattr(os, '_exit')(0)\n";
    let d = task.verify_app(exits);
    assert_eq!(d["decision"], "accepted", "{d}");
    let limits = d["limits"].to_string();
    assert!(limits.contains("no test inventory"), "{d}");
    assert!(limits.contains("status 0 before the tests run"), "{d}");
}

#[test]
fn a_listing_whose_lines_are_not_test_ids_is_unverified_not_empty() {
    // `-q -q` style counts per file: nothing parses as `path::name`
    let counts = json!({"command": ["python3", "-c",
        "print('tests/test_app.py: 3'); print('3 tests collected in 0.01s')"],
        "format": "pytest"});
    let task = Task::new(Some(counts), None);
    let d = task.verify_app(FIXED_APP);
    assert_eq!(d["decision"], "incomplete", "{d}");
    assert_eq!(
        d["gates"]["verification"]["inventory"]["state"], "unverified",
        "{d}"
    );
    assert!(
        d["gates"]["verification"]["inventory"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("no line of its output is a test id")),
        "{d}"
    );
}

/// A listing of 40000 long ids, more than the 32 MiB of output the
/// controller keeps of a listing, from which `skip.txt` removes the ids it
/// names.
fn long_listing() -> Value {
    let code = "import re\nomit = set(open('skip.txt').read().split())\n\
                pad = 'x' * 900\n\
                for i in range(40000):\n    n = f'test_{i:05d}'\n    \
                if n not in omit:\n        print(f'tests/test_long.py::{n}_{pad}')\n";
    json!({"command": ["python3", "-c", code], "format": "pytest"})
}

#[test]
fn a_cut_off_inventory_is_unverified_not_a_shorter_list() {
    let task = Task::new(Some(long_listing()), None);
    // a test beyond the retained output is dropped by configuration
    write(task.repo.path(), "skip.txt", "test_39999\n");
    let d = task.verify_app(FIXED_APP);
    assert_ne!(d["decision"], "accepted", "{d}");
    assert_eq!(
        d["gates"]["verification"]["inventory"]["state"], "unverified",
        "{d}"
    );
    assert!(
        d["gates"]["verification"]["inventory"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("list of tests is partial")),
        "{d}"
    );
}

#[test]
fn a_check_with_more_output_than_is_kept_records_that_it_was_cut() {
    let noisy = json!({"command": ["python3", "-c",
        "import sys\nsys.stdout.write('x' * 3000000 + '\\nEND\\n')"],
        "success": {"exit_code": 0}, "timeout_s": 60});
    let task = Task::new(None, Some(noisy));
    let d = task.verify_app(FIXED_APP);
    // the verdict rests on the exit status, which is complete
    assert_eq!(d["decision"], "accepted", "{d}");
    let runs = task
        .store
        .rows("check_run")
        .unwrap_or_else(|e| panic!("{e}"));
    let run = &runs[0];
    assert_eq!(run["output_truncated"], true, "{run}");
    assert!(
        run["stdout_bytes"].as_u64().is_some_and(|n| n > 3_000_000),
        "{run}"
    );
}

#[test]
fn a_check_that_dies_of_a_signal_is_a_failure_not_a_pass() {
    let dies = json!({"command": ["python3", "-c", "import os, signal\nos.kill(os.getpid(), signal.SIGKILL)"],
        "success": {"exit_code": 0}, "timeout_s": 60});
    let task = Task::new(None, Some(dies));
    let d = task.verify_app(FIXED_APP);
    assert_eq!(d["decision"], "rejected", "{d}");
    assert_ne!(d["checks"][0]["status"], "passed", "{d}");
}

#[test]
fn a_check_that_outlives_its_timeout_leaves_the_requirement_unverified() {
    let slow = json!({"command": ["python3", "-c", "import time\ntime.sleep(60)"],
        "success": {"exit_code": 0}, "timeout_s": 1});
    let task = Task::new(None, Some(slow));
    let d = task.verify_app(FIXED_APP);
    assert_eq!(d["decision"], "incomplete", "{d}");
    assert_eq!(d["checks"][0]["status"], "timeout", "{d}");
}

#[test]
fn a_baseline_that_lists_no_test_is_reported_as_nothing_to_compare() {
    let silent = json!({"command": ["python3", "-c", "pass"], "format": "pytest"});
    let task = Task::new(Some(silent), None);
    let d = task.verify_app(FIXED_APP);
    assert_eq!(
        d["gates"]["verification"]["inventory"]["state"], "nothing_to_compare",
        "{d}"
    );
}

#[test]
fn a_check_that_declares_evidence_but_prints_none_is_not_a_pass() {
    // a non-test command stays usable without evidence (see the lifecycle
    // fixtures); declaring evidence for one demands a test report
    let quiet = json!({"command": ["python3", "-c", "pass"],
        "success": {"exit_code": 0}, "timeout_s": 60, "evidence": {"format": "pytest"}});
    let task = Task::new(Some(pytest_listing()), Some(quiet));
    let d = task.verify_app(FIXED_APP);
    assert_eq!(d["decision"], "rejected", "{d}");
}

#[test]
fn an_early_exit_after_collection_is_not_a_pass_even_with_an_inventory() {
    let task = Task::new(Some(pytest_listing()), None);
    // exits only when running, so the listing stays complete
    let exits = format!(
        "{FIXED_APP}\nimport os, sys\n\nif '--collect-only' not in sys.argv:\n    getattr(os, '_exit')(0)\n"
    );
    let d = task.verify_app(&exits);
    assert_ne!(d["decision"], "accepted", "{d}");
}

#[test]
fn a_baseline_test_that_skips_on_the_candidates_code_is_not_a_pass() {
    let task = Task::new(Some(pytest_listing()), None);
    let skipping = FIXED_APP.replace("DISABLED = False", "DISABLED = True");
    let d = task.verify_app(&skipping);
    assert_ne!(d["decision"], "accepted", "{d}");
}
