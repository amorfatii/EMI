//! `emi proving` through the binary. The measurement itself is tested
//! against real pytest in crates/emi-proving; this covers the command: its
//! arguments, its output and its exit codes, with a language that needs no
//! test runner.
// a test that cannot set itself up must fail loudly, not pass
#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "git {args:?}");
}

/// A repository whose second commit adds a Go function.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("README.md"), "x\n").expect("write");
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    std::fs::write(
        dir.path().join("add.go"),
        "package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n",
    )
    .expect("write");
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "add"]);
    dir
}

fn emi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_emi"))
        .arg("proving")
        .args(args)
        .output()
        .expect("run emi")
}

#[test]
fn another_language_is_an_explicit_unknown_with_exit_zero() {
    let dir = repo();
    let repo = dir.path().to_string_lossy().into_owned();
    let out = emi(&[
        "--repo",
        &repo,
        "--baseline",
        "HEAD~1",
        "--candidate",
        "HEAD",
        "--language",
        "go",
        "--",
        "go",
        "test",
        "./...",
    ]);
    assert!(
        out.status.success(),
        "a report exits 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
    assert_eq!(v["status"], "unknown", "{v}");
    assert_eq!(v["functions"][0]["function"], "add", "{v}");
    assert_eq!(v["functions"][0]["status"], "unknown", "{v}");
    let reason = v["not_measured"][0].as_str().unwrap_or_default();
    assert!(reason.contains("`go` is not supported"), "{v}");
}

#[test]
fn a_ref_that_does_not_resolve_is_an_error() {
    let dir = repo();
    let repo = dir.path().to_string_lossy().into_owned();
    let out = emi(&["--repo", &repo, "--baseline", "no-such-ref", "--", "pytest"]);
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "no result for a request that cannot run"
    );
}

#[test]
fn the_baseline_and_a_test_command_are_required() {
    for args in [&["--baseline", "HEAD"][..], &["--", "pytest"][..]] {
        let out = emi(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}
