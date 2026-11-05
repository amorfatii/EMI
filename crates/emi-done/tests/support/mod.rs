//! Fixture repositories for the done check: a Python project with a
//! virtualenv whose pytest is a stand-in (`fake_pytest.py`), because the
//! machine the tests run on has none and no package may be installed.
//!
//! These tests need `git`, `python3` and bubblewrap, as the controller's
//! own lifecycle tests do. Without bubblewrap the controller cannot run
//! sandboxed and every check is `incomplete`, which these tests report as
//! failures rather than passing around it.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use emi_done::candidate::Spec;
use emi_done::{ChangeEvidence, Given, Outcome, Request, Setting};
use serde_json::Value;
use tempfile::TempDir;

const FAKE_PYTEST: &str = include_str!("fake_pytest.py");

pub const CONFIG: &str = r#"{
  "schema": "emi-config-0.1",
  "language": "python",
  "source": ".",
  "default_branch": "main",
  "tests": {"command": [".venv/bin/python", "-m", "pytest", "-q"], "globs": ["tests/*"]},
  "link": [".venv"]
}
"#;

pub const OTHER: &str = "def neg(x):\n    return -x\n";
pub const BROKEN_OTHER: &str = "def neg(x):\n    return x\n";
pub const TEST_OTHER: &str =
    "from other import neg\n\n\ndef test_neg():\n    assert neg(1) == -1\n";
pub const PYPROJECT: &str = "[project]\nname = \"demo\"\ndependencies = []\n";
pub const BROKEN_APP: &str = "def add(a, b):\n    return a - b\n";
pub const APP: &str = "def add(a, b):\n    return a + b\n";
pub const TEST_APP: &str = "from app import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_zero():\n    assert add(0, 0) == 0\n";

pub fn git(dir: &Path, args: &[&str]) {
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
}

pub struct Fixture {
    pub repo: TempDir,
    /// Where `emi-done` keeps its state, outside the repository.
    pub state: TempDir,
}

fn make_venv(repo: &Path) {
    let status = Command::new("python3")
        .args(["-m", "venv", "--without-pip"])
        .arg(repo.join(".venv"))
        .status()
        .unwrap_or_else(|e| panic!("python3: {e}"));
    assert!(status.success(), "python3 -m venv failed");
    let lib = repo.join(".venv/lib");
    let version = std::fs::read_dir(&lib)
        .unwrap_or_else(|e| panic!("{e}"))
        .flatten()
        .next()
        .unwrap_or_else(|| panic!("the virtualenv has no lib/python*"))
        .path();
    let package = version.join("site-packages/pytest");
    std::fs::create_dir_all(&package).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(package.join("__init__.py"), "").unwrap_or_else(|e| panic!("{e}"));
    // started as `-m pytest` and imported as `pytest`, like the real one
    for file in ["__main__.py", "__init__.py"] {
        std::fs::write(package.join(file), FAKE_PYTEST).unwrap_or_else(|e| panic!("{e}"));
    }
}

impl Fixture {
    /// `main` holds the project and its configuration; `feature` adds one
    /// commit, so the merge base with `main` is not HEAD.
    pub fn new() -> Fixture {
        let repo = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let state = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let fx = Fixture { repo, state };
        git(fx.path(), &["init", "-q", "-b", "main"]);
        fx.write(".gitignore", ".venv/\n__pycache__/\n");
        fx.write("app.py", APP);
        fx.write("tests/test_app.py", TEST_APP);
        fx.write("other.py", OTHER);
        fx.write("tests/test_other.py", TEST_OTHER);
        fx.write("pyproject.toml", PYPROJECT);
        fx.write(".emi/config.json", CONFIG);
        git(fx.path(), &["add", "-A"]);
        git(fx.path(), &["commit", "-q", "-m", "base"]);
        make_venv(fx.path());
        git(fx.path(), &["checkout", "-q", "-b", "feature"]);
        fx.write("util.py", "def twice(x):\n    return 2 * x\n");
        git(fx.path(), &["add", "-A"]);
        git(fx.path(), &["commit", "-q", "-m", "feature work"]);
        fx
    }

    pub fn path(&self) -> &Path {
        self.repo.path()
    }

    pub fn write(&self, rel: &str, content: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{e}"));
        }
        std::fs::write(path, content).unwrap_or_else(|e| panic!("{e}"));
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.path().join(rel)).unwrap_or_else(|e| panic!("{e}"));
    }

    pub fn commit_all(&self, message: &str) {
        git(self.path(), &["add", "-A"]);
        git(self.path(), &["commit", "-q", "-m", message]);
    }

    pub fn request<'a>(&'a self, approve: &'a [String]) -> Request<'a> {
        Request {
            repo: self.path(),
            state_root: self.state.path(),
            baseline: None,
            candidate: Spec::Worktree,
            setting: Setting::Cooperative,
            approve,
            preserve: None,
        }
    }

    /// The working tree, cooperative, baseline from the merge base.
    pub fn check(&self) -> Outcome {
        self.run(&self.request(&[]))
    }

    pub fn run(&self, request: &Request) -> Outcome {
        emi_done::done(request, |_| {
            ChangeEvidence::NotMeasured("not measured in this test".into())
        })
        .unwrap_or_else(|e| panic!("emi done failed: {e}"))
    }

    pub fn try_run(&self, request: &Request) -> Result<Outcome, emi_done::Error> {
        emi_done::done(request, |_| {
            ChangeEvidence::NotMeasured("not measured in this test".into())
        })
    }

    /// Check with an explicit baseline of the given source.
    pub fn check_against<'a>(&'a self, baseline: &'a str, how: Given) -> Request<'a> {
        Request {
            baseline: Some((baseline, how)),
            ..self.request(&[])
        }
    }
}

pub fn strs(v: &Value) -> Vec<&str> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}
