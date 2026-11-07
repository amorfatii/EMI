//! The Rust driver end to end: a Cargo project checked by the `emi`
//! binary, real `cargo test` in the sandbox. Only the regression is
//! measured; every other stage says so in the receipt.
#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn write(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(file, text).expect("write");
}

const LIB: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n";

/// A crate with a unit test and an integration test, configured for EMI,
/// on a `task` branch off `main`.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("repo");
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    write(p, ".gitignore", "target/\n");
    write(
        p,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(p, "src/lib.rs", LIB);
    write(
        p,
        "tests/adds.rs",
        "#[test]\nfn adds_zero() {\n    assert_eq!(demo::add(0, 0), 0);\n}\n",
    );
    let config = json!({
        "schema": "emi-config-1.0", "language": "rust", "source": "src",
        "default_branch": "main",
        "tests": {"command": ["cargo", "test"], "globs": ["tests/*"]},
    });
    write(p, ".emi/config.json", &config.to_string());
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "baseline"]);
    git(p, &["switch", "-q", "-c", "task"]);
    dir
}

fn done(repo: &Path, state: &Path) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["done", "--format", "json"])
        .current_dir(repo)
        .env("XDG_STATE_HOME", state)
        .output()
        .expect("emi");
    let receipt =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("receipt: {e}: {out:?}"));
    (out.status.code().expect("exit code"), receipt)
}

#[test]
fn a_rust_change_is_checked_test_by_test_and_says_what_it_did_not_measure() {
    let repo = project();
    let state = tempfile::tempdir().expect("state");
    write(
        repo.path(),
        "src/lib.rs",
        &LIB.replace("    a + b\n", "    // the sum\n    a + b\n"),
    );
    let (code, receipt) = done(repo.path(), state.path());
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "held", "{receipt}");
    assert_eq!(receipt["inputs"]["driver"]["language"], "rust");
    let this_run = receipt["this_run"].to_string();
    assert!(
        this_run.contains("the rust driver does not measure"),
        "{receipt}"
    );
    assert!(receipt["signals"].is_null(), "{receipt}");

    // the integration test now fails: a break, named by its file
    write(
        repo.path(),
        "src/lib.rs",
        &LIB.replace("    a + b\n", "    a + b + 1\n"),
    );
    let (code, receipt) = done(repo.path(), state.path());
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "broken", "{receipt}");
    let failed = receipt["regression"]["failed"].to_string();
    assert!(failed.contains("tests/adds.rs::adds_zero"), "{receipt}");
    assert!(failed.contains("src/lib.rs::tests::adds"), "{receipt}");
}

#[test]
fn a_language_no_driver_knows_is_a_configuration_error() {
    let repo = project();
    let state = tempfile::tempdir().expect("state");
    let path = repo.path().join(".emi/config.json");
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("config")).expect("JSON");
    config["language"] = json!("cobol");
    std::fs::write(&path, config.to_string()).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["done", "--format", "json"])
        .current_dir(repo.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("emi");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no driver for language \"cobol\""), "{err}");
}

#[test]
fn a_rust_check_from_nothing_cached_reaches_the_same_decision() {
    let repo = project();
    write(
        repo.path(),
        "src/lib.rs",
        &LIB.replace("    a + b\n", "    a + b + 1\n"),
    );
    let (first, second) = (
        tempfile::tempdir().expect("state"),
        tempfile::tempdir().expect("state"),
    );
    let (code, one) = done(repo.path(), first.path());
    assert_eq!(code, 3, "{one}");
    let (_, two) = done(repo.path(), second.path());
    assert!(one["decision_id"].is_string(), "{one}");
    assert_eq!(one["input_id"], two["input_id"], "{one}\n{two}");
    assert_eq!(one["decision_id"], two["decision_id"], "{one}\n{two}");
}
