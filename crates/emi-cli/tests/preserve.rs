//! Preserve intent end to end: the person asks for the behaviour to be
//! kept, and every modified function is compared with the baseline's on
//! the inputs the tests pass it and inputs drawn from its annotations.
#![allow(clippy::expect_used)]

#[path = "support/project.rs"]
mod project;

use project::{Project, git};
use serde_json::Value;

const SIGN: &str = "def add(a, b):\n    return a + b\n\n\ndef sign(n: int) -> str:\n    if n < 0:\n        return \"negative\"\n    if n == 0:\n        return \"zero\"\n    return \"positive\"\n";

/// The baseline has `sign` and a test that never asks about zero.
fn project() -> Project {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    p.write("app.py", SIGN);
    p.write(
        "tests/test_sign.py",
        "from app import sign\n\n\ndef test_sign():\n    assert sign(5) == \"positive\"\n    assert sign(-3) == \"negative\"\n",
    );
    p.commit("sign");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    p
}

fn done(p: &Project, args: &[&str]) -> (i32, Value) {
    p.done(args)
}

#[test]
fn a_refactoring_that_keeps_the_behaviour_stops_with_the_inputs_compared() {
    let p = project();
    p.write(
        "app.py",
        "def add(a, b):\n    return a + b\n\n\ndef sign(n: int) -> str:\n    return \"negative\" if n < 0 else (\"zero\" if n == 0 else \"positive\")\n",
    );
    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 0, "{receipt}");
    let kept = &receipt["preserve"];
    assert_eq!(kept["divergences"], serde_json::json!([]), "{receipt}");
    let sign = &kept["functions"][0];
    assert_eq!(sign["function"], "sign", "{receipt}");
    assert!(
        sign["compared"].as_u64().is_some_and(|n| n > 2),
        "{receipt}"
    );
    assert_eq!(sign["deterministic"], true);
    assert!(kept["counts"]["generated"].as_u64().is_some_and(|n| n > 0));
    assert_eq!(
        receipt["inputs"]["intent"]["preserve"],
        serde_json::json!([])
    );
    assert!(
        receipt["not_verified"]
            .to_string()
            .contains("not a proof of equivalence")
    );
}

#[test]
fn a_refactoring_that_changes_an_untested_input_is_sent_back_with_it() {
    let p = project();
    // zero now counts as negative; the tests never ask about zero
    p.write(
        "app.py",
        "def add(a, b):\n    return a + b\n\n\ndef sign(n: int) -> str:\n    if n <= 0:\n        return \"negative\"\n    return \"positive\"\n",
    );
    let (code, plain) = done(&p, &[]);
    assert_eq!(code, 0, "without the intent the tests pass: {plain}");
    assert!(plain["preserve"].is_null());

    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 3, "{receipt}");
    let d = &receipt["preserve"]["divergences"][0];
    assert_eq!(d["input"]["args"], serde_json::json!([0]), "{receipt}");
    assert_eq!(d["before"]["returned"], "zero");
    assert_eq!(d["now"]["returned"], "negative");
    assert!(
        receipt["repair"][0]
            .as_str()
            .is_some_and(|r| r.contains("app.py::sign answers differently")
                && r.contains("returned \"zero\" and now returned \"negative\"")),
        "{receipt}"
    );
}

#[test]
fn the_recorded_intent_applies_until_cleared() {
    let p = project();
    p.write(
        "app.py",
        "def add(a, b):\n    return a + b\n\n\ndef sign(n: int) -> str:\n    if n <= 0:\n        return \"negative\"\n    return \"positive\"\n",
    );
    let set = p.emi(&["intent", "preserve", "app.py"], None);
    assert!(set.status.success(), "{set:?}");
    assert!(String::from_utf8_lossy(&set.stdout).contains("functions under app.py"));
    let (code, receipt) = done(&p, &[]);
    assert_eq!(code, 3, "{receipt}");
    assert_eq!(
        receipt["inputs"]["intent"]["preserve"],
        serde_json::json!(["app.py"])
    );
    let cleared = p.emi(&["intent", "clear"], None);
    assert!(cleared.status.success(), "{cleared:?}");
    let (code, receipt) = done(&p, &[]);
    assert_eq!(code, 0, "{receipt}");
    assert!(receipt["preserve"].is_null());
}

#[test]
fn decorated_staticmethod_and_classmethod_are_compared_under_preserve() {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    p.write(
        "app.py",
        "class Calc:\n    @staticmethod\n    def double(n: int) -> int:\n        return n * 2\n",
    );
    p.write(
        "tests/test_app.py",
        "from app import Calc\n\ndef test_double():\n    assert Calc.double(3) == 6\n",
    );
    p.commit("staticmethod baseline");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    p.write(
        "app.py",
        "class Calc:\n    @staticmethod\n    def double(n: int) -> int:\n        return n + n if n != 0 else 1\n",
    );
    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 3, "{receipt}");
    let d = &receipt["preserve"]["divergences"][0];
    assert_eq!(d["function"], "Calc.double", "{receipt}");
    assert_eq!(d["input"]["args"], serde_json::json!([0]), "{receipt}");
}

#[test]
fn instance_methods_and_in_place_mutations_are_compared_under_preserve() {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    p.write(
        "app.py",
        "class Account:\n    def __init__(self, balance: int):\n        self.balance = balance\n\n    def deposit(self, amount: int) -> None:\n        if amount > 0:\n            self.balance += amount\n",
    );
    p.write(
        "tests/test_app.py",
        "from app import Account\n\ndef test_deposit():\n    acc = Account(10)\n    acc.deposit(5)\n    assert acc.balance == 15\n",
    );
    p.commit("instance method baseline");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    // Candidate mutates self on negative amounts too, which the test never checks
    p.write(
        "app.py",
        "class Account:\n    def __init__(self, balance: int):\n        self.balance = balance\n\n    def deposit(self, amount: int) -> None:\n        if amount != 0:\n            self.balance += amount\n",
    );
    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 3, "{receipt}");
    let d = &receipt["preserve"]["divergences"][0];
    assert_eq!(d["function"], "Account.deposit", "{receipt}");
    assert!(d["now"].get("self").is_some(), "{receipt}");
    assert!(
        receipt["repair"][0].as_str().is_some_and(|r| r
            .contains("app.py::Account.deposit answers differently")
            && r.contains("leaving self =")),
        "{receipt}"
    );
}
