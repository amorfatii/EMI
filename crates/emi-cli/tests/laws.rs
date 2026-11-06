//! Laws end to end: tests the person owns in `.emi/laws`, held to as the
//! baseline has them, with counterexamples in the repair lines and
//! proposals checked against trivial stand-ins.
#![allow(clippy::expect_used)]

#[path = "support/project.rs"]
mod project;

use project::{Project, git};
use serde_json::Value;

const LAWS: &str = ".emi/laws/test_add_laws.py";

/// A project whose baseline holds `laws`: committed on the default branch
/// before the task branch is cut.
fn with_laws(laws: &[(&str, &str)]) -> Project {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    for (path, text) in laws {
        p.write(path, text);
    }
    p.commit("the person's laws");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    p
}

fn law<'a>(receipt: &'a Value, id: &str) -> &'a Value {
    receipt["laws"]["laws"]
        .as_array()
        .expect("laws")
        .iter()
        .find(|l| l["id"] == id)
        .unwrap_or_else(|| panic!("no law {id} in {receipt}"))
}

const ADD_LAWS: &str = "from app import add\n\n\ndef test_add_commutes():\n    assert add(2, 3) == add(3, 2)\n\n\ndef test_none_is_nothing_to_add():\n    assert add(None, 4) == 4\n";

#[test]
fn an_open_law_sends_the_proposer_back_until_the_code_satisfies_it() {
    let p = with_laws(&[(LAWS, ADD_LAWS)]);
    // nothing changed yet: the open law already fails at the baseline
    p.write("notes.txt", "started\n");
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    let open = law(&receipt, &format!("{LAWS}::test_none_is_nothing_to_add"));
    assert_eq!(open["status"], "unmet", "{receipt}");
    assert!(
        open["counterexample"]
            .as_str()
            .is_some_and(|c| c.contains("TypeError")),
        "{open}"
    );
    assert!(
        receipt["repair"][0]
            .as_str()
            .is_some_and(|r| r.contains("does not hold yet")),
        "{receipt}"
    );
    assert_eq!(
        law(&receipt, &format!("{LAWS}::test_add_commutes"))["status"],
        "holds"
    );
    assert!(
        receipt["inputs"]["laws"].is_string(),
        "the laws are an input"
    );

    p.write(
        "app.py",
        "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(
        law(&receipt, &format!("{LAWS}::test_none_is_nothing_to_add"))["status"],
        "now_holds"
    );
    // the law executes what the change wrote: the function is law-checked
    let add = &receipt["change"]["functions"][0];
    assert_eq!(add["function"], "add", "{receipt}");
    assert!(
        add["laws"]
            .to_string()
            .contains(&format!("{LAWS}::test_none_is_nothing_to_add")),
        "{receipt}"
    );
}

#[test]
fn a_law_the_change_breaks_is_repaired_and_an_edited_law_does_not_count() {
    let p = with_laws(&[(LAWS, ADD_LAWS)]);
    p.write(
        "app.py",
        "def add(a, b):\n    return a - b if a > b else a + b\n",
    );
    // weakening the law does not help: it runs as the baseline has it
    p.write(
        LAWS,
        "from app import add\n\n\ndef test_add_commutes():\n    assert True\n\n\ndef test_none_is_nothing_to_add():\n    assert True\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(
        law(&receipt, &format!("{LAWS}::test_add_commutes"))["status"],
        "broken",
        "{receipt}"
    );
    assert!(
        receipt["repair"][0]
            .as_str()
            .is_some_and(|r| r.contains("held at the baseline")),
        "{receipt}"
    );
    let item = &receipt["changed_old_tests"][0];
    assert_eq!(item["item"], format!("law:{LAWS}"), "{receipt}");
    assert_eq!(item["approved"], false);
}

#[test]
fn a_proposed_law_a_trivial_body_passes_goes_back_to_the_proposer_before_a_person() {
    let p = Project::new();
    p.write(
        "app.py",
        "def add(a, b):\n    return a + b\n\n\ndef normalize(text):\n    return text.strip()\n",
    );
    // idempotence alone: the identity function satisfies it too
    p.write(
        ".emi/laws/test_normalize.py",
        "from app import normalize\n\n\ndef test_normalize_is_idempotent():\n    once = normalize(' a ')\n    assert normalize(once) == once\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    assert!(
        receipt["repair"][0].as_str().is_some_and(|r| r.starts_with(
            "The law file you propose, .emi/laws/test_normalize.py, still holds when"
        )),
        "{receipt}"
    );
    let item = &receipt["changed_old_tests"][0];
    assert_eq!(item["item"], "law:.emi/laws/test_normalize.py", "{receipt}");
    let proposal = &receipt["laws"]["proposals"][0];
    assert_eq!(proposal["laws"][0]["status"], "holds", "{receipt}");
    let vacuous = proposal["vacuous_for"].to_string();
    assert!(
        vacuous.contains("normalize returns its first argument (`text`) unchanged"),
        "{receipt}"
    );
    assert!(
        receipt["not_verified"]
            .to_string()
            .contains("does not pin that behaviour down")
    );
    // approved, it counts as a law
    let (code, approved) = p.done(&["--approve", "law:.emi/laws/test_normalize.py"]);
    assert_eq!(code, 0, "{approved}");
    assert_eq!(
        law(
            &approved,
            ".emi/laws/test_normalize.py::test_normalize_is_idempotent"
        )["status"],
        "now_holds",
        "{approved}"
    );
}

#[test]
fn a_property_law_reports_its_falsifying_example() {
    let p = with_laws(&[(
        ".emi/laws/test_absolute.py",
        "from hypothesis import given, strategies as st\n\nfrom app import absolute\n\n\n@given(st.integers())\ndef test_never_negative(x):\n    assert absolute(x) >= 0\n",
    )]);
    p.write(
        "app.py",
        "def add(a, b):\n    return a + b\n\n\ndef absolute(x):\n    return x\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 3, "{receipt}");
    let l = law(&receipt, ".emi/laws/test_absolute.py::test_never_negative");
    assert_eq!(l["status"], "unmet", "{receipt}");
    let example = l["counterexample"].as_str().expect("counterexample");
    assert!(
        example.contains("test_never_negative(") && example.contains("x=-1"),
        "{example}"
    );
    // derandomized and with no database: the same example every time
    let (_, again) = p.done(&[]);
    assert_eq!(
        law(&again, ".emi/laws/test_absolute.py::test_never_negative")["counterexample"],
        l["counterexample"]
    );
}

#[test]
fn without_laws_the_receipt_says_what_is_missing() {
    let p = Project::new();
    p.write("notes.txt", "x\n");
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["laws"]["laws"], serde_json::json!([]));
    assert!(
        receipt["not_verified"].to_string().contains("No laws"),
        "{receipt}"
    );
}

#[test]
fn a_locked_laws_directory_refuses_the_agents_edits_as_they_are_made() {
    let p = Project::new();
    let path = p.path().join(".emi/config.json");
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("config")).expect("JSON");
    config["laws"] = serde_json::json!({"lock": true});
    p.write(".emi/config.json", &config.to_string());
    let edit = |file: &str| {
        let input = serde_json::json!({
            "session_id": "lock", "cwd": p.path(), "tool_name": "Write",
            "tool_input": {"file_path": p.path().join(file), "content": "x"},
        });
        let out = p.emi(&["hook", "claude-code", "pre-tool-use"], Some(&input));
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let refused: Value = serde_json::from_str(&edit(".emi/laws/test_new.py")).expect("JSON");
    assert_eq!(
        refused["hookSpecificOutput"]["permissionDecision"], "deny",
        "{refused}"
    );
    assert!(
        edit("app.py").is_empty(),
        "product code is the agent's to edit"
    );
}
