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
fn laws_measured_stand_when_what_they_execute_cannot_be() {
    // the second law holds when run, and fails under the tracer coverage
    // installs: which functions the laws execute cannot be measured
    let p = with_laws(&[(
        LAWS,
        "import sys\n\nfrom app import add\n\n\ndef test_add_commutes():\n    assert add(2, 3) == add(3, 2)\n\n\ndef test_untraced():\n    assert sys.gettrace() is None\n",
    )]);
    p.write("app.py", "def add(a, b):\n    return b + a\n");
    let (_, receipt) = p.done(&[]);
    assert!(receipt["laws"]["fault"].is_null(), "{receipt}");
    for name in ["test_add_commutes", "test_untraced"] {
        assert_eq!(
            law(&receipt, &format!("{LAWS}::{name}"))["status"],
            "holds",
            "{receipt}"
        );
    }
    assert_ne!(receipt["action"], "incomplete", "{receipt}");
    assert!(
        receipt["not_verified"]
            .to_string()
            .contains("which changed functions the laws execute"),
        "{receipt}"
    );
}

#[test]
fn measuring_the_same_broken_law_twice_gives_the_same_decision_id() {
    // called through the module, pytest's message shows the function's
    // repr, with its address in memory
    let p = with_laws(&[(
        LAWS,
        "import app\n\n\ndef test_adds():\n    assert app.add(2, 3) == 5\n",
    )]);
    p.write("app.py", project::BROKEN);
    let (code, first) = p.done(&[]);
    assert_eq!(code, 3, "{first}");
    let broken = law(&first, &format!("{LAWS}::test_adds"));
    assert!(
        broken["counterexample"]
            .as_str()
            .is_some_and(|c| c.contains("at 0x…")),
        "{broken}"
    );
    let (_, second) = p.done(&[]);
    assert_eq!(
        first["decision_id"], second["decision_id"],
        "{first}\n{second}"
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

const PROPERTY_LAWS: &str = r#"from hypothesis import assume, event, given, strategies as st

from app import add


@given(st.integers(0, 9), st.booleans())
def test_small(x, b):
    assert add(x, int(b)) == x + int(b)


@given(st.integers(), st.integers())
def test_commutes(a, b):
    event("negative" if a < 0 else "not negative")
    assert add(a, b) == add(b, a)


@given(st.integers())
def test_multiples_of_three(x):
    assume(x % 3 == 0)
    assert add(x, 0) == x
"#;

#[test]
fn a_property_law_says_which_inputs_it_held_on() {
    let p = with_laws(&[(LAWS, PROPERTY_LAWS)]);
    p.write(
        "app.py",
        "def add(a, b):\n    # the sum\n    return a + b\n",
    );
    let (code, receipt) = p.done(&[]);
    assert_eq!(code, 0, "{receipt}");

    // a space Hypothesis tried every input of: the law holds for all
    let small = &law(&receipt, &format!("{LAWS}::test_small"))["examples"];
    assert_eq!(small["every_input"], true, "{receipt}");
    assert_eq!(small["valid"], 20, "{receipt}");

    // a sample, and which cases its inputs reached
    let commutes = &law(&receipt, &format!("{LAWS}::test_commutes"))["examples"];
    assert_eq!(commutes["every_input"], false, "{receipt}");
    assert_eq!(commutes["valid"], 100, "{receipt}");
    let events = commutes["events"].as_object().expect("events");
    assert!(
        events.contains_key("negative") && events.contains_key("not negative"),
        "{receipt}"
    );

    // what was not shown is said
    let not_verified = receipt["not_verified"].to_string();
    assert!(
        not_verified.contains("hold on sampled inputs, not on every input"),
        "{not_verified}"
    );
    assert!(!not_verified.contains("test_small ("), "{not_verified}");
    assert!(
        not_verified.contains("test_multiples_of_three ran on"),
        "{not_verified}"
    );

    // from nothing cached, the same inputs and the same decision
    clear(p.state.path());
    let (_, again) = p.done(&[]);
    assert!(receipt["decision_id"].is_string() && receipt["input_id"].is_string());
    assert_eq!(again["input_id"], receipt["input_id"], "{receipt}\n{again}");
    assert_eq!(
        again["decision_id"], receipt["decision_id"],
        "{receipt}\n{again}"
    );
    assert_eq!(again["laws"], receipt["laws"]);
}

/// Empty the state directory: no cache, no journal.
fn clear(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).expect("state") {
        let path = entry.expect("entry").path();
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        gone.expect("clear state");
    }
}
