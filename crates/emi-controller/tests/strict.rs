//! What a contract may say and how a decision reduces: unsupported or
//! ignored contract content is refused at freeze, and an acceptance cannot
//! rest on a requirement that was not verified.

use std::path::Path;
use std::process::Command;

use emi_controller::{Candidate, Store, create, verify};
use serde_json::{Value, json};
use tempfile::TempDir;

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";

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

/// A repository holding `app.py` at `app`, committed, and an empty
/// authority directory outside it.
fn repo_with(app: &str) -> (TempDir, TempDir) {
    let repo = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let authority = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("app.py"), app).unwrap_or_else(|e| panic!("{e}"));
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "base"]);
    (repo, authority)
}

/// The spec `emi task draft` writes in miniature: one requirement, one
/// check that asserts the fix, `app.py` the only file the change may touch.
fn spec(repo: &Path, task: &str) -> Value {
    json!({
        "task_id": task, "repo": repo, "baseline": "HEAD",
        "objective": "make add() add",
        "requirements": [{"id": "R1", "acceptance": "add(1, 2) == 3", "checks": ["unit"]}],
        "checks": {"unit": {"command": ["python3", "-c", "import app; assert app.add(1, 2) == 3"],
                            "success": {"exit_code": 0}, "timeout_s": 30}},
        "envelope": {"permitted_paths": ["app.py"]},
        "budgets": {"max_retries": 4},
    })
}

fn refusal(authority: &Path, spec: &Value) -> String {
    create(authority, spec).err().unwrap_or_default()
}

#[test]
fn a_token_budget_is_refused_because_nothing_measures_tokens() {
    let (repo, authority) = repo_with(BUGGY);
    let mut s = spec(repo.path(), "tok");
    s["budgets"] = json!({"max_retries": 4, "max_tokens": 100_000});
    let err = refusal(authority.path(), &s);
    assert!(
        err.contains("max_tokens") && err.contains("cannot observe token use"),
        "{err}"
    );

    s["budgets"] = json!({"max_retries": 4, "max_time_s": 60});
    create(authority.path(), &s).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(authority.path(), "tok", repo.path()).unwrap_or_else(|e| panic!("{e}"));
    let err = store
        .revise(&json!({"budgets": {"max_tokens": 5}}), "lead", "why")
        .err()
        .unwrap_or_default();
    assert!(err.contains("max_tokens"), "{err}");
}

#[test]
fn the_unbroken_spec_freezes_and_verifies() {
    // the control for every refusal in this file: the spec they patch is valid
    let (repo, authority) = repo_with(FIXED);
    create(authority.path(), &spec(repo.path(), "ok")).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(authority.path(), "ok", repo.path()).unwrap_or_else(|e| panic!("{e}"));
    let d = verify(&store, &Candidate::Ref("HEAD".into())).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(d["decision"], "accepted", "{d}");
}
