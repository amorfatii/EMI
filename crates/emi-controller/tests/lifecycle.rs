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
