//! Determinism end to end: every run is held to the pins, and a receipt
//! computed again from nothing cached is the same decision on the same
//! evidence.
#![allow(clippy::expect_used)]

#[path = "support/project.rs"]
mod project;

use project::Project;
use serde_json::Value;

/// A test that passes only under the pins: the hash seed, the time zone,
/// and `random` seeded from the test's own id.
const PINNED: &str = r#"import hashlib, os, random, time

def test_the_pins_hold():
    assert os.environ["PYTHONHASHSEED"] == "0"
    assert os.environ.get("TZ") == "UTC" and time.strftime("%Z") == "UTC"
    nodeid = os.environ["PYTEST_CURRENT_TEST"].rsplit(" ", 1)[0]
    seed = int.from_bytes(hashlib.sha256(nodeid.encode()).digest()[:8], "big")
    expected = random.Random(seed).random()
    assert random.random() == expected
"#;

#[test]
fn every_run_is_held_to_the_pins() {
    let p = Project::new();
    p.write("tests/test_pins.py", PINNED);
    p.commit("a test of the pins");
    p.write(
        "app.py",
        "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n",
    );
    p.write(
        "tests/test_new.py",
        &format!(
            "{PINNED}\nfrom app import add\n\ndef test_none():\n    test_the_pins_hold()\n    assert add(None, 2) == 2\n"
        ),
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    // the old test of the pins ran in both baseline runs and on the
    // candidate, and passed every time
    assert_eq!(receipt["regression"]["status"], "held", "{receipt}");
    assert_eq!(
        receipt["regression"]["reasons"],
        serde_json::json!([]),
        "{receipt}"
    );
    assert_eq!(receipt["regression"]["quarantined"], serde_json::json!([]));
    // the added test needs the pins too, in the proving runs
    assert_eq!(receipt["change"]["status"], "exercised", "{receipt}");
    assert_eq!(receipt["inputs"]["pins"]["hash_seed"], 0);
    assert!(receipt["inputs"]["environment"].is_string(), "{receipt}");
}

fn replay(p: &Project, receipt: &Value) -> (i32, Value) {
    let path = p.state.path().join("receipt.json");
    std::fs::write(&path, receipt.to_string()).expect("write receipt");
    let out = p.emi(&["replay", path.to_str().expect("path")], None);
    let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {out:?}"));
    (out.status.code().expect("exit code"), v)
}

#[test]
fn a_replay_reaches_the_same_decision_on_the_same_evidence() {
    let p = Project::new();
    p.write(
        "app.py",
        "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n",
    );
    p.write(
        "tests/test_new.py",
        "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    let (code, replayed) = replay(&p, &receipt);
    assert_eq!(code, 0, "{replayed}");
    assert_eq!(replayed["same"], true, "{replayed}");
    assert_eq!(replayed["decision_id"], receipt["decision_id"]);
    // a receipt someone edited is not what the evidence says
    let mut edited = receipt.clone();
    edited["decision_id"] = Value::String("0".repeat(64));
    edited["regression"]["tests_run"] = Value::from(99);
    let (code, differs) = replay(&p, &edited);
    assert_eq!(code, 1, "{differs}");
    assert_eq!(differs["same"], false);
    assert_eq!(differs["differences"][0]["part"], "regression", "{differs}");
}

#[test]
fn a_working_tree_that_changed_since_the_receipt_is_not_replayed() {
    let p = Project::new();
    p.write("app.py", project::BROKEN);
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    p.write("app.py", project::APP);
    let path = p.state.path().join("receipt.json");
    std::fs::write(&path, receipt.to_string()).expect("write receipt");
    let out = p.emi(&["replay", path.to_str().expect("path")], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("working tree changed"));
}
