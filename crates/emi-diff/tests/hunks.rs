//! `hunks::changes` on a scratch repository: commits, renames, deletions,
//! and a working tree with untracked files.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_diff::hunks::{Status, changes};

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

fn write(repo: &Repo, name: &str, text: &str) {
    let path = repo.0.join(name);
    std::fs::create_dir_all(path.parent().unwrap_or(&repo.0)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn commit(repo: &Repo, files: &[(&str, &str)]) -> String {
    for (name, text) in files {
        write(repo, name, text);
    }
    git(&repo.0, &["add", "-A"]);
    git(&repo.0, &["commit", "-q", "--allow-empty", "-m", "c"]);
    git(&repo.0, &["rev-parse", "HEAD"])
}

fn repo(name: &str) -> Repo {
    let dir = std::env::temp_dir().join(format!("emi-hunks-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_default();
    git(&dir, &["init", "-q", "-b", "main"]);
    Repo(dir)
}

const TEST: &str = "def test_a():\n    assert f() == 1\n\n\ndef test_b():\n    assert f() == 2\n";
const LIB: &str =
    "def f():\n    return 1\n\n\ndef g():\n    return 2\n\n\ndef h():\n    return 3\n";

#[test]
fn commit_to_commit_with_rename_and_deletion() {
    let r = repo("commits");
    let base = commit(
        &r,
        &[
            ("src/lib.py", LIB),
            ("tests/test_lib.py", TEST),
            ("old.txt", "x\n"),
        ],
    );
    git(&r.0, &["mv", "src/lib.py", "src/core.py"]);
    git(&r.0, &["rm", "-q", "old.txt"]);
    let head = commit(
        &r,
        &[("tests/test_lib.py", "def test_a():\n    assert f() == 1\n")],
    );
    let mut got = changes(&r.0, &base, Some(&head)).unwrap_or_default();
    got.sort_by(|a, b| a.path.cmp(&b.path));
    let by = |p: &str| got.iter().find(|f| f.path == p).cloned();
    let renamed = by("src/core.py").unwrap_or_else(|| panic!("rename"));
    assert_eq!(renamed.status, Status::Renamed);
    assert_eq!(renamed.old_path.as_deref(), Some("src/lib.py"));
    assert!(renamed.added.is_empty() && renamed.removed.is_empty());
    assert_eq!(by("old.txt").map(|f| f.status), Some(Status::Deleted));
    let test = by("tests/test_lib.py").unwrap_or_else(|| panic!("modified test"));
    assert_eq!(test.status, Status::Modified);
    assert_eq!(test.removed.len(), 4, "{test:?}");
    assert!(test.added.is_empty());
}

#[test]
fn worktree_includes_edits_and_untracked_files() {
    let r = repo("worktree");
    let base = commit(&r, &[("src/lib.py", LIB)]);
    write(
        &r,
        "src/lib.py",
        &LIB.replace("return 2", "return 2  # noqa"),
    );
    write(&r, "src/new.py", "import os\n\nx = 1\n");
    write(&r, ".gitignore", "*.log\n");
    write(&r, "debug.log", "ignored\n");
    let got = changes(&r.0, &base, None).unwrap_or_default();
    let paths: Vec<&str> = got.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"src/lib.py")
            && paths.contains(&"src/new.py")
            && paths.contains(&".gitignore")
    );
    assert!(
        !paths.contains(&"debug.log"),
        "ignored files are not part of the change"
    );
    let lib = got
        .iter()
        .find(|f| f.path == "src/lib.py")
        .unwrap_or_else(|| panic!("lib"));
    assert_eq!(lib.added, vec![(6, "    return 2  # noqa".to_owned())]);
    let new = got
        .iter()
        .find(|f| f.path == "src/new.py")
        .unwrap_or_else(|| panic!("new"));
    assert_eq!((new.status, new.added.len()), (Status::Added, 3));
}
