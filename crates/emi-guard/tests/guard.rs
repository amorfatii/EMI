//! `emi_guard::guard` end to end on a scratch repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_guard::{Policy, Tier, guard};
use serde_json::json;

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

fn write(r: &Repo, name: &str, text: &str) {
    let path = r.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(&r.0)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn commit(r: &Repo) -> String {
    git(&r.0, &["add", "-A"]);
    git(&r.0, &["commit", "-q", "--allow-empty", "-m", "c"]);
    git(&r.0, &["rev-parse", "HEAD"])
}

fn repo(name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("emi-guard-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"]);
    let r = Repo(dir);
    write(&r, "src/app.py", "def f():\n    return 1\n");
    write(
        &r,
        "tests/test_app.py",
        "def test_a():\n    assert f() == 1\n    assert f() > 0\n\n\ndef test_b():\n    assert f()\n",
    );
    write(
        &r,
        "tests/test_old.py",
        "def test_old():\n    assert True\n",
    );
    write(
        &r,
        "pyproject.toml",
        "[project]\nname = \"x\"\n\n[tool.pytest.ini_options]\naddopts = \"-q\"\n",
    );
    write(&r, "poetry.lock", "# lock\n");
    r
}

fn ids(report: &emi_guard::Report) -> Vec<&'static str> {
    report.violations.iter().map(|v| v.id).collect()
}

#[test]
fn clean_change_passes() {
    let r = repo("clean");
    let base = commit(&r);
    write(&r, "src/app.py", "def f():\n    return 2\n");
    let head = commit(&r);
    let report =
        guard(&r.0, &base, Some(&head), &Policy::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.decision(), "pass", "{:?}", report.violations);
}

#[test]
fn every_core_guardrail_fires() {
    let r = repo("dirty");
    let base = commit(&r);
    write(&r, "src/app.py", "def f():\n    return 2  # noqa\n");
    write(
        &r,
        "tests/test_app.py",
        "import pytest\n\n\n@pytest.mark.skip(reason='later')\ndef test_a():\n    assert f() == 2\n\n\ndef test_b():\n    assert f()\n",
    );
    git(&r.0, &["rm", "-q", "tests/test_old.py"]);
    write(
        &r,
        "pyproject.toml",
        "[project]\nname = \"x\"\n\n[tool.pytest.ini_options]\naddopts = \"-q --deselect tests\"\n",
    );
    write(&r, "poetry.lock", "# lock v2\n");
    write(&r, ".github/workflows/ci.yml", "on: push\n");
    write(&r, "docs/notes.md", "hi\n");
    let head = commit(&r);
    let policy = Policy::from_json(&json!({"permitted_paths": ["src/*", "tests/*", "pyproject.toml", "poetry.lock", ".github/*"]}))
        .unwrap_or_else(|e| panic!("{e}"));
    let report = guard(&r.0, &base, Some(&head), &policy).unwrap_or_else(|e| panic!("{e}"));
    let got = ids(&report);
    for want in [
        "suppress.added",
        "tests.skip_added",
        "tests.fewer_assertions",
        "tests.deleted",
        "tests.config_changed",
        "prohibited.lockfile",
        "prohibited.ci",
        "scope.outside",
    ] {
        assert!(got.contains(&want), "missing {want} in {got:?}");
    }
    assert!(
        !got.contains(&"tests.fewer"),
        "test count unchanged: {got:?}"
    );
    assert_eq!(report.decision(), "block");
    let noqa = report
        .violations
        .iter()
        .find(|v| v.id == "suppress.added")
        .map(|v| (v.tier, v.line));
    assert_eq!(noqa, Some((Tier::Review, Some(2))));
    let outside = report
        .violations
        .iter()
        .find(|v| v.id == "scope.outside")
        .map(|v| v.path.clone());
    assert_eq!(outside.as_deref(), Some("docs/notes.md"));
}

#[test]
fn policy_overrides_tiers_and_allows_classes() {
    let r = repo("policy");
    let base = commit(&r);
    write(&r, "poetry.lock", "# lock v2\n");
    write(&r, "src/app.py", "def f():\n    return 1  # type: ignore\n");
    let head = commit(&r);
    let policy =
        Policy::from_json(&json!({"allow_classes": ["lockfile"], "tiers": {"suppress": "off"}}))
            .unwrap_or_else(|e| panic!("{e}"));
    let report = guard(&r.0, &base, Some(&head), &policy).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), vec!["suppress.added"]);
    assert_eq!(
        report.decision(),
        "pass",
        "an 'off' tier reports without affecting the decision"
    );
    assert!(Policy::from_json(&json!({"tiers": {"x": "maybe"}})).is_err());
}

#[test]
fn worktree_and_budgets() {
    let r = repo("worktree");
    let base = commit(&r);
    write(&r, "src/new.py", &"x = 1\n".repeat(30));
    let policy = Policy::from_json(&json!({"max_net_sloc": 20})).unwrap_or_else(|e| panic!("{e}"));
    let report = guard(&r.0, &base, None, &policy).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ids(&report), vec!["growth.net_sloc"]);
    assert_eq!(report.decision(), "review");
    assert_eq!(report.to_json()["head"], "worktree");
}
