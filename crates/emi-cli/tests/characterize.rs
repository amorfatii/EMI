//! Characterization of `emi done`: one small project per situation the
//! receipt distinguishes, and the summary of what the receipt said. The
//! table is the contract that refactoring the decision path must keep;
//! a row changes only on purpose, in the same commit as the behaviour.
#![allow(clippy::expect_used)]

#[path = "support/project.rs"]
mod project;

use project::Project;
use serde_json::Value;

const FIX: &str = "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n";
const NONE_TEST: &str = "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n";
const AGAIN_TEST: &str = "from app import add\n\ndef test_again():\n    assert add(1, 2) == 3\n";

/// `action regression change [lists that are not empty, with their size]`.
fn summary(code: i32, r: &Value) -> String {
    let mut parts = vec![
        format!("exit={code}"),
        r["action"].as_str().unwrap_or("?").to_owned(),
        format!(
            "regression={}",
            r["regression"]["status"].as_str().unwrap_or("?")
        ),
        format!("change={}", r["change"]["status"].as_str().unwrap_or("?")),
    ];
    for list in [
        "proving_tests",
        "redundant_tests",
        "failing_tests",
        "companion_tests",
        "skipped_tests",
    ] {
        let n = r["change"][list].as_array().map_or(0, Vec::len);
        if n > 0 {
            parts.push(format!("{list}={n}"));
        }
    }
    let old = r["changed_old_tests"].as_array().map_or(0, Vec::len);
    if old > 0 {
        parts.push(format!("changed_old_tests={old}"));
    }
    parts.join(" ")
}

type Setup = fn(&Project);

fn situations() -> Vec<(&'static str, Setup, &'static str)> {
    vec![
        (
            "unchanged",
            |_| {},
            "exit=0 stop regression=held change=none",
        ),
        (
            "broken product code",
            |p| p.write("app.py", project::BROKEN),
            "exit=3 repair regression=broken change=unexercised",
        ),
        (
            "fix with a proving test",
            |p| {
                p.write("app.py", FIX);
                p.write("tests/test_new.py", NONE_TEST);
            },
            "exit=0 stop regression=held change=exercised proving_tests=1",
        ),
        (
            "fix with a redundant test",
            |p| {
                p.write("app.py", FIX);
                p.write("tests/test_new.py", AGAIN_TEST);
            },
            "exit=0 stop regression=held change=unexercised redundant_tests=1",
        ),
        (
            "fix without a test",
            |p| p.write("app.py", FIX),
            "exit=0 stop regression=held change=unexercised",
        ),
        (
            "failing added test",
            |p| p.write("tests/test_new.py", NONE_TEST),
            "exit=3 repair regression=held change=none failing_tests=1",
        ),
        (
            "edited old test",
            |p| {
                p.write(
                    "tests/test_app.py",
                    "from app import add\n\ndef test_add():\n    assert add(1, 2) == 3\n",
                );
            },
            "exit=4 ask_person regression=held change=none changed_old_tests=1",
        ),
        (
            "added runner configuration",
            |p| p.write("conftest.py", "import os\n"),
            "exit=4 ask_person regression=held change=none changed_old_tests=1",
        ),
        (
            "uncollectable added test file",
            |p| {
                p.write(
                    "tests/test_bad.py",
                    "from app import nothing_here\n\ndef test_x():\n    pass\n",
                )
            },
            "exit=3 repair regression=broken change=none failing_tests=1",
        ),
    ]
}

#[test]
fn the_receipt_for_each_situation() {
    let mut got = Vec::new();
    let mut want = Vec::new();
    for (name, setup, expected) in situations() {
        let p = Project::new();
        setup(&p);
        let (code, receipt) = p.done(&[]);
        got.push(format!("{name}: {}", summary(code, &receipt)));
        want.push(format!("{name}: {expected}"));
    }
    assert_eq!(got.join("\n"), want.join("\n"));
}
