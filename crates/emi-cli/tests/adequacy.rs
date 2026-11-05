//! Adequacy and signals end to end: mutants of the changed lines run
//! against the proving tests and the laws, and escape hatches the change
//! added to product code, reported for a person.
#![allow(clippy::expect_used)]

#[path = "support/project.rs"]
mod project;

use project::Project;
use serde_json::Value;

/// The `+` of the first branch is not what the proving test exercises.
const FIX: &str = "def add(a, b):\n    return a + b if a is not None else b\n";
const PROVING: &str = "from app import add\n\n\ndef test_none():\n    assert add(None, 2) == 2\n";

fn with_adequacy(p: &Project, on_survivors: &str) {
    let path = p.path().join(".emi/config.json");
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("config")).expect("JSON");
    config["adequacy"] = serde_json::json!({"max_mutants": 10, "on_survivors": on_survivors});
    p.write(".emi/config.json", &config.to_string());
}

#[test]
fn a_mutant_the_evidence_does_not_notice_is_reported_and_can_send_the_proposer_back() {
    let p = Project::new();
    p.write("app.py", FIX);
    p.write("tests/test_new.py", PROVING);
    with_adequacy(&p, "report");
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    let a = &receipt["adequacy"];
    assert_eq!(a["state"], "measured", "{receipt}");
    let survivor = &a["survivors"][0];
    assert_eq!(
        (
            survivor["original"].as_str(),
            survivor["replacement"].as_str()
        ),
        (Some("+"), Some("-")),
        "{receipt}"
    );
    assert!(
        a.get("baseline").is_none(),
        "durations stay out of the receipt"
    );

    with_adequacy(&p, "repair");
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    assert!(
        receipt["repair"][0]
            .as_str()
            .is_some_and(|r| r.contains("changing `+` to `-` breaks no proving test or law")),
        "{receipt}"
    );
}

#[test]
fn adequacy_is_off_unless_configured() {
    let p = Project::new();
    p.write("app.py", FIX);
    p.write("tests/test_new.py", PROVING);
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    assert!(receipt["adequacy"].is_null(), "{receipt}");
}

#[test]
fn escape_hatches_in_product_code_are_reported_not_acted_on() {
    let p = Project::new();
    p.write(
        "app.py",
        "import os\n\n\ndef add(a, b):\n    if os.environ.get(\"PYTEST_CURRENT_TEST\"):\n        pass\n    return a + b  # type: ignore\n",
    );
    // the same line in a test file is not a signal
    p.write(
        "tests/test_more.py",
        "import os  # noqa\n\n\ndef test_x():\n    assert True\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    let kinds: Vec<&str> = receipt["signals"]
        .as_array()
        .expect("signals")
        .iter()
        .filter_map(|s| s["kind"].as_str())
        .collect();
    assert_eq!(kinds, ["test_sniffing", "suppression"], "{receipt}");
    assert_eq!(receipt["signals"][0]["assurance"], "pattern");
    assert!(
        receipt["not_verified"]
            .to_string()
            .contains("escape-hatch signal(s)"),
        "{receipt}"
    );
}
