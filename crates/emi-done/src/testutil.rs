//! Fixture repositories for unit tests.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

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

pub fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repository on `main` with one commit.
pub fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    git(dir.path(), &["init", "-q", "-b", "main"]);
    commit_file(dir.path(), "README.md", "hello\n", "first");
    dir
}

pub fn commit_file(dir: &Path, name: &str, content: &str, message: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{e}"));
    }
    std::fs::write(&path, content).unwrap_or_else(|e| panic!("{e}"));
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}
