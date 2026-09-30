//! `emi diff-mutate` through the binary: mutants only on the lines a change
//! added, run against copies of its head.

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
    let d = std::env::temp_dir().join(format!("emi-cli-diffmut-{name}-{}", std::process::id()));
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
        println!("python3 is not available: diff-mutate test skipped");
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

fn write(repo: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let p = repo.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap_or_default();
        }
        std::fs::write(p, text).unwrap_or_default();
    }
}

/// A repository whose last commit changes `base` into `head`.
fn change(name: &str, base: &[(&str, &str)], head: &[(&str, &str)]) -> Dir {
    let repo = scratch(name);
    write(&repo.0, base);
    git(&repo.0, &["init", "-q", "-b", "main"]);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "base"]);
    write(&repo.0, head);
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "-m", "change"]);
    repo
}

fn diff_mutate(repo: &Path, extra: &[&str], command: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .args(["diff-mutate", "--base", "HEAD~1", "--head", "HEAD"])
        .args(["--format", "json", "--repo"])
        .arg(repo)
        .args(extra)
        .arg("--")
        .args(command)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    assert!(
        out.status.success(),
        "a report exits 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

// every line of price has a mutable token
const PRICE: &str = "def price(total, member):\n    if total > 100 and member:\n        return total * 0.9\n    if total < 0:\n        return 0\n    return total + 1\n";

const PASS: &[&str] = &["python3", "-c", "pass"];

fn lines(v: &Value) -> Vec<u64> {
    v["survivors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["line"].as_u64())
        .collect()
}

#[test]
fn only_the_added_line_is_mutated() {
    if !python3() {
        return;
    }
    let repo = change(
        "added",
        &[("src/calc.py", PRICE)],
        &[("src/calc.py", &PRICE.replace("total + 1", "total + 2"))],
    );
    // tests that check nothing let every mutant survive, so the survivors
    // are exactly the mutants that ran
    let r = diff_mutate(&repo.0, &[], PASS);
    assert_eq!(r["state"], "measured", "{r}");
    assert_eq!(r["candidates"], 1, "{r}");
    assert_eq!(r["run"], 1);
    assert_eq!(lines(&r), vec![6], "{r}");
    assert_eq!(r["survivors"][0]["function"], "price");
    assert_eq!(r["survivors"][0]["original"], "+");
    assert_eq!(r["survivors"][0]["replacement"], "-");
}

#[test]
fn a_new_line_no_test_checks_survives() {
    if !python3() {
        return;
    }
    let test =
        "import sys\nsys.path.insert(0, 'src')\nimport calc\nassert calc.price(5, False) == 6\n";
    let repo = change(
        "survivor",
        &[("src/calc.py", PRICE), ("tests/run.py", test)],
        &[(
            "src/calc.py",
            &format!("{PRICE}\n\ndef fee(weight):\n    return weight * 2\n"),
        )],
    );
    let r = diff_mutate(&repo.0, &[], &["python3", "tests/run.py"]);
    assert_eq!(r["state"], "measured", "{r}");
    assert!(r["survived"].as_u64().unwrap_or(0) >= 1, "{r}");
    let s = &r["survivors"][0];
    assert_eq!(s["file"], "src/calc.py", "{r}");
    assert_eq!(s["function"], "fee");
    assert_eq!(s["line"], 10);
    assert!(r["note"].as_str().unwrap_or("").contains("equivalent"));
}

#[test]
fn a_change_a_test_pins_is_killed() {
    if !python3() {
        return;
    }
    let test =
        "import sys\nsys.path.insert(0, 'src')\nimport calc\nassert calc.price(5, False) == 7\n";
    let repo = change(
        "killed",
        &[("src/calc.py", PRICE)],
        &[
            ("src/calc.py", &PRICE.replace("total + 1", "total + 2")),
            ("tests/run.py", test),
        ],
    );
    let r = diff_mutate(&repo.0, &[], &["python3", "tests/run.py"]);
    assert_eq!(r["state"], "measured", "{r}");
    assert_eq!(r["killed"], 1, "{r}");
    assert_eq!(r["survived"], 0);
    assert_eq!(r["survivors"], Value::Array(Vec::new()));
}

#[test]
fn a_failing_baseline_is_unverified_not_zero_survivors() {
    if !python3() {
        return;
    }
    let repo = change(
        "baseline",
        &[("src/calc.py", PRICE)],
        &[("src/calc.py", &PRICE.replace("total + 1", "total + 2"))],
    );
    let r = diff_mutate(&repo.0, &[], &["python3", "-c", "raise SystemExit(1)"]);
    assert_eq!(r["state"], "unverified", "{r}");
    assert_eq!(r["baseline"]["passed"], false);
    assert_eq!(r["baseline"]["exit_code"], 1);
    for key in [
        "run",
        "killed",
        "survived",
        "timed_out",
        "errors",
        "survivors",
    ] {
        assert_eq!(r[key], Value::Null, "{key}: {r}");
    }
    assert_eq!(r["candidates"], 1);
}

#[test]
fn the_budget_is_respected_and_the_sample_repeats() {
    if !python3() {
        return;
    }
    let extra = "\n\ndef band(x):\n    if x < 1 or x > 9:\n        return x * 2\n    return x - 1\n\n\ndef flag(a, b):\n    return a == b and a != 0\n";
    let repo = change(
        "budget",
        &[("src/calc.py", PRICE)],
        &[("src/calc.py", &format!("{PRICE}{extra}"))],
    );
    let r = diff_mutate(&repo.0, &["--max-mutants", "3"], PASS);
    assert_eq!(r["state"], "measured", "{r}");
    assert!(r["candidates"].as_u64().unwrap_or(0) > 3, "{r}");
    assert_eq!(r["run"], 3, "{r}");
    let functions: std::collections::BTreeSet<&str> = r["survivors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["function"].as_str())
        .collect();
    assert_eq!(functions, ["band", "flag"].into(), "both represented: {r}");
    let again = diff_mutate(&repo.0, &["--max-mutants", "3"], PASS);
    assert_eq!(r["survivors"], again["survivors"]);
}

#[test]
fn a_change_to_a_non_code_file_has_nothing_to_mutate() {
    let repo = change(
        "nocode",
        &[("src/calc.py", PRICE), ("README.md", "one\n")],
        &[("README.md", "two\n")],
    );
    let r = diff_mutate(&repo.0, &[], &["false"]);
    assert_eq!(r["state"], "nothing_to_mutate", "{r}");
    assert_eq!(r["candidates"], 0);
    assert_eq!(r["not_mutated"]["unsupported"][0], "README.md", "{r}");
    // nothing ran, so the failing command never mattered
    assert_eq!(r["baseline"], Value::Null);
}
