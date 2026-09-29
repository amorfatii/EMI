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
    // the gates read the frozen commit: the untracked file is in it (and
    // outside the envelope), the edit to app.py too
    assert_eq!(
        git_out(&repo.0, &["show", &format!("{content}:app.py")]),
        "def add(a, b):\n    return a + b\n"
    );
    assert_eq!(
        d["gates"]["envelope"]["violations"][0]["path"], "notes.txt",
        "{d}"
    );
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
