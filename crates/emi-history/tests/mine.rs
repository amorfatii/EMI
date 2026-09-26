//! History mining on a scratch repository with fixed commit dates.

use std::path::{Path, PathBuf};
use std::process::Command;

use emi_history::{DEFAULT_AI_PATTERN, Origin, Query, change_coupling, file_histories, mine};

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, env_date: Option<&str>, author: (&str, &str), args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args);
    cmd.env("GIT_AUTHOR_NAME", author.0)
        .env("GIT_AUTHOR_EMAIL", author.1);
    cmd.env("GIT_COMMITTER_NAME", author.0)
        .env("GIT_COMMITTER_EMAIL", author.1);
    if let Some(d) = env_date {
        cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("git: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(r: &Repo, day: u32, author: (&str, &str), files: &[(&str, &str)], message: &str) {
    for (name, text) in files {
        let path = r.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(&r.0)).unwrap_or_default();
        std::fs::write(path, text).unwrap_or_default();
    }
    let date = format!("2026-01-{day:02}T12:00:00Z");
    git(&r.0, None, author, &["add", "-A"]);
    git(&r.0, Some(&date), author, &["commit", "-q", "-m", message]);
}

const ALICE: (&str, &str) = ("Alice", "alice@example.com");
const BOB: (&str, &str) = ("Bob", "bob@example.com");
const BOT: (&str, &str) = ("renovate[bot]", "bot@example.com");

/// Five commits on 2026-01-01..05: a rename, an AI co-authored change,
/// a bot bump, and a file outside the scope.
fn scratch() -> Repo {
    let r = Repo(std::env::temp_dir().join(format!("emi-history-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&r.0);
    std::fs::create_dir_all(&r.0).unwrap_or_default();
    git(&r.0, None, ALICE, &["init", "-q", "-b", "main"]);
    commit(
        &r,
        1,
        ALICE,
        &[("src/old.py", "a\nb\nc\n"), ("src/b.py", "x\n")],
        "start",
    );
    commit(
        &r,
        2,
        BOB,
        &[("src/old.py", "a\nb\nc\nd\n"), ("src/b.py", "x\ny\n")],
        "both",
    );
    git(&r.0, None, ALICE, &["mv", "src/old.py", "src/new.py"]);
    commit(&r, 3, ALICE, &[], "rename");
    commit(
        &r,
        4,
        ALICE,
        &[("src/new.py", "a\nb\nc\nd\ne\n"), ("src/b.py", "x\ny\nz\n")],
        "agent change\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
    );
    commit(
        &r,
        5,
        BOT,
        &[("src/b.py", "x\ny\nz\nw\n"), ("README", "r\n")],
        "bump",
    );
    r
}

#[test]
fn follows_renames_and_attributes_origins() {
    let r = scratch();
    let query = |days| Query {
        repo: r.0.clone(),
        source: "src".into(),
        head: "HEAD".into(),
        days,
        ai_pattern: DEFAULT_AI_PATTERN.into(),
    };
    let commits = mine(&query(None)).unwrap_or_default();
    assert_eq!(commits.len(), 5);
    let origins: Vec<Origin> = commits.iter().map(|c| c.origin).collect();
    assert_eq!(
        origins,
        [
            Origin::Bot,
            Origin::Ai,
            Origin::Human,
            Origin::Human,
            Origin::Human
        ]
    );
    let h = file_histories(&commits);
    assert!(
        !h.contains_key("old.py"),
        "history is keyed by the current name"
    );
    let new = &h["new.py"];
    assert_eq!(
        (new.revisions, new.added),
        (4, 5),
        "the rename keeps old.py's history"
    );
    assert_eq!((new.authors, new.ai_revisions), (2, 1));
    assert!((new.main_author_share - 0.75).abs() < 1e-9);
    assert!(
        !h.contains_key("README") && !h.contains_key("../README"),
        "outside the scope"
    );
    let coupling = change_coupling(&commits, 2, 30.0, 30);
    assert_eq!(coupling.len(), 1);
    assert_eq!(
        (
            coupling[0].a.as_str(),
            coupling[0].b.as_str(),
            coupling[0].shared
        ),
        ("b.py", "new.py", 3)
    );
    let recent = mine(&query(Some(2))).unwrap_or_default();
    assert_eq!(recent.len(), 3, "a 2-day window before HEAD's commit time");
}
