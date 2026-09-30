//! `emi drift` through the binary: a small Python package, its unittest
//! suite on the base commit, and a head commit that changes it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("emi-cli-drift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_default();
    Dir(d)
}

fn python3() -> bool {
    let ok = Command::new("python3")
        .args(["-c", "pass"])
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        println!("python3 is not available: drift test skipped");
    }
    ok
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "git {args:?}");
}

fn write(repo: &Dir, path: &str, text: &str) {
    let p = repo.0.join(path);
    std::fs::create_dir_all(p.parent().unwrap_or(&repo.0)).unwrap_or_default();
    std::fs::write(p, text).unwrap_or_default();
}

const TEST_HEADER: &str = "import os\nimport sys\nimport unittest\n\nsys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'src'))\n\nfrom shop import prices\n\n\nclass T(unittest.TestCase):\n";

/// A repository whose base has `module` and `tests` and whose head has
/// `changed` in place of `module`.
fn repo(name: &str, module: &str, tests: &str, changed: &str) -> Dir {
    let repo = scratch(name);
    write(&repo, "src/shop/__init__.py", "");
    write(&repo, "src/shop/prices.py", module);
    write(
        &repo,
        "tests/test_prices.py",
        &format!("{TEST_HEADER}{tests}"),
    );
    git(&repo.0, &["init", "-q", "-b", "main"]);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "base"]);
    write(&repo, "src/shop/prices.py", changed);
    git(&repo.0, &["commit", "-q", "-am", "head"]);
    repo
}

fn drift(repo: &Dir, command: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["drift", "--base", "HEAD~1", "--format", "json", "--repo"])
        .arg(&repo.0)
        .arg("--")
        .args(command)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

const UNITTEST: &[&str] = &["python3", "-m", "unittest", "discover", "-s", "tests"];

fn function<'a>(r: &'a Value, name: &str) -> &'a Value {
    r["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["name"] == name)
        .unwrap_or(&Value::Null)
}

#[test]
fn tightened_validation_drifts_on_the_input_the_tests_pass() {
    if !python3() {
        return;
    }
    let old = "def clamp(x):\n    if x < 0:\n        return 0\n    return x\n";
    let new = "def clamp(x):\n    if x < 0:\n        raise ValueError(x)\n    return x\n";
    let tests = "    def test_clamp(self):\n        self.assertEqual(clamp_all(), 7)\n\n\ndef clamp_all():\n    return prices.clamp(-2) + prices.clamp(3) + prices.clamp(4)\n";
    let repo = repo("tighten", old, tests, new);
    let r = drift(&repo, UNITTEST);
    assert_eq!(r["state"], "measured", "{r}");
    assert_eq!(r["counts"]["with_drift"], 1, "{r}");
    let clamp = function(&r, "clamp");
    assert_eq!(clamp["inputs"], 3, "{clamp}");
    assert_eq!(clamp["pinned"], "unknown");
    let drifts = clamp["drifts"].as_array().cloned().unwrap_or_default();
    assert_eq!(drifts.len(), 1, "{clamp}");
    assert_eq!(drifts[0]["input"]["args"][0], -2);
    assert_eq!(drifts[0]["old"]["returned"], 0);
    assert_eq!(drifts[0]["new"]["raised"], "ValueError");
}

#[test]
fn a_pure_refactor_has_no_drift() {
    if !python3() {
        return;
    }
    let old = "def total(items, scale=1):\n    s = 0\n    for i in items:\n        s += i\n    return s * scale\n";
    let new = "def total(items, scale=1):\n    return sum(items) * scale\n";
    let tests = "    def test_total(self):\n        self.assertEqual(prices.total([1, 2, 3]), 6)\n        self.assertEqual(prices.total((4, 5), scale=2), 18)\n";
    let repo = repo("refactor", old, tests, new);
    let r = drift(&repo, UNITTEST);
    assert_eq!(r["state"], "measured", "{r}");
    assert_eq!(r["counts"]["compared"], 1, "{r}");
    assert_eq!(r["counts"]["with_drift"], 0, "{r}");
    assert_eq!(function(&r, "total")["compared"], 2, "{r}");
}

#[test]
fn arguments_without_a_stable_encoding_are_counted_not_recorded() {
    if !python3() {
        return;
    }
    let old = "class Box:\n    def __init__(self, v):\n        self.v = v\n\n\ndef peek(box):\n    return box.v\n";
    let new = "class Box:\n    def __init__(self, v):\n        self.v = v\n\n\ndef peek(box):\n    return getattr(box, 'v')\n";
    let tests = "    def test_peek(self):\n        self.assertEqual(prices.peek(prices.Box(3)), 3)\n        self.assertEqual(prices.peek(prices.Box({1, 2})), {1, 2})\n";
    let repo = repo("unrecordable", old, tests, new);
    let r = drift(&repo, UNITTEST);
    assert_eq!(r["counts"]["unrecordable"], 2, "{r}");
    assert_eq!(r["counts"]["no_inputs"], 1, "{r}");
    assert_eq!(function(&r, "peek")["inputs"], 0, "{r}");
    assert_eq!(r["state"], "nothing_to_compare", "{r}");
}

#[test]
fn changed_instance_methods_and_nested_functions_are_counted_as_skipped() {
    if !python3() {
        return;
    }
    let old = "class Rate:\n    def __init__(self, r):\n        self.r = r\n\n    def apply(self, x):\n        return x * self.r\n\n    @staticmethod\n    def half(x):\n        return x / 2\n\n\ndef outer(x):\n    def helper(y):\n        return y + 1\n    return helper(x)\n";
    let new = "class Rate:\n    def __init__(self, r):\n        self.r = r\n\n    def apply(self, x):\n        return self.r * x\n\n    @staticmethod\n    def half(x):\n        return x // 2\n\n\ndef outer(x):\n    def helper(y):\n        return 1 + y\n    return helper(x)\n";
    let tests = "    def test_rate(self):\n        self.assertEqual(prices.Rate(2).apply(3), 6)\n        self.assertEqual(prices.Rate.half(4), 2)\n";
    let repo = repo("method", old, tests, new);
    let r = drift(&repo, UNITTEST);
    assert_eq!(r["counts"]["skipped_method"], 1, "{r}");
    assert_eq!(r["skipped"][0]["name"], "Rate.apply", "{r}");
    assert_eq!(r["skipped"][0]["reason"], "instance_method", "{r}");
    assert_eq!(r["counts"]["skipped_nested"], 1, "{r}");
    assert_eq!(r["skipped"][1]["name"], "outer.helper", "{r}");
    // the static method is replayed: 4 / 2 is 2.0, 4 // 2 is 2
    let half = function(&r, "Rate.half");
    assert_eq!(half["drifts"][0]["old"]["returned"], 2.0, "{r}");
    assert_eq!(half["drifts"][0]["new"]["returned"], 2, "{r}");
}

#[test]
fn a_test_run_that_cannot_start_is_unverified() {
    if !python3() {
        return;
    }
    let old = "def clamp(x):\n    return max(x, 0)\n";
    let new = "def clamp(x):\n    return x if x > 0 else 0\n";
    let repo = repo("nostart", old, "    pass\n", new);
    let r = drift(&repo, &["emi-drift-no-such-runner"]);
    assert_eq!(r["state"], "unverified", "{r}");
    let why = r["reasons"].to_string();
    assert!(why.contains("could not start"), "{why}");
    assert_eq!(r["counts"]["compared"], 0);
    assert_eq!(r["functions"], serde_json::json!([]));
}
