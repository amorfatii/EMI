//! The integrated binary, real pytest and a scripted proposer, with no
//! stand-in for the done check. Adapter API tests are separate from this.
#![allow(clippy::expect_used)]

#[path = "support/project.rs"]
mod project;

use project::Project;
use serde_json::{Value, json};

fn event(p: &Project, name: &str, session: &str) -> Value {
    let input = json!({"session_id": session, "cwd": p.path()});
    let out = p.emi(&["hook", "claude-code", name], Some(&input));
    assert!(out.status.success(), "{out:?}");
    if out.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&out.stdout).expect("hook JSON")
    }
}

/// The journal has the three checks the hook ran, and takes a label.
fn journal_has_the_three_checks_and_takes_a_label(p: &Project) {
    let report = p.emi(&["journal", "report", "--format", "json"], None);
    assert!(report.status.success(), "{report:?}");
    let s: Value = serde_json::from_slice(&report.stdout).expect("journal JSON");
    assert_eq!(s["checks"], 3, "{s}");
    assert_eq!(s["sent_back"], 2, "{s}");
    assert_eq!(
        s["rules"],
        json!([["redundant_test", 1], ["regression", 1]]),
        "{s}"
    );
    let id = s["unlabelled"][0]["id"].as_str().expect("an id").to_owned();
    let labelled = p.emi(&["journal", "label", &id[..6], "useful"], None);
    assert!(labelled.status.success(), "{labelled:?}");
    let text = p.emi(&["journal", "report"], None);
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    assert!(text.contains("Labels: useful 1."), "{text}");
}

#[test]
fn a_scripted_proposer_repairs_a_false_completion_then_removes_redundancy() {
    let p = Project::new();
    assert!(event(&p, "user-prompt-submit", "repair").is_null());
    p.write("app.py", project::BROKEN);
    let failed = event(&p, "stop", "repair");
    assert_eq!(failed["decision"], "block", "{failed}");
    assert!(
        failed["reason"]
            .as_str()
            .expect("reason")
            .contains("tests/test_app.py::test_add fails"),
        "{failed}"
    );

    p.write(
        "app.py",
        "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n",
    );
    p.write("tests/test_new.py", "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n\ndef test_again():\n    assert add(1, 2) == 3\n");
    let redundant = event(&p, "stop", "repair");
    assert_eq!(redundant["decision"], "block", "{redundant}");
    assert!(
        redundant["reason"]
            .as_str()
            .expect("reason")
            .contains("test_again"),
        "{redundant}"
    );

    p.write(
        "tests/test_new.py",
        "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "held");
    assert_eq!(receipt["change"]["status"], "proven");
    assert_eq!(receipt["candidate"]["id"], "worktree");
    assert_eq!(
        receipt["change"]["proving_tests"][0]["id"],
        "tests/test_new.py::test_none"
    );
    let stopped = event(&p, "stop", "repair");
    assert!(
        stopped["systemMessage"]
            .as_str()
            .expect("receipt")
            .contains("proven"),
        "{stopped}"
    );
    assert!(
        event(&p, "stop", "repair").is_null(),
        "unchanged content is skipped"
    );

    journal_has_the_three_checks_and_takes_a_label(&p);
}

#[test]
fn real_failures_exhaust_the_budget_and_a_user_message_resets_it() {
    let p = Project::new();
    event(&p, "user-prompt-submit", "budget");
    p.write("app.py", project::BROKEN);
    for round in 1..=3 {
        let answer = event(&p, "stop", "budget");
        assert_eq!(answer["decision"], "block", "{answer}");
        assert!(
            answer["reason"]
                .as_str()
                .expect("reason")
                .contains(&format!("round {round} of 3"))
        );
    }
    let last = event(&p, "stop", "budget");
    assert!(
        last["systemMessage"]
            .as_str()
            .expect("notice")
            .starts_with("NOT VERIFIED"),
        "{last}"
    );
    event(&p, "user-prompt-submit", "budget");
    p.write(
        "app.py",
        "# a new attempt\n\ndef add(a, b):\n    return a - b\n",
    );
    let again = event(&p, "stop", "budget");
    assert_eq!(again["decision"], "block", "{again}");
    assert!(
        again["reason"]
            .as_str()
            .expect("reason")
            .contains("round 1 of 3")
    );
}

#[test]
fn enforced_verification_reads_baseline_configuration_and_refuses_approval() {
    let p = Project::new();
    p.write("app.py", project::BROKEN);
    let config_path = p.path().join(".emi/config.json");
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(config_path).expect("config")).expect("JSON");
    config["tests"]["command"] = json!(["true"]);
    p.write(".emi/config.json", &config.to_string());
    let (code, receipt) = p.done(&["--setting", "enforced", "--baseline", "main"]);
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(receipt["assurance"]["setting"], "enforced");
    assert_eq!(receipt["regression"]["status"], "broken");
    let out = p.emi(
        &[
            "done",
            "--setting",
            "enforced",
            "--approve",
            "old-test:tests/test_app.py",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}

#[test]
fn skipped_regressions_and_unmeasurable_added_tests_are_incomplete() {
    let p = Project::new();
    p.write("tests/test_new.py", "import pytest\n\n@pytest.mark.skip(reason='not implemented')\ndef test_future():\n    pass\n");
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 5, "{receipt}");
    assert_eq!(
        receipt["change"]["unknown_tests"][0]["id"],
        "tests/test_new.py::test_future"
    );
}

#[test]
fn old_skips_and_old_failures_do_not_block_but_a_test_that_stops_running_does() {
    let p = Project::new();
    p.write("flags.py", "SLOW = False\n");
    p.write(
        "tests/test_env.py",
        "import pytest\nimport flags\n\n@pytest.mark.skip(reason='needs a database')\ndef test_db():\n    assert False\n\n@pytest.mark.xfail(reason='known')\ndef test_known():\n    assert False\n\ndef test_legacy():\n    assert False\n\ndef test_fast():\n    if flags.SLOW:\n        pytest.skip('slow machine')\n    assert True\n",
    );
    p.commit("a baseline with a skip, an expected failure and a failing test");

    p.write(
        "app.py",
        "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n",
    );
    p.write(
        "tests/test_new.py",
        "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n",
    );
    let (code, receipt) = p.done(&["--baseline", "HEAD"]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "held", "{receipt}");
    assert!(
        receipt["regression"]["reasons"][0]
            .as_str()
            .expect("note")
            .contains("already failed at the baseline"),
        "{receipt}"
    );
    assert_eq!(receipt["change"]["status"], "proven", "{receipt}");

    p.write("flags.py", "SLOW = True\n");
    let (code, receipt) = p.done(&["--baseline", "HEAD"]);
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "broken", "{receipt}");
    assert_eq!(
        receipt["regression"]["failed"],
        json!(["tests/test_env.py::test_fast"]),
        "{receipt}"
    );
}

#[test]
fn an_added_test_that_fails_sends_the_proposer_back() {
    let p = Project::new();
    p.write(
        "tests/test_new.py",
        "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "broken", "{receipt}");

    // appended to an old file, the failing test is not part of the
    // regression run (the file runs in its baseline form): still a repair
    let fresh = Project::new();
    let old = std::fs::read_to_string(fresh.path().join("tests/test_app.py")).expect("old test");
    fresh.write(
        "tests/test_app.py",
        &format!("{old}\n\ndef test_none():\n    assert add(None, 2) == 2\n"),
    );
    let (code, receipt) = fresh.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(receipt["regression"]["status"], "held", "{receipt}");
    assert_eq!(
        receipt["change"]["failing_tests"][0]["id"], "tests/test_app.py::test_none",
        "{receipt}"
    );
    assert!(
        receipt["repair"][0]
            .as_str()
            .expect("repair line")
            .contains("test_none"),
        "{receipt}"
    );
}
