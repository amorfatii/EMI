//! `emi trend` through the binary on scratch repositories: the size split
//! against the tool's own per-commit measurements, `--step`, and commits
//! that cannot be measured.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_diff::Request;
use emi_scan::Probe;
use serde_json::Value;

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str], date: Option<&str>) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
    ]);
    if let Some(d) = date {
        cmd.env("GIT_COMMITTER_DATE", d).env("GIT_AUTHOR_DATE", d);
    }
    let out = cmd
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn repo(name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("emi-trend-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"], None);
    Repo(dir)
}

fn commit_at(r: &Repo, files: &[(&str, &str)], date: Option<&str>) -> String {
    for (name, text) in files {
        let path = r.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(&r.0)).unwrap_or_default();
        std::fs::write(&path, text).unwrap_or_default();
    }
    git(&r.0, &["add", "-A"], date);
    git(&r.0, &["commit", "-q", "--allow-empty", "-m", "c"], date);
    git(&r.0, &["rev-parse", "HEAD"], None)
}

fn commit(r: &Repo, files: &[(&str, &str)]) -> String {
    commit_at(r, files, None)
}

/// Rows and summary of `emi trend` in JSONL.
fn trend(r: &Repo, args: &[&str]) -> (Vec<Value>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_emi"))
        .arg("trend")
        .arg("--repo")
        .arg(&r.0)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("emi: {e}"));
    assert!(
        out.status.success(),
        "emi trend {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    let summary = lines
        .pop()
        .map(|s| s["summary"].clone())
        .unwrap_or_default();
    (lines, summary)
}

/// A function of `branches` sequential ifs (cyclomatic complexity above
/// the erosion threshold, so it carries burden); `nested` puts a second
/// branch on each body line, keeping the line count.
fn branchy(name: &str, op: &str, branches: u32, nested: bool) -> String {
    let mut text = format!("def {name}(p, q):\n");
    for k in 1..=branches {
        let body = format!("q = q * {} + {k}", k + 1);
        let body = if nested {
            format!("if q: {body}")
        } else {
            body
        };
        text += &format!("    if p {op} {}:\n        {body}\n", k * 7);
    }
    text + "    return q\n"
}

/// SLOC and burden of the scope at `sha`, from the tool's own measurement.
fn measured(r: &Repo, sha: &str) -> (f64, f64) {
    let req = Request::new(&r.0, "src", sha, sha, Probe::Python);
    let (_, m) = emi_diff::change::measure_ref(&req, sha).unwrap_or_else(|e| panic!("{e}"));
    let m = m.unwrap_or_else(|| panic!("empty scope at {sha}"));
    let mut sloc = 0.0;
    let mut burden = 0.0;
    for c in m["components"].as_array().into_iter().flatten() {
        sloc += c["sloc"].as_f64().unwrap_or(f64::NAN);
        for dim in ["E", "V", "C"] {
            // the fixture is fully resolved: every dimension is a number
            burden += c["burden"][dim]
                .as_f64()
                .unwrap_or_else(|| panic!("{dim} not exact: {c}"));
        }
    }
    (sloc, burden)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

/// The split the formula gives on the tool's own measurements of two
/// commits: `(more code, denser code)`.
fn expected_split(first: (f64, f64), last: (f64, f64)) -> (f64, f64) {
    let (d0, d1) = (first.1 / first.0, last.1 / last.0);
    ((last.0 - first.0) * d0, last.0 * (d1 - d0))
}

fn check_rows(rows: &[Value], r: &Repo, shas: &[&str]) {
    assert_eq!(rows.len(), shas.len());
    for (row, sha) in rows.iter().zip(shas) {
        let (sloc, burden) = measured(r, sha);
        assert_eq!(row["sha"], *sha);
        assert_eq!(row["state"], "measured");
        assert!(close(f(&row["sloc"]), sloc), "{row}");
        assert!(close(f(&row["burden"]), burden), "{row}");
    }
}

/// The summary's split of `first -> last` is the formula applied to the
/// tool's own measurements; returns the decomposition.
fn check_split(s: &Value, first: (f64, f64), last: (f64, f64)) -> Value {
    let (size, density) = expected_split(first, last);
    let d = s["decomposition"].clone();
    assert!(close(f(&d["more_code"]["burden"]), size), "{d}");
    assert!(close(f(&d["denser_code"]["burden"]), density), "{d}");
    assert!(close(f(&d["burden_change"]), last.1 - first.1), "{d}");
    d
}

#[test]
fn growth_is_split_into_more_code_and_denser_code() {
    let r = repo("split");
    let c0 = commit(&r, &[("README", "x\n")]);
    let c1 = commit(&r, &[("src/a.py", &branchy("alpha", "==", 14, false))]);
    // twice the code, same shape of function: same density
    let c2 = commit(&r, &[("src/b.py", &branchy("beta", ">", 14, false))]);
    // same lines, each body now branches again
    let c3 = commit(
        &r,
        &[
            ("src/a.py", &branchy("alpha", "==", 14, true)),
            ("src/b.py", &branchy("beta", ">", 14, true)),
        ],
    );

    let (rows, s) = trend(&r, &["--since", &c0, "--until", &c2]);
    check_rows(&rows, &r, &[&c1, &c2]);
    let (m1, m2) = (measured(&r, &c1), measured(&r, &c2));
    assert!(
        m1.1 > 0.0 && m2.0 > m1.0,
        "fixture carries burden and grows"
    );
    let d = check_split(&s, m1, m2);
    assert!(f(&d["more_code"]["burden"]) > 0.0);
    assert!(close(f(&d["more_code"]["share"]), 1.0), "{d}");
    assert!(close(f(&d["denser_code"]["share"]), 0.0), "{d}");

    let (rows, s) = trend(&r, &["--since", &c2, "--until", &c3]);
    check_rows(&rows, &r, &[&c3]);
    assert!(s["decomposition"].is_null(), "one commit has no split");

    let (rows, s) = trend(&r, &["--since", &c1, "--until", &c3]);
    check_rows(&rows, &r, &[&c2, &c3]);
    let m3 = measured(&r, &c3);
    assert!(close(m3.0, m2.0) && m3.1 > m2.1, "same size, more burden");
    let d = check_split(&s, m2, m3);
    assert!(f(&d["denser_code"]["burden"]) > 0.0);
    assert!(close(f(&d["denser_code"]["share"]), 1.0), "{d}");
    assert!(close(f(&d["more_code"]["share"]), 0.0), "{d}");
    assert_eq!(s["change"]["sloc"], 0);
}

#[test]
fn step_keeps_the_first_and_last_commit() {
    let r = repo("step");
    let base = commit(&r, &[("src/a.py", "def f():\n    return 0\n")]);
    let shas: Vec<String> = (1..=5)
        .map(|i| {
            let text = format!("def f():\n    return {i}\n");
            commit(&r, &[("src/a.py", &text)])
        })
        .collect();
    let picked = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .map(|row| row["sha"].as_str().unwrap_or("").to_owned())
            .collect()
    };
    let (rows, s) = trend(&r, &["--since", &base, "--step", "3"]);
    assert_eq!(
        picked(&rows),
        [shas[0].clone(), shas[3].clone(), shas[4].clone()]
    );
    assert_eq!(s["commits"], 3);
    let (rows, _) = trend(&r, &["--since", &base, "--step", "2"]);
    assert_eq!(
        picked(&rows),
        [shas[0].clone(), shas[2].clone(), shas[4].clone()]
    );
    let (rows, _) = trend(&r, &["--since", &base, "--step", "9"]);
    assert_eq!(picked(&rows), [shas[0].clone(), shas[4].clone()]);
}

#[test]
fn a_date_starts_the_walk_after_it() {
    let r = repo("date");
    commit_at(
        &r,
        &[("src/a.py", "x = 0\n")],
        Some("2020-01-01T12:00:00+00:00"),
    );
    let second = commit_at(
        &r,
        &[("src/a.py", "x = 1\n")],
        Some("2021-01-01T12:00:00+00:00"),
    );
    let third = commit_at(
        &r,
        &[("src/a.py", "x = 2  # noqa\n")],
        Some("2022-01-01T12:00:00+00:00"),
    );
    let (rows, _) = trend(&r, &["--since", "2020-06-01"]);
    let shas: Vec<&str> = rows.iter().filter_map(|row| row["sha"].as_str()).collect();
    assert_eq!(shas, [second.as_str(), third.as_str()]);
    assert_eq!(rows[0]["date"], "2021-01-01T12:00:00+00:00");
    assert_eq!(rows[0]["guard_warnings"], serde_json::json!({}));
    assert_eq!(
        rows[1]["guard_warnings"],
        serde_json::json!({"suppress": 1}),
        "the step that added a suppression"
    );
}

#[test]
fn a_commit_that_cannot_be_measured_is_an_unverified_row() {
    let r = repo("unverified");
    let base = commit(&r, &[("src/a.py", "def f():\n    return 0\n")]);
    let ok = commit(&r, &[("src/a.py", "def f():\n    return 1\n")]);
    let broken = commit(&r, &[("src/a.py", "def f(:\n    return\n")]);
    let fixed = commit(&r, &[("src/a.py", "def f():\n    return 2\n")]);
    let (rows, s) = trend(&r, &["--since", &base]);
    assert_eq!(rows.len(), 3, "no gap: {rows:?}");
    assert_eq!(rows[1]["sha"], broken.as_str());
    assert_eq!(rows[1]["state"], "unverified");
    assert!(
        rows[1]["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{}",
        rows[1]
    );
    assert!(rows[1].get("sloc").is_none(), "no totals for it");
    assert_eq!(rows[0]["sha"], ok.as_str());
    assert_eq!(rows[2]["sha"], fixed.as_str());
    assert_eq!(
        (s["measured"].clone(), s["unverified"].clone()),
        (2.into(), 1.into())
    );
    assert_eq!(s["first"]["sha"], ok.as_str());
    assert_eq!(s["last"]["sha"], fixed.as_str());

    // tolerated, the parse failure is listed instead
    let (rows, _) = trend(&r, &["--since", &base, "--tolerate-syntax-errors"]);
    assert_eq!(rows[1]["state"], "measured");
    let unmeasured = rows[1]["unmeasured"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        unmeasured
            .iter()
            .any(|u| u["kind"] == "file" && u["file"] == "a.py"),
        "{unmeasured:?}"
    );
}
