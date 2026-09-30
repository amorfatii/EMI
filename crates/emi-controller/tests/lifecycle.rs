//! A supervised task from freeze to completion, on a scratch repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_controller::{Candidate, Store, complete, create, hook_check, verify};
use serde_json::{Value, json};

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("emi-ctl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_default();
    Dir(d)
}

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

fn spec(repo: &Path, task: &str) -> Value {
    json!({
        "task_id": task, "repo": repo, "baseline": "HEAD",
        "objective": "make add() add",
        "requirements": [{"id": "R1", "acceptance": "add(1, 2) == 3", "checks": ["unit"]}],
        "checks": {"unit": {"command": ["python3", "-c", "import app; assert app.add(1, 2) == 3"],
                            "success": {"exit_code": 0}, "timeout_s": 30}},
        "envelope": {"permitted_paths": ["app.py"], "protected_paths": ["checks/*"]},
        "structural": {"probe": "python", "source": ".", "allowance": 10},
        "budgets": {"max_retries": 4},
    })
}

fn decision(v: &Value) -> &str {
    v["decision"].as_str().unwrap_or("")
}

/// A repository with a buggy `add` and a frozen task over it.
fn buggy_task(name: &str) -> (Dir, Dir, Store) {
    let repo = scratch(&format!("{name}-repo"));
    let authority = scratch(&format!("{name}-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a - b\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "buggy"]);
    let inside = create(&repo.0.join(".authority"), &spec(&repo.0, name));
    assert!(
        inside
            .err()
            .unwrap_or_default()
            .contains("inside the repository")
    );
    let contract = create(&authority.0, &spec(&repo.0, name)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(contract["schema_version"], "emi-task-contract-0.1");
    assert!(
        create(&authority.0, &spec(&repo.0, name)).is_err(),
        "no silent re-init"
    );
    let store = Store::open(&authority.0, name, &repo.0).unwrap_or_else(|e| panic!("{e}"));
    (repo, authority, store)
}

fn check(store: &Store, candidate: Candidate) -> Value {
    verify(store, &candidate).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn freeze_verify_complete() {
    let (repo, _authority, store) = buggy_task("t1");
    let first = check(&store, Candidate::Worktree);
    assert_eq!(decision(&first), "rejected", "{first}");
    assert_eq!(first["requirements"][0]["state"], "failed");
    assert_eq!(first["checks"][0]["verified_clean"], true);

    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    let fixed = check(&store, Candidate::Worktree);
    assert_eq!(decision(&fixed), "accepted", "{fixed}");
    assert_eq!(fixed["gates"]["structural"]["state"], "pass");

    git(&repo.0, &["commit", "-q", "-am", "fix"]);
    let (done, _) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !done,
        "a commit must be verified itself before it completes"
    );
    assert_eq!(
        decision(&check(&store, Candidate::Ref("HEAD".into()))),
        "accepted"
    );
    let (done, record) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(done, "{record}");
    assert!(hook_check(&store, "HEAD").is_ok_and(|(ok, _)| ok));
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[test]
fn worktree_is_verified_as_one_frozen_commit() {
    let (repo, _authority, store) = buggy_task("t-snap");
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    std::fs::write(repo.0.join("notes.txt"), "untracked\n").unwrap_or_default();
    let status_before = git_out(&repo.0, &["status", "--porcelain"]);
    let d = check(&store, Candidate::Worktree);
    assert_eq!(d["candidate_id"], "worktree", "{d}");
    let content = d["content_id"].as_str().unwrap_or("");
    assert_eq!(content.len(), 40, "{d}");
    // the gates read the frozen commit: the edit to app.py (the check
    // passes) and the untracked file (outside the envelope) are in it
    assert_eq!(d["requirements"][0]["state"], "verified", "{d}");
    assert_eq!(
        d["gates"]["envelope"]["violations"][0]["path"], "notes.txt",
        "{d}"
    );
    // it was built in a clean view: nothing is written to the agent's repository
    assert_eq!(git_out(&repo.0, &["cat-file", "-t", content]), "");
    // freezing touches neither the index nor HEAD
    assert_eq!(git_out(&repo.0, &["status", "--porcelain"]), status_before);
    let (done, _) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(!done, "a working-tree verification never completes a task");
}

#[test]
fn scope_budget_and_tampering() {
    let (repo, _authority, store) = buggy_task("t3");
    std::fs::write(repo.0.join("README.md"), "scope creep\n").unwrap_or_default();
    let creep = check(&store, Candidate::Worktree);
    assert_eq!(decision(&creep), "rejected");
    assert_eq!(
        creep["gates"]["envelope"]["violations"][0]["path"],
        "README.md"
    );
    for _ in 0..3 {
        check(&store, Candidate::Worktree);
    }
    let spent = check(&store, Candidate::Worktree);
    assert!(
        spent["reasons"][0]
            .as_str()
            .unwrap_or("")
            .contains("budget exhausted"),
        "{spent}"
    );

    let file = std::fs::read_dir(&store.dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("contract-r1"))
        })
        .unwrap_or_default();
    let text = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .replace("make add() add", "anything goes");
    std::fs::write(&file, text).unwrap_or_default();
    assert!(
        store
            .contract()
            .err()
            .unwrap_or_default()
            .contains("tampered")
    );
}

#[test]
fn revisions_void_prior_acceptance() {
    let repo = scratch("repo2");
    let authority = scratch("authority2");
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "ok"]);
    create(&authority.0, &spec(&repo.0, "t2")).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, "t2", &repo.0).unwrap_or_else(|e| panic!("{e}"));
    let ok = verify(&store, &Candidate::Ref("HEAD".into())).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(decision(&ok), "accepted", "{ok}");
    assert!(
        store.revise(&json!({"objective": "x"}), "", "why").is_err(),
        "actor required"
    );
    let revised = store
        .revise(
            &json!({"objective": "add and document"}),
            "lead",
            "scope grew",
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revised["revision"], 2);
    let (done, record) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !done,
        "acceptance under revision 1 does not complete revision 2: {record}"
    );
}

/// A task over a repository with a test file the agent could weaken.
fn guarded_task(name: &str, guardrails: Value) -> (Dir, Dir, Store) {
    let repo = scratch(&format!("{name}-repo"));
    let authority = scratch(&format!("{name}-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a - b\n").unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(
        repo.0.join("tests/test_app.py"),
        "from app import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "buggy"]);
    let mut s = spec(&repo.0, name);
    s["envelope"]["permitted_paths"] = json!(["app.py", "tests/*"]);
    s["guardrails"] = guardrails;
    create(&authority.0, &s).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, name, &repo.0).unwrap_or_else(|e| panic!("{e}"));
    (repo, authority, store)
}

const FIXED: &str = "def add(a, b):\n    return a + b\n";
const SKIPPED: &str = "import pytest\nfrom app import add\n\n\n@pytest.mark.skip\ndef test_add():\n    assert add(1, 2) == 3\n";

#[test]
fn guardrails_flag_weakened_tests_and_suppressions_for_review() {
    // v0.4 (emi-lab GV2): a skip on an existing test needs review by
    // default; restored trusted tests are what reject a hidden bug
    let (repo, _a, store) = guarded_task("g1", json!({}));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    std::fs::write(repo.0.join("tests/test_app.py"), SKIPPED).unwrap_or_default();
    let skipped = check(&store, Candidate::Worktree);
    assert_eq!(decision(&skipped), "needs_review", "{skipped}");
    assert_eq!(skipped["gates"]["guardrails"]["state"], "needs_review");
    assert_eq!(
        skipped["gates"]["guardrails"]["violations"][0]["id"],
        "tests.skip_added"
    );
    assert!(
        skipped["reasons"][0]
            .as_str()
            .unwrap_or("")
            .contains("tests.skip_added")
    );

    git(&repo.0, &["checkout", "-q", "tests/test_app.py"]);
    std::fs::write(
        repo.0.join("app.py"),
        "def add(a, b):\n    return a + b  # noqa\n",
    )
    .unwrap_or_default();
    let noqa = check(&store, Candidate::Worktree);
    assert_eq!(decision(&noqa), "needs_review", "{noqa}");
    assert!(
        noqa["reasons"][0]
            .as_str()
            .unwrap_or("")
            .contains("suppress.added")
    );

    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    assert_eq!(decision(&check(&store, Candidate::Worktree)), "accepted");
}

#[test]
fn guardrails_can_be_disabled_or_retiered_in_the_contract() {
    let (repo, _a, store) = guarded_task("g2", json!(false));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    std::fs::write(repo.0.join("tests/test_app.py"), SKIPPED).unwrap_or_default();
    let v = check(&store, Candidate::Worktree);
    assert_eq!(decision(&v), "accepted", "{v}");
    assert_eq!(v["gates"]["guardrails"]["state"], "not_required");

    let (repo, _a, store) = guarded_task("g3", json!({"tiers": {"tests.skip_added": "review"}}));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    std::fs::write(repo.0.join("tests/test_app.py"), SKIPPED).unwrap_or_default();
    assert_eq!(
        decision(&check(&store, Candidate::Worktree)),
        "needs_review"
    );
    let repo_path = repo.0.clone();
    assert!(
        emi_controller::create(&scratch("g4-authority").0, &{
            let mut s = spec(&repo_path, "g4");
            s["guardrails"] = json!({"tiers": {"tests": "sometimes"}});
            s
        })
        .is_err(),
        "an invalid policy is refused at freeze"
    );
}

/// A repository with a buggy `add`, a visible test, and a task whose
/// verification trusts `tests/*` and holds out one more test.
fn verification_task(name: &str, extra: Value) -> (Dir, Dir, Store) {
    let repo = scratch(&format!("{name}-repo"));
    let authority = scratch(&format!("{name}-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a - b\n").unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(
        repo.0.join("tests/test_app.py"),
        "import app\n\ndef test_one():\n    assert app.add(1, 2) == 3\n\ndef test_two():\n    assert app.add(0, 0) == 0\n\ntest_one()\ntest_two()\n",
    )
    .unwrap_or_default();
    std::fs::write(repo.0.join("skip.txt"), "").unwrap_or_default();
    // a stand-in test runner: lists test ids, leaving out those in skip.txt
    std::fs::write(
        repo.0.join("runner.py"),
        "import glob, re\nskip = set(open('skip.txt').read().split())\nfor f in sorted(glob.glob('tests/test_*.py')):\n    for n in re.findall(r'^def (test_\\w+)', open(f).read(), re.M):\n        if n not in skip:\n            print(f'{f}::{n}')\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "buggy"]);
    let run = |f: &str| {
        json!({"command": ["python3", "-c", format!("import runpy; runpy.run_path('{f}')")],
               "success": {"exit_code": 0}, "timeout_s": 30})
    };
    let mut spec = json!({
        "task_id": name, "repo": repo.0, "baseline": "HEAD", "objective": "make add() add",
        "requirements": [{"id": "R1", "acceptance": "add adds", "checks": ["visible", "hidden"]}],
        "checks": {"visible": run("tests/test_app.py"), "hidden": run("tests/test_hidden.py")},
        "envelope": {"permitted_paths": ["app.py", "tests/*", "skip.txt"]},
        "verification": {
            "trusted": ["tests/*"],
            "held_out": {"tests/test_hidden.py": {"content": "import app\nassert app.add(2, 5) == 7\n"}},
        },
    });
    if let (Some(v), Some(extra)) = (spec["verification"].as_object_mut(), extra.as_object()) {
        v.extend(extra.clone());
    }
    create(&authority.0, &spec).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, name, &repo.0).unwrap_or_else(|e| panic!("{e}"));
    (repo, authority, store)
}

#[test]
fn trusted_tests_are_restored_and_held_out_tests_catch_special_casing() {
    let (repo, _a, store) = verification_task("v1", json!({}));
    // tampering: the visible test is gutted, the bug stays
    std::fs::write(repo.0.join("tests/test_app.py"), "assert True\n").unwrap_or_default();
    let tampered = check(&store, Candidate::Worktree);
    assert_eq!(decision(&tampered), "rejected", "{tampered}");
    assert_eq!(
        tampered["gates"]["verification"]["restored"],
        json!(["tests/test_app.py"])
    );
    git(&repo.0, &["checkout", "-q", "tests/test_app.py"]);

    // special-casing: the visible test passes, the held-out one does not
    std::fs::write(
        repo.0.join("app.py"),
        "def add(a, b):\n    if (a, b) == (1, 2):\n        return 3\n    return a - b\n",
    )
    .unwrap_or_default();
    let special = check(&store, Candidate::Worktree);
    assert_eq!(decision(&special), "rejected", "{special}");
    let states: Vec<&Value> = special["checks"]
        .as_array()
        .map(|c| c.iter().map(|r| &r["status"]).collect())
        .unwrap_or_default();
    assert!(states.contains(&&json!("failed")), "{special}");

    // an honest fix with a new test file is accepted
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    std::fs::write(
        repo.0.join("tests/test_more.py"),
        "import app\nassert app.add(3, 4) == 7\n",
    )
    .unwrap_or_default();
    let honest = check(&store, Candidate::Worktree);
    assert_eq!(decision(&honest), "accepted", "{honest}");
    assert_eq!(honest["gates"]["verification"]["state"], "pass");
    assert_eq!(
        honest["gates"]["verification"]["held_out"],
        json!(["tests/test_hidden.py"])
    );

    // a change to a trusted file still ships, so it needs review
    std::fs::write(
        repo.0.join("tests/test_app.py"),
        "# reworded\nimport app\n\ndef test_one():\n    assert app.add(1, 2) == 3\n\ndef test_two():\n    assert app.add(0, 0) == 0\n\ntest_one()\ntest_two()\n",
    )
    .unwrap_or_default();
    let reviewed = check(&store, Candidate::Worktree);
    assert_eq!(decision(&reviewed), "needs_review", "{reviewed}");
}

#[test]
fn inventory_rejects_tests_removed_by_configuration() {
    let inventory = json!({"inventory": {"command": ["python3", "runner.py"], "format": "pytest"}});
    let (repo, _a, store) = verification_task("v2", inventory);
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    let fixed = check(&store, Candidate::Worktree);
    assert_eq!(decision(&fixed), "accepted", "{fixed}");
    assert_eq!(fixed["gates"]["verification"]["inventory"]["baseline"], 2);

    // an untrusted configuration file deselects a test
    std::fs::write(repo.0.join("skip.txt"), "test_two\n").unwrap_or_default();
    let skipped = check(&store, Candidate::Worktree);
    assert_eq!(decision(&skipped), "rejected", "{skipped}");
    assert_eq!(
        skipped["gates"]["verification"]["inventory"]["missing"],
        json!(["tests/test_app.py::test_two"])
    );
}

#[test]
fn verification_policy_is_validated_at_freeze() {
    let repo = scratch("v3-repo");
    let authority = scratch("v3-authority");
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "c"]);
    let mut bad = spec(&repo.0, "v3");
    bad["verification"] = json!({"held_out": {"../escape.py": {"content": "x"}}});
    assert!(create(&authority.0, &bad).is_err_and(|e| e.contains("inside the workspace")));
    bad["verification"] = json!({"trusted": ["tests/*"], "restore": true});
    assert!(create(&authority.0, &bad).is_err_and(|e| e.contains("unknown key")));
}

const NESTED: &str = "def classify(n, strict):\n    if n is not None:\n        if n > 0:\n            if strict and n > 100:\n                return 'big'\n            else:\n                return 'pos'\n        else:\n            if n == 0:\n                return 'zero'\n            return 'neg'\n    return 'none'\n";
const FLAT: &str = "def classify(n, strict):\n    if n is None:\n        return 'none'\n    if n > 0:\n        return 'big' if strict and n > 100 else 'pos'\n    if n == 0:\n        return 'zero'\n    return 'neg'\n";

/// A refactoring task: behaviour kept by the trusted tests, simplification
/// measured by the refactor gate.
fn refactor_task(name: &str) -> (Dir, Dir, Store) {
    let repo = scratch(&format!("{name}-repo"));
    let authority = scratch(&format!("{name}-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), NESTED).unwrap_or_default();
    std::fs::write(repo.0.join("util.py"), "X = 1\n").unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(
        repo.0.join("tests/test_app.py"),
        "from app import classify\nassert classify(None, True) == 'none'\nassert classify(500, True) == 'big'\nassert classify(500, False) == 'pos'\nassert classify(0, True) == 'zero'\nassert classify(-3, True) == 'neg'\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "nested"]);
    let spec = json!({
        "task_id": name, "repo": repo.0, "baseline": "HEAD", "objective": "simplify classify()",
        "requirements": [{"id": "R1", "acceptance": "same behaviour", "checks": ["tests"]}],
        "checks": {"tests": {"command": ["python3", "-c", "import sys; sys.path.insert(0, '.'); import runpy; runpy.run_path('tests/test_app.py')"],
                             "success": {"exit_code": 0}, "timeout_s": 30}},
        "envelope": {"permitted_paths": ["app.py", "helpers.py", "util.py", "tests/*"]},
        "verification": {"trusted": ["tests/*"]},
        "refactor": {"targets": ["app.py"], "min_cognitive_reduction": 0.2},
    });
    create(&authority.0, &spec).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, name, &repo.0).unwrap_or_else(|e| panic!("{e}"));
    (repo, authority, store)
}

#[test]
fn refactors_must_simplify_and_keep_behaviour() {
    let (repo, _a, store) = refactor_task("rf");
    // a real flattening that keeps behaviour
    std::fs::write(repo.0.join("app.py"), FLAT).unwrap_or_default();
    let flat = check(&store, Candidate::Worktree);
    assert_eq!(decision(&flat), "accepted", "{flat}");
    let r = &flat["gates"]["refactor"];
    assert!(
        r["cognitive_reduction"].as_f64().unwrap_or(0.0) >= 0.2,
        "{r}"
    );

    // the same shape with a behaviour change: the trusted tests catch it
    std::fs::write(repo.0.join("app.py"), FLAT.replace("n == 0", "n == 1")).unwrap_or_default();
    assert_eq!(decision(&check(&store, Candidate::Worktree)), "rejected");

    // cosmetic change only: behaviour kept, nothing simpler
    std::fs::write(
        repo.0.join("app.py"),
        NESTED.replace("strict", "strict_mode"),
    )
    .unwrap_or_default();
    let cosmetic = check(&store, Candidate::Worktree);
    assert_eq!(decision(&cosmetic), "rejected", "{cosmetic}");
    assert!(
        cosmetic["reasons"]
            .to_string()
            .contains("cognitive complexity fell")
    );

    // moving the function to a new module is not a simplification
    std::fs::write(repo.0.join("helpers.py"), NESTED).unwrap_or_default();
    std::fs::write(
        repo.0.join("app.py"),
        "from helpers import classify  # noqa: F401\n",
    )
    .unwrap_or_default();
    let moved = check(&store, Candidate::Worktree);
    assert_eq!(decision(&moved), "rejected", "{moved}");
    std::fs::remove_file(repo.0.join("helpers.py")).unwrap_or_default();

    // ... nor is moving it into a file that already exists
    std::fs::write(repo.0.join("util.py"), format!("X = 1\n\n\n{NESTED}")).unwrap_or_default();
    git(&repo.0, &["checkout", "-q", "app.py"]);
    std::fs::write(
        repo.0.join("app.py"),
        "from util import classify  # noqa: F401\n",
    )
    .unwrap_or_default();
    let moved = check(&store, Candidate::Worktree);
    assert_eq!(decision(&moved), "rejected", "{moved}");
}

#[test]
fn refactor_policy_is_validated_at_freeze() {
    let repo = scratch("rf-bad-repo");
    let authority = scratch("rf-bad-authority");
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("a.py"), "x = 1\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "c"]);
    for bad in [
        json!({"targets": []}),
        json!({"targets": ["a.py"], "min_cognitive_reduction": 0}),
        json!({"targets": ["a.py"], "typo": 1}),
        json!({"targets": ["a.py"]}),
    ] {
        let mut s = spec(&repo.0, "rf-bad");
        s["refactor"] = bad.clone();
        assert!(create(&authority.0, &s).is_err(), "{bad} accepted");
    }
}

/// From the independent review (2026-09-28): what the checks do at run
/// time cannot change what is measured or what the trusted tests say.
#[test]
fn run_time_rewrites_cannot_fake_a_refactor_or_a_passing_test() {
    if !emi_exec::bwrap_available() {
        return;
    }
    let base = "def f(a, b, c):\n    if a:\n        if b:\n            if c:\n                return 1\n    return 0\n";
    let repo = scratch("rt-repo");
    let authority = scratch("rt-authority");
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), base).unwrap_or_default();
    std::fs::write(
        repo.0.join("test_check.py"),
        "import app\nfrom itertools import product\nfor a, b, c in product((False, True), repeat=3):\n    assert app.f(a, b, c) == int(a and b and c)\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "base"]);
    let run = json!({"command": ["python3", "test_check.py"], "success": {"exit_code": 0}, "timeout_s": 30});
    let spec = json!({
        "task_id": "rt", "repo": repo.0, "baseline": "HEAD", "objective": "simplify f",
        "requirements": [{"id": "R1", "acceptance": "same outputs", "checks": ["first", "second"]}],
        "checks": {"first": run, "second": run},
        "envelope": {"permitted_paths": ["app.py", "test_check.py"]},
        "verification": {"trusted": ["test_check.py"]},
        "refactor": {"targets": ["app.py"], "min_cognitive_reduction": 0.2},
    });
    create(&authority.0, &spec).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, "rt", &repo.0).unwrap_or_else(|e| panic!("{e}"));

    // 1. the reviewer's exploit: the nested code stays in the commit, and
    //    rewrites itself to a trivial version when imported
    std::fs::write(
        repo.0.join("app.py"),
        format!("{base}\nfrom pathlib import Path\nPath(__file__).write_text('def f(a, b, c):\\n    return 0\\n')\n"),
    )
    .unwrap_or_default();
    git(&repo.0, &["commit", "-qam", "self-rewriting candidate"]);
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_ne!(decision(&d), "accepted", "{d}");
    assert_eq!(
        d["gates"]["refactor"]["state"], "fail",
        "{}",
        d["gates"]["refactor"]
    );

    // 2. candidate code that overwrites the trusted test at run time: the
    //    test is read-only, so the write fails instead of the test passing
    std::fs::write(
        repo.0.join("app.py"),
        "from pathlib import Path\nPath('test_check.py').write_text('pass\\n')\n\ndef f(a, b, c):\n    return 0\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["commit", "-qam", "tampers with its test"]);
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "rejected", "{d}");

    // 3. a trusted test replaced by a link out of the workspace: restoring
    //    it replaces the link and never writes through it
    let outside = authority.0.join("outside.txt");
    std::fs::write(&outside, "untouched").unwrap_or_default();
    git(&repo.0, &["checkout", "-q", "HEAD~2", "--", "app.py"]);
    std::fs::remove_file(repo.0.join("test_check.py")).unwrap_or_default();
    std::os::unix::fs::symlink(&outside, repo.0.join("test_check.py")).unwrap_or_default();
    let _ = check(&store, Candidate::Worktree);
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap_or_default(),
        "untouched"
    );
}

#[test]
fn code_moved_under_tests_or_left_unparseable_is_not_a_simplification() {
    let (repo, _a, store) = refactor_task("rf-hide");
    // logic moved into a test-directory helper that defines no tests
    std::fs::write(repo.0.join("tests/helpers.py"), NESTED).unwrap_or_default();
    std::fs::write(
        repo.0.join("app.py"),
        "import sys\nsys.path.insert(0, 'tests')\nfrom helpers import classify  # noqa: F401,E402\n",
    )
    .unwrap_or_default();
    let d = check(&store, Candidate::Worktree);
    assert_ne!(decision(&d), "accepted", "{d}");
    assert_eq!(
        d["gates"]["refactor"]["state"], "fail",
        "{}",
        d["gates"]["refactor"]
    );
    std::fs::remove_file(repo.0.join("tests/helpers.py")).unwrap_or_default();

    // a changed file that does not parse is unmeasured, never simpler
    std::fs::write(repo.0.join("app.py"), format!("{FLAT}\ndef broken(:\n")).unwrap_or_default();
    let d = check(&store, Candidate::Worktree);
    assert_eq!(
        d["gates"]["refactor"]["state"], "unverified",
        "{}",
        d["gates"]["refactor"]
    );
    assert_ne!(decision(&d), "accepted");
}

#[test]
fn checks_cannot_read_the_authority_or_the_source_repository() {
    if !emi_exec::bwrap_available() {
        return;
    }
    // outside /tmp, which the sandbox replaces anyway
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let mk = |n: &str| {
        let d = base.join(format!("emi-ctl-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_default();
        Dir(d)
    };
    let (repo, authority) = (mk("hide-repo"), mk("hide-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "app"]);
    let probe = format!(
        "import os, sys; seen = [p for p in ({:?}, {:?}) if os.listdir(p)]; sys.exit(1 if seen else 0)",
        authority.0.display().to_string(),
        repo.0.display().to_string()
    );
    let mut s = spec(&repo.0, "hide");
    s["checks"]["unit"]["command"] = json!(["python3", "-c", probe]);
    create(&authority.0, &s).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, "hide", &repo.0).unwrap_or_else(|e| panic!("{e}"));
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "accepted", "{d}");
}

#[test]
fn edits_to_linked_dependencies_void_the_checks_until_re_pinned() {
    let repo = scratch("deps-repo");
    let authority = scratch("deps-authority");
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a - b\n").unwrap_or_default();
    std::fs::write(repo.0.join(".gitignore"), ".deps/\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "buggy"]);
    // an ignored, linked "test runner" the agent could edit outside git
    let runner = repo.0.join(".deps/runner.py");
    std::fs::create_dir_all(repo.0.join(".deps")).unwrap_or_default();
    std::fs::write(
        &runner,
        "import app, sys\nsys.exit(0 if app.add(1, 2) == 3 else 1)\n",
    )
    .unwrap_or_default();
    let mut s = spec(&repo.0, "deps");
    s["checks"]["unit"]["command"] =
        json!(["python3", "-c", "exec(open('.deps/runner.py').read())"]);
    s["execution"] = json!({"link": [".deps"]});
    let contract = create(&authority.0, &s).unwrap_or_else(|e| panic!("{e}"));
    assert!(contract["dependencies"][".deps"].is_string(), "{contract}");
    let store = Store::open(&authority.0, "deps", &repo.0).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(decision(&check(&store, Candidate::Worktree)), "rejected");

    std::fs::write(&runner, "import sys\nsys.exit(0)\n").unwrap_or_default();
    let gamed = check(&store, Candidate::Worktree);
    assert_eq!(decision(&gamed), "incomplete", "{gamed}");
    assert_eq!(gamed["gates"]["dependencies"]["state"], "unverified");

    store
        .revise(
            &json!({"execution": {"link": [".deps"]}}),
            "lead",
            "new runner",
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let repinned = check(&store, Candidate::Worktree);
    assert_eq!(
        repinned["gates"]["dependencies"]["state"], "pass",
        "{repinned}"
    );
}

#[test]
fn an_approval_completes_exactly_one_needs_review_decision() {
    let (repo, _a, store) = guarded_task("ap", json!({"tiers": {"tests.skip_added": "review"}}));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    std::fs::write(repo.0.join("tests/test_app.py"), SKIPPED).unwrap_or_default();
    git(&repo.0, &["commit", "-q", "-am", "fix, skip"]);
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "needs_review", "{d}");
    let items: Vec<String> = d["review_items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i.as_str().map(str::to_owned))
        .collect();
    assert!(items.iter().any(|i| i.starts_with("guard:")), "{items:?}");
    assert!(!complete(&store, "HEAD").is_ok_and(|(done, _)| done));

    let approve = |items: &[String], actor: &str| {
        emi_controller::approve(&store, "HEAD", actor, "skip is temporary", items)
    };
    assert!(approve(&items, "").is_err(), "an actor is required");
    assert!(approve(&[], "lead").is_err(), "every item must be named");
    let mut extra = items.clone();
    extra.push("guard:ci.edited:.github/x.yml".into());
    assert!(
        approve(&extra, "lead").is_err(),
        "no waiver beyond the decision"
    );
    approve(&items, "lead").unwrap_or_else(|e| panic!("{e}"));

    let (done, record) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(done, "{record}");
    assert_eq!(record["approval"]["actor"], "lead");

    // a new verification is a new decision: the approval does not carry over
    check(&store, Candidate::Ref("HEAD".into()));
    assert!(!complete(&store, "HEAD").is_ok_and(|(done, _)| done));
}

#[test]
fn a_committed_entry_cannot_stand_in_for_a_linked_dependency() {
    let (repo, _authority, store) = {
        let repo = scratch("shadow-repo");
        let authority = scratch("shadow-authority");
        git(&repo.0, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n")
            .unwrap_or_default();
        git(&repo.0, &["add", "-A"]);
        git(&repo.0, &["commit", "-q", "-m", "app"]);
        let mut s = spec(&repo.0, "shadow");
        s["execution"] = json!({"link": [".venv"]});
        s["envelope"]["permitted_paths"] = json!(["app.py", ".venv"]);
        create(&authority.0, &s).unwrap_or_else(|e| panic!("{e}"));
        let store = Store::open(&authority.0, "shadow", &repo.0).unwrap_or_else(|e| panic!("{e}"));
        (repo, authority, store)
    };
    // the candidate commits `.venv` as a link to an environment of its choosing
    #[cfg(unix)]
    std::os::unix::fs::symlink("/usr", repo.0.join(".venv")).unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "venv"]);
    let err = verify(&store, &Candidate::Ref("HEAD".into()))
        .err()
        .unwrap_or_default();
    assert!(err.contains("linked from the repository"), "{err}");
}

#[test]
fn an_edited_test_file_outside_the_trusted_set_needs_review() {
    // a contract whose trusted globs miss the test layout (a hand-written
    // list, say) must not turn a weakened test into a silent acceptance
    let (repo, _authority, store) =
        verification_task("vuntrusted", json!({"trusted": ["nothing/*"]}));
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    std::fs::write(
        repo.0.join("tests/test_app.py"),
        "import app\n\ndef test_one():\n    assert app.add(1, 2) == 3\n\ndef test_two():\n    pass\n\ntest_one()\ntest_two()\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(
        &repo.0,
        &["commit", "-q", "-m", "fix, and drop an assertion"],
    );
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "needs_review", "{d}");
    assert_eq!(
        d["gates"]["verification"]["untrusted_test_changes"],
        json!(["tests/test_app.py"]),
        "{d}"
    );
    assert!(
        d["review_items"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|i| i == "untrusted_test:tests/test_app.py"),
        "{d}"
    );
}

#[test]
fn added_test_runner_configuration_needs_review() {
    let (repo, _authority, store) = verification_task("vconf", json!({}));
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    // restoring trusted files cannot undo a file the baseline never had
    std::fs::write(
        repo.0.join("tests/conftest.py"),
        "def pytest_runtest_makereport(item, call):\n    pass\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "fix, plus a conftest"]);
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "needs_review", "{d}");
    assert_eq!(
        d["gates"]["verification"]["added_runner_config"],
        json!(["tests/conftest.py"]),
        "{d}"
    );
    assert!(
        d["review_items"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|i| i == "runner_config:tests/conftest.py"),
        "{d}"
    );
    assert!(
        d.get("limits").is_some(),
        "no inventory: the limit is recorded"
    );
}

#[test]
fn revisions_are_validated_like_creation() {
    let (_repo, _authority, store) = buggy_task("rv");
    let revise = |changes: Value| store.revise(&changes, "lead", "why");
    let refused = [
        json!({"requirements": []}),
        json!({"checks": {"unit": {"command": "python3 -c pass", "success": {"exit_code": 0}}}}),
        json!({"requirements": [{"id": "R1", "acceptance": "x", "checks": ["missing"]}]}),
        json!({"structural": {"probe": "cobol", "source": "."}}),
        json!({"execution": {"isolation": "bwarp"}}),
        json!({"checks": {"unit": {"command": ["true"], "success": {"exit_code": 0}, "timeout_s": 1e30}}}),
        json!({"measurement_contract_id": "x"}),
        json!({"dependencies": {}}),
    ];
    for changes in refused {
        assert!(revise(changes.clone()).is_err(), "accepted {changes}");
    }
    // the structural policy's derived ids follow a revision of it
    let before = store.contract().unwrap_or_default();
    let after = revise(json!({"structural": {"probe": "python", "source": ".", "allowance": 3}}))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(after["gate_policy_id"], before["gate_policy_id"]);
}

#[test]
fn creation_bounds_timeouts_and_validates_execution() {
    let repo = scratch("cv-repo");
    let authority = scratch("cv-authority");
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "x = 1\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "a"]);
    let with = |patch: Value| {
        let mut s = spec(&repo.0, "cv");
        if let (Some(s), Some(p)) = (s.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                s.insert(k.clone(), v.clone());
            }
        }
        s
    };
    let refused = [
        json!({"checks": {"unit": {"command": ["true"], "success": {"exit_code": 0}, "timeout_s": 1e30}}}),
        json!({"verification": {"inventory": {"command": ["true"], "timeout_s": -1}}}),
        json!({"execution": {"network": "yes"}}),
        json!({"execution": {"link": ["../outside"]}}),
        json!({"completion_policy": {"require_all_requirements_verified": "no"}}),
    ];
    for patch in refused {
        assert!(
            create(&authority.0, &with(patch.clone())).is_err(),
            "accepted {patch}"
        );
    }
    let go = with(json!({"structural": {"probe": "go", "source": "."}}));
    assert!(
        create(&authority.0, &go).is_ok(),
        "Go is a measured language"
    );
}

#[test]
fn a_damaged_evidence_log_is_an_error_not_a_shorter_log() {
    let (repo, authority, store) = buggy_task("dmg");
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a + b\n").unwrap_or_default();
    git(&repo.0, &["commit", "-q", "-am", "fix"]);
    assert_eq!(
        decision(&check(&store, Candidate::Ref("HEAD".into()))),
        "accepted"
    );
    // a later row torn in half (a crash, or two writers interleaving)
    let log = authority.0.join("dmg").join("evidence.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap_or_default();
    text.push_str("{\"kind\": \"decision\", \"record\": {\"decision\": \"rej\n");
    std::fs::write(&log, text).unwrap_or_default();
    let err = complete(&store, "HEAD").err().unwrap_or_default();
    assert!(err.contains("evidence log is damaged"), "{err}");
}

#[test]
fn concurrent_verifications_are_all_charged_and_logged_whole() {
    let (_repo, authority, _store) = buggy_task("conc");
    let runs: Vec<_> = (0..4)
        .map(|_| {
            let (authority, repo) = (authority.0.clone(), _repo.0.clone());
            std::thread::spawn(move || {
                let store =
                    Store::open(&authority, "conc", &repo).unwrap_or_else(|e| panic!("{e}"));
                verify(&store, &Candidate::Ref("HEAD".into())).is_ok()
            })
        })
        .collect();
    assert!(runs.into_iter().all(|t| t.join().unwrap_or(false)));
    let store = Store::open(&authority.0, "conc", &_repo.0).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        store.state().unwrap_or_default()["cumulative"]["retries"],
        4
    );
    assert_eq!(store.rows("decision").map(|r| r.len()).unwrap_or(0), 4);
}

#[test]
fn a_dummy_test_does_not_hide_moved_product_code() {
    let (repo, _a, store) = refactor_task("rf-dummy");
    // the nested logic moved into a product module that also defines one
    // trivial test, which used to leave the module out of the measurement
    std::fs::write(
        repo.0.join("helpers.py"),
        format!("{NESTED}\n\ndef test_dummy():\n    assert True\n"),
    )
    .unwrap_or_default();
    std::fs::write(
        repo.0.join("app.py"),
        "from helpers import classify  # noqa: F401\n",
    )
    .unwrap_or_default();
    let d = check(&store, Candidate::Worktree);
    assert_eq!(
        d["gates"]["refactor"]["state"], "fail",
        "{}",
        d["gates"]["refactor"]
    );
    assert!(d["gates"]["refactor"]["after"]["files"]["helpers.py"].is_object());
}

#[test]
fn task_ids_never_become_paths_outside_the_authority() {
    let (_repo, authority, _store) = buggy_task("tid");
    for id in ["../tid", "tid/../../x", ""] {
        assert!(Store::open(&authority.0, id, &_repo.0).is_err(), "{id:?}");
    }
}

/// A task whose trusted test runs every `test_*` function it defines, with
/// shaken reruns; `app` is the candidate's code.
fn shaken_task(name: &str, app: &str) -> (Dir, Dir, Store) {
    let repo = scratch(&format!("{name}-repo"));
    let authority = scratch(&format!("{name}-authority"));
    git(&repo.0, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.0.join("app.py"), "def add(a, b):\n    return a - b\n").unwrap_or_default();
    std::fs::create_dir_all(repo.0.join("tests")).unwrap_or_default();
    std::fs::write(
        repo.0.join("tests/test_app.py"),
        "import app\n\ndef test_add():\n    assert app.add(1, 2) == 3\n\nfor _n, _f in list(globals().items()):\n    if _n.startswith('test_'):\n        _f()\n",
    )
    .unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "buggy"]);
    let spec = json!({
        "task_id": name, "repo": repo.0, "baseline": "HEAD", "objective": "make add() add",
        "requirements": [{"id": "R1", "acceptance": "add adds", "checks": ["tests"]}],
        "checks": {"tests": {"command": ["python3", "-c", "import runpy; runpy.run_path('tests/test_app.py')"],
                             "success": {"exit_code": 0}, "timeout_s": 30}},
        "envelope": {"permitted_paths": ["app.py"]},
        "verification": {"trusted": ["tests/*"], "shake": {"rounds": 1}},
    });
    create(&authority.0, &spec).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(&authority.0, name, &repo.0).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(repo.0.join("app.py"), app).unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "candidate"]);
    (repo, authority, store)
}

#[test]
fn an_honest_fix_passes_its_shaken_reruns() {
    let (_repo, _authority, store) =
        shaken_task("shake-honest", "def add(a, b):\n    return a + b\n");
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "accepted", "{d}");
    assert_eq!(d["gates"]["verification"]["shake"]["state"], "pass", "{d}");
    assert_eq!(
        d["gates"]["verification"]["shake"]["tests_renamed"], 1,
        "{d}"
    );
}

#[test]
fn code_that_depends_on_the_calling_test_needs_review() {
    // passes only when called from a function of that exact name
    let app = "import sys\n\ndef add(a, b):\n    return a + b if sys._getframe(1).f_code.co_name == 'test_add' else 0\n";
    let (_repo, _authority, store) = shaken_task("shake-named", app);
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "needs_review", "{d}");
    assert_eq!(
        d["gates"]["verification"]["shake"]["state"], "disagree",
        "{d}"
    );
    assert!(
        d["review_items"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|i| i == "shake:disagree"),
        "{d}"
    );
}

/// A verification task whose `app.py` is fixed in a commit, with the
/// reporting agent's escalation already open.
fn escalated_task(name: &str) -> (Dir, Dir, Store) {
    let (repo, authority, store) = verification_task(name, json!({}));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    git(&repo.0, &["commit", "-q", "-am", "fix"]);
    let row = emi_controller::escalate(&store, Some("HEAD"), "test_two contradicts the spec")
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(row["index"], 1, "{row}");
    assert!(row["candidate_id"].is_string(), "{row}");
    (repo, authority, store)
}

fn items_of(d: &Value) -> Vec<String> {
    d["review_items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn an_open_escalation_holds_a_passing_candidate_for_review() {
    let (_repo, _a, store) = escalated_task("esc1");
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "needs_review", "{d}");
    assert_eq!(d["gates"]["escalation"]["state"], "open", "{d}");
    assert_eq!(items_of(&d), ["escalation:1"], "{d}");
    assert!(
        d["reasons"][0]
            .as_str()
            .is_some_and(|r| r.contains("escalation 1") && r.contains("test_two contradicts")),
        "{d}"
    );
    // reporting a conflict spends nothing
    assert_eq!(d["budget"]["retries_used"], 1, "{d}");
}

#[test]
fn an_escalation_is_approved_by_name_and_then_completes() {
    let (_repo, _a, store) = escalated_task("esc2");
    check(&store, Candidate::Ref("HEAD".into()));
    let approve = |items: &[String]| {
        emi_controller::approve(
            &store,
            "HEAD",
            "lead",
            "spec is wrong, test is stale",
            items,
        )
    };
    assert!(approve(&[]).is_err(), "the escalation must be named");
    approve(&["escalation:1".into()]).unwrap_or_else(|e| panic!("{e}"));
    let (done, record) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(done, "{record}");
    assert_eq!(record["approval"]["actor"], "lead");
}

#[test]
fn an_escalation_after_acceptance_blocks_completion_until_verified_again() {
    let (repo, _a, store) = verification_task("esc7", json!({}));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    git(&repo.0, &["commit", "-q", "-am", "fix"]);
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "accepted", "{d}");
    emi_controller::escalate(&store, None, "test_two contradicts the spec")
        .unwrap_or_else(|e| panic!("{e}"));
    let (done, record) = complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(!done, "{record}");
    assert!(
        record["reason"]
            .as_str()
            .is_some_and(|r| r.contains("filed after the latest verification")),
        "{record}"
    );
    let again = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&again), "needs_review", "{again}");
}

#[test]
fn a_revision_resolves_an_escalation() {
    let (_repo, _a, store) = escalated_task("esc3");
    let held = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&held), "needs_review", "{held}");
    store
        .revise(
            &json!({"budgets": {"max_retries": 9}}),
            "lead",
            "fixed the test",
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let after = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&after), "accepted", "{after}");
    assert_eq!(after["gates"]["escalation"]["state"], "none", "{after}");
}

#[test]
fn an_empty_reason_or_unknown_candidate_records_nothing() {
    let (_repo, _a, store) = verification_task("esc4", json!({}));
    for reason in ["", "  \n"] {
        let err = emi_controller::escalate(&store, None, reason).err();
        assert!(err.is_some_and(|e| e.contains("reason")));
    }
    assert!(emi_controller::escalate(&store, Some("no-such-ref"), "why").is_err());
    assert!(store.rows("escalation").is_ok_and(|r| r.is_empty()));
}

#[test]
fn an_escalation_does_not_soften_a_rejection() {
    let (repo, _a, store) = verification_task("esc5", json!({}));
    std::fs::write(repo.0.join("app.py"), FIXED).unwrap_or_default();
    std::fs::write(repo.0.join("outside.txt"), "x\n").unwrap_or_default();
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "fix, and stray file"]);
    emi_controller::escalate(&store, None, "tests look wrong").unwrap_or_else(|e| panic!("{e}"));
    let d = check(&store, Candidate::Ref("HEAD".into()));
    assert_eq!(decision(&d), "rejected", "{d}");
    assert_eq!(d["gates"]["escalation"]["state"], "open", "{d}");
}

#[test]
fn status_lists_open_escalations_and_the_hook_mentions_them() {
    let (_repo, _a, store) = escalated_task("esc6");
    let c = store.contract().unwrap_or_else(|e| panic!("{e}"));
    let cid = c["contract_id"].as_str().unwrap_or("");
    let open = emi_controller::open_escalations(&store, cid).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["index"], 1);
    assert_eq!(open[0]["reason"], "test_two contradicts the spec");
    let (ok, msg) = hook_check(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(!ok && msg.contains("1 open escalation(s)"), "{msg}");
}
