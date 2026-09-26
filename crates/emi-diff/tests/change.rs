//! `build_change` on a scratch repository: pinned refs, empty sides,
//! unchanged files, and the snapshot safety rules.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_diff::{Request, build_change, snapshot};
use emi_scan::Probe;
use serde_json::Value;

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
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
    let dir = std::env::temp_dir().join(format!("emi-diff-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"]);
    Repo(dir)
}

fn commit(repo: &Repo, files: &[(&str, &str)], message: &str) -> String {
    for (name, text) in files {
        let path = repo.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(&repo.0)).unwrap_or_default();
        std::fs::write(&path, text).unwrap_or_default();
    }
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "--allow-empty", "-m", message]);
    git(&repo.0, &["rev-parse", "HEAD"])
}

const STABLE: &str = "def stable(x):\n    return x + 1\n";
const GROWING: &str = "def grow(a, b):\n    if a:\n        return b\n    return a\n";

fn change(repo: &Repo, before: &str, after: &str) -> Result<Value, String> {
    build_change(&Request::new(&repo.0, "src", before, after, Probe::Python))
}

#[test]
fn pins_refs_and_marks_unchanged_files() {
    let r = repo("pins");
    let first = commit(&r, &[("src/a.py", STABLE), ("src/b.py", GROWING)], "one");
    git(&r.0, &["tag", "-a", "v1", "-m", "v1"]);
    let grown = GROWING.replace("return a\n", "for i in b:\n        a += i\n    return a\n");
    let second = commit(&r, &[("src/b.py", &grown)], "two");
    let c = change(&r, "v1", "main").unwrap_or_default();
    assert_eq!(
        c["before_sha"],
        first.as_str(),
        "annotated tag peels to its commit"
    );
    assert_eq!(c["after_sha"], second.as_str());
    assert_eq!(
        c["scope"]["unchanged_components"],
        serde_json::json!(["a.py"])
    );
    assert_eq!(c["scope"]["complete"], true);
    let report = emi_ledger::review(&c).unwrap_or_default();
    assert!(
        report["growth"]["net_sloc"]
            .as_f64()
            .is_some_and(|n| n > 0.0)
    );
}

#[test]
fn first_commit_side_is_an_empty_scope() {
    let r = repo("empty");
    let base = commit(&r, &[("README.md", "hi\n")], "docs only");
    let head = commit(&r, &[("src/a.py", STABLE)], "code");
    let c = change(&r, &base, &head).unwrap_or_default();
    assert_eq!(c["before"], serde_json::json!([]));
    assert_eq!(c["after"][0]["id"], "a.py");
    assert_eq!(
        c["measurement_options_before"],
        c["measurement_options_after"]
    );
    let both_empty = change(&r, &base, &base);
    assert_eq!(
        both_empty.err().as_deref(),
        Some("empty review: no measurable source files on either side")
    );
}

#[test]
fn refuses_unsafe_refs_and_sources() {
    let r = repo("refs");
    let head = commit(&r, &[("src/a.py", STABLE)], "one");
    let refused = |before: &str| change(&r, before, &head).err().unwrap_or_default();
    assert!(refused("--output=x").starts_with("ref looks like an option"));
    assert!(refused("  ").starts_with("ref must be a nonempty string"));
    assert!(
        refused("no-such-branch").starts_with("git rev-parse --verify --end-of-options failed")
    );
    let tree = git(&r.0, &["rev-parse", "HEAD^{tree}"]);
    assert!(refused(&tree).contains("failed"), "a tree is not a commit");
    let escaped = build_change(&Request::new(&r.0, "../x", &head, &head, Probe::Python));
    assert!(escaped.err().unwrap_or_default().contains("audit F06"));
}

#[test]
fn gitlinks_fail_closed() {
    let r = repo("gitlink");
    let head = commit(&r, &[("src/a.py", STABLE)], "one");
    git(
        &r.0,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},src/vendored"),
        ],
    );
    git(
        &r.0,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "submodule",
        ],
    );
    let with_link = git(&r.0, &["rev-parse", "HEAD"]);
    let err = change(&r, &head, &with_link).err().unwrap_or_default();
    assert!(
        err.starts_with("gitlink/submodule entries unsupported"),
        "{err}"
    );
}

#[test]
fn merge_base_diffs_against_the_fork_point() {
    let r = repo("mergebase");
    let fork = commit(&r, &[("src/a.py", STABLE)], "fork");
    git(&r.0, &["checkout", "-q", "-b", "feature"]);
    let feature = commit(&r, &[("src/b.py", GROWING)], "feature");
    git(&r.0, &["checkout", "-q", "main"]);
    commit(
        &r,
        &[("src/c.py", STABLE.replace("stable", "other").as_str())],
        "main moves",
    );
    assert_eq!(
        snapshot::merge_base(&r.0, &git(&r.0, &["rev-parse", "main"]), &feature).ok(),
        Some(fork.clone())
    );
    let mut req = Request::new(&r.0, "src", "main", "feature", Probe::Python);
    req.merge_base = true;
    let c = build_change(&req).unwrap_or_default();
    assert_eq!(c["before_sha"], fork.as_str());
    assert_eq!(
        c["scope"]["expected_components"],
        serde_json::json!(["a.py", "b.py"])
    );
}
