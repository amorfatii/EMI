//! A scope that cannot be read is an error, not an empty scope.

use std::path::Path;
use std::process::Command;

use serde_json::json;

use super::files_matching;

fn repo_with_a_test() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    std::fs::create_dir_all(dir.path().join("tests")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(dir.path().join("tests/test_a.py"), "def test_a(): pass\n")
        .unwrap_or_else(|e| panic!("{e}"));
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["commit", "-q", "-m", "c"],
    ] {
        git(dir.path(), args);
    }
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    assert!(out.status.success(), "git {args:?}");
}

#[test]
fn a_revision_that_cannot_be_listed_is_an_error_not_an_empty_scope() {
    let repo = repo_with_a_test();
    let globs = vec!["tests/*".to_owned()];
    let missing = "0".repeat(40);
    assert_eq!(
        files_matching(repo.path(), "HEAD", &globs).unwrap_or_default(),
        ["tests/test_a.py"]
    );
    let err = files_matching(repo.path(), &missing, &globs).err();
    assert!(
        err.is_some_and(|e| e.contains("cannot list the files of")),
        "an unreadable baseline must not read as no trusted files"
    );
}

#[test]
fn a_refactor_whose_baseline_cannot_be_listed_says_why() {
    let repo = repo_with_a_test();
    let contract = json!({"refactor": {"targets": ["app.py"], "min_cognitive_reduction": 0.2}});
    let missing = "0".repeat(40);
    let gate = crate::refactor::gate(&contract, repo.path(), &missing, repo.path(), &[]);
    assert_eq!(gate["state"], "unverified", "{gate}");
    assert!(
        gate["detail"]
            .as_str()
            .is_some_and(|d| d.contains("cannot list the files of")),
        "{gate}"
    );
}
