//! A configured project with real pytest, used by the integrated done loop.
#![allow(clippy::expect_used, dead_code)]

#[path = "../../../emi-proving/tests/support/venv.rs"]
mod python;

use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

pub const APP: &str = "def add(a, b):\n    return a + b\n";
pub const BROKEN: &str = "def add(a, b):\n    return a - b\n";

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
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
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

pub struct Project {
    pub repo: tempfile::TempDir,
    pub state: tempfile::TempDir,
}

impl Project {
    pub fn new() -> Self {
        let p = Self {
            repo: tempfile::tempdir().expect("repo"),
            state: tempfile::tempdir().expect("state"),
        };
        git(p.path(), &["init", "-q", "-b", "main"]);
        p.write(
            ".gitignore",
            ".venv\n__pycache__/\n.pytest_cache/\n.opencode/\n.claude/\n",
        );
        p.write("pytest.ini", "[pytest]\npythonpath = .\n");
        p.write("app.py", APP);
        p.write("tests/test_app.py", "from app import add\n\ndef test_add():\n    assert add(1, 2) == 3\n\ndef test_zero():\n    assert add(0, 0) == 0\n");
        std::os::unix::fs::symlink(python::venv(), p.path().join(".venv")).expect("link venv");
        p.commit("baseline");
        let out = p.emi(&["init", "--yes"], None);
        assert!(out.status.success(), "init: {out:?}");
        p.commit("EMI configuration");
        git(p.path(), &["switch", "-q", "-c", "task"]);
        p
    }

    pub fn path(&self) -> &Path {
        self.repo.path()
    }

    pub fn write(&self, path: &str, text: &str) {
        let file = self.path().join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(file, text).expect("write");
    }

    pub fn commit(&self, message: &str) {
        git(self.path(), &["add", "-A"]);
        git(self.path(), &["commit", "-q", "-m", message]);
    }

    pub fn emi(&self, args: &[&str], input: Option<&Value>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_emi"));
        c.args(args)
            .current_dir(self.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env_remove("EMI_DONE_COMMAND")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if input.is_some() {
            c.stdin(Stdio::piped());
        }
        let mut child = c.spawn().expect("emi");
        if let Some(input) = input {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(input.to_string().as_bytes())
                .expect("payload");
        }
        child.wait_with_output().expect("emi result")
    }

    pub fn done(&self, args: &[&str]) -> (i32, Value) {
        let mut command = vec!["done", "--format", "json"];
        command.extend(args);
        let out = self.emi(&command, None);
        let v =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("receipt: {e}: {out:?}"));
        (out.status.code().expect("exit code"), v)
    }
}
