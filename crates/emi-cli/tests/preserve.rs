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

#[test]
fn a_mixed_commit_with_a_proven_fix_and_a_refactoring_guards_only_the_unproven_refactoring() {
    let p = project();
    // Fix `add` (handling None) with a proving test AND refactor `sign` cleanly in the same commit
    p.write(
        "app.py",
        "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n\n\ndef sign(n: int) -> str:\n    return \"negative\" if n < 0 else (\"zero\" if n == 0 else \"positive\")\n",
    );
    p.write(
        "tests/test_add.py",
        "from app import add\n\n\ndef test_add_none():\n    assert add(None, 4) == 4\n",
    );
    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(
        receipt["preserve"]["divergences"],
        serde_json::json!([]),
        "{receipt}"
    );
    assert_eq!(
        receipt["preserve"]["functions"][0]["function"], "sign",
        "{receipt}"
    );
}

#[test]
fn argument_in_place_mutations_are_compared_under_preserve() {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    p.write(
        "app.py",
        "def append_positive(items: list[int], x: int) -> None:\n    if x > 0:\n        items.append(x)\n",
    );
    p.write(
        "tests/test_app.py",
        "from app import append_positive\n\ndef test_append():\n    lst = [1]\n    append_positive(lst, 2)\n    assert lst == [1, 2]\n",
    );
    p.commit("arg mutation baseline");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    // Candidate appends non-zero instead of positive (e.g. on negative numbers)
    p.write(
        "app.py",
        "def append_positive(items: list[int], x: int) -> None:\n    if x != 0:\n        items.append(x)\n",
    );
    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 3, "{receipt}");
    let d = &receipt["preserve"]["divergences"][0];
    assert_eq!(d["function"], "append_positive", "{receipt}");
    assert!(
        receipt["repair"][0].as_str().is_some_and(|r| r
            .contains("app.py::append_positive answers differently")
            && r.contains("leaving mutated =")),
        "{receipt}"
    );
}

#[test]
fn async_def_and_rich_types_are_compared_under_preserve() {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    p.write(
        "app.py",
        "import asyncio\nfrom typing import NamedTuple\n\nclass Point(NamedTuple):\n    x: int\n    y: int\n\nasync def process(pt: Point, tags: set[str], payload: bytes) -> int:\n    await asyncio.sleep(0)\n    if b'skip' in payload:\n        return 0\n    return pt.x + pt.y + len(tags)\n",
    );
    p.write(
        "tests/test_app.py",
        "import asyncio\nfrom app import Point, process\n\ndef test_process():\n    res = asyncio.run(process(Point(1, 2), {'a', 'b'}, b'hello'))\n    assert res == 5\n",
    );
    p.commit("async and rich types baseline");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    // Candidate refactors cleanly keeping behavior
    p.write(
        "app.py",
        "import asyncio\nfrom typing import NamedTuple\n\nclass Point(NamedTuple):\n    x: int\n    y: int\n\nasync def process(pt: Point, tags: set[str], payload: bytes) -> int:\n    if b'skip' in payload:\n        return 0\n    await asyncio.sleep(0)\n    return (pt.x + pt.y) + len(tags)\n",
    );
    let (code, receipt) = done(&p, &["--preserve"]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(
        receipt["preserve"]["divergences"],
        serde_json::json!([]),
        "{receipt}"
    );
}

#[test]
fn path_filtering_restricts_drift_to_specified_files() {
    let p = Project::new();
    git(p.path(), &["switch", "-q", "main"]);
    p.write(
        "app.py",
        "def f(n: int) -> str:\n    if n < 0:\n        return 'neg'\n    return 'pos'\n",
    );
    p.write("other.py", "def g(n: int) -> int:\n    return n * 2\n");
    p.write(
        "tests/test_all.py",
        "from app import f\nfrom other import g\n\ndef test_all():\n    assert f(5) == 'pos'\n    assert g(2) == 4\n",
    );
    p.commit("two files baseline");
    git(p.path(), &["switch", "-q", "-C", "task"]);
    // Modify both app.py and other.py
    // app.py has a breaking change on untested input 0
    p.write(
        "app.py",
        "def f(n: int) -> str:\n    if n <= 0:\n        return 'neg'\n    return 'pos'\n",
    );
    // other.py refactors cleanly keeping behavior
    p.write("other.py", "def g(n: int) -> int:\n    return n + n\n");
    // If we asked to preserve all, app.py would fail on 0
    let (code_all, receipt_all) = done(&p, &["--preserve"]);
    assert_eq!(code_all, 3, "{receipt_all}");

    // But asking to preserve only other.py restricts check to other.py: passes!
    let (code, receipt) = done(&p, &["--preserve", "other.py"]);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(
        receipt["preserve"]["divergences"],
        serde_json::json!([]),
        "{receipt}"
    );
    let funcs = receipt["preserve"]["functions"].as_array().expect("array");
    assert_eq!(funcs.len(), 1);
    assert_eq!(funcs[0]["function"], "g");
}
