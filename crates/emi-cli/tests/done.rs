//! `emi init` and `emi done` through the binary: flags, output and exit
//! statuses. The fixture is the done check's own (a project whose pytest is
//! a stand-in), shared rather than copied.

#[path = "../../emi-done/tests/support/mod.rs"]
mod support;

use std::process::Command;

use serde_json::Value;
use support::{BROKEN_APP, Fixture, git};

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn emi(fx: &Fixture, args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(args)
        .arg("--repo")
        .arg(fx.path())
        .env("XDG_STATE_HOME", fx.state.path())
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn without_config(fx: &Fixture) {
    git(fx.path(), &["rm", "-q", "-f", ".emi/config.json"]);
    fx.commit_all("before emi init");
}

#[test]
fn init_proposes_without_a_terminal_and_writes_only_with_yes() {
    let fx = Fixture::new();
    without_config(&fx);
    let config = fx.path().join(".emi/config.json");

    let asked = emi(&fx, &["init"]);
    assert_eq!(asked.code, 2, "{}", asked.stderr);
    assert!(
        asked.stdout.contains("emi-config-0.1"),
        "the proposal is printed: {}",
        asked.stdout
    );
    assert!(
        asked.stdout.contains(".venv/bin/python -m pytest -q"),
        "{}",
        asked.stdout
    );
    assert!(!config.exists(), "nothing is written without confirmation");

    let written = emi(&fx, &["init", "--yes"]);
    assert_eq!(written.code, 0, "{}", written.stderr);
    let text = std::fs::read_to_string(&config).unwrap_or_default();
    assert!(emi_done::Config::parse(&text).is_ok(), "{text}");

    let again = emi(&fx, &["init", "--yes"]);
    assert_eq!(again.code, 2);
    assert!(again.stderr.contains("already exists"), "{}", again.stderr);
    assert_eq!(emi(&fx, &["init", "--yes", "--force"]).code, 0);
}

#[test]
fn init_refuses_a_language_it_does_not_support() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("index.js"), "1\n").unwrap_or_default();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "js"]);
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["init", "--yes", "--repo"])
        .arg(dir.path())
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported for now"));
    assert!(!dir.path().join(".emi").exists());
}

#[test]
fn done_exits_with_the_action_and_prints_canonical_json() {
    let fx = Fixture::new();
    let held = emi(&fx, &["done", "--format", "json"]);
    assert_eq!(held.code, 0, "{}{}", held.stdout, held.stderr);
    assert!(
        held.stdout.starts_with(r#"{"action":"stop","assurance":"#),
        "{}",
        held.stdout
    );
    let receipt: Value = serde_json::from_str(&held.stdout).unwrap_or_default();
    assert_eq!(receipt["regression"]["status"], "held");
    assert_eq!(receipt["change"]["status"], "unknown");

    fx.write("app.py", BROKEN_APP);
    let broken = emi(&fx, &["done", "--format", "json"]);
    assert_eq!(broken.code, 3, "{}", broken.stdout);

    fx.write("app.py", support::APP);
    fx.write(
        "tests/test_app.py",
        &format!(
            "{}\n\ndef test_more():\n    assert add(2, 2) == 4\n",
            support::TEST_APP
        ),
    );
    let ask = emi(&fx, &["done", "--format", "text"]);
    assert_eq!(ask.code, 4, "{}", ask.stdout);
    assert!(
        ask.stdout
            .starts_with("Regression: held (2 tests ran).\nNew behaviour: not measured.\n"),
        "{}",
        ask.stdout
    );
    assert!(
        ask.stdout
            .contains("emi done --approve old-test:tests/test_app.py"),
        "{}",
        ask.stdout
    );
    let approved = emi(
        &fx,
        &[
            "done",
            "--format",
            "json",
            "--approve",
            "old-test:tests/test_app.py",
        ],
    );
    assert_eq!(approved.code, 0, "{}", approved.stdout);
}

#[test]
fn done_reports_usage_and_configuration_errors_with_status_2() {
    let fx = Fixture::new();
    without_config(&fx);
    let missing = emi(&fx, &["done", "--format", "json"]);
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("emi init"), "{}", missing.stderr);
    assert_eq!(emi(&fx, &["done", "--setting", "sometimes"]).code, 2);
    assert_eq!(
        emi(&fx, &["done", "--baseline-source", "session"]).code,
        2,
        "a source needs a baseline"
    );
}
