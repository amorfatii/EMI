//! Line-level view of a change: which files a candidate adds, deletes,
//! renames or modifies relative to a baseline commit, and exactly which
//! lines it adds and removes. Guardrails judge an agent's change from this
//! (skip markers added, suppressions added, tests removed), so it is parsed
//! from `git diff -U0` with renames detected, and a working tree includes
//! its untracked-but-not-ignored files as additions.

use std::path::Path;

use crate::snapshot::{Result, git_bytes};

/// How a path changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Added => "add",
            Status::Modified => "modify",
            Status::Deleted => "delete",
            Status::Renamed => "rename",
        }
    }
}

/// One file's change: new path (old path for deletions), the rename
/// source, and the added/removed lines as `(1-based line, text)` on the
/// new/old side respectively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub status: Status,
    pub binary: bool,
    pub added: Vec<(u32, String)>,
    pub removed: Vec<(u32, String)>,
}

impl FileChange {
    fn new(path: String, status: Status) -> Self {
        FileChange {
            path,
            old_path: None,
            status,
            binary: false,
            added: Vec::new(),
            removed: Vec::new(),
        }
    }

    /// Net lines added (added − removed).
    pub fn net(&self) -> i64 {
        self.added.len() as i64 - self.removed.len() as i64
    }
}

/// The change from `base` (a commit) to `head` (a commit, or the working
/// tree when `None`).
pub fn changes(repo: &Path, base: &str, head: Option<&str>) -> Result<Vec<FileChange>> {
    let mut args = vec![
        "-c",
        "core.quotePath=false",
        "diff",
        "-U0",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        base,
    ];
    if let Some(h) = head {
        args.push(h);
    }
    args.push("--");
    let raw = git_bytes(repo, &args)?;
    let mut files = parse(&String::from_utf8_lossy(&raw));
    if head.is_none() {
        files.extend(untracked(repo)?);
    }
    Ok(files)
}

/// Text of `path` at commit `rev`, or in the working tree when `None`;
/// `None` when the path does not exist there or is not valid text.
pub fn file_at(repo: &Path, rev: Option<&str>, path: &str) -> Option<String> {
    text_at(repo, rev, path).filter(|t| !t.contains('\0'))
}

/// Like [`file_at`], but keeps files with NUL bytes: for a path the diff
/// already treats as text (a test of a tokenizer may contain `"\0"`).
pub fn text_at(repo: &Path, rev: Option<&str>, path: &str) -> Option<String> {
    let bytes = match rev {
        Some(r) => git_bytes(repo, &["show", &format!("{r}:{path}")]).ok()?,
        None => {
            let top =
                String::from_utf8_lossy(&git_bytes(repo, &["rev-parse", "--show-toplevel"]).ok()?)
                    .trim()
                    .to_owned();
            std::fs::read(Path::new(&top).join(path)).ok()?
        }
    };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Untracked, non-ignored files of the working tree, as additions of all
/// their lines (binary files flagged, without lines).
fn untracked(repo: &Path) -> Result<Vec<FileChange>> {
    let top = String::from_utf8_lossy(&git_bytes(repo, &["rev-parse", "--show-toplevel"])?)
        .trim()
        .to_owned();
    let listed = git_bytes(
        repo,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--full-name",
            "--",
            ":/",
        ],
    )?;
    let mut out = Vec::new();
    for raw in listed.split(|b| *b == 0).filter(|c| !c.is_empty()) {
        let path = String::from_utf8_lossy(raw).into_owned();
        let mut fc = FileChange::new(path.clone(), Status::Added);
        match std::fs::read(Path::new(&top).join(&path)) {
            Ok(bytes) if bytes.contains(&0) => fc.binary = true,
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                fc.added = text
                    .lines()
                    .enumerate()
                    .map(|(i, l)| (i as u32 + 1, l.trim_end_matches('\r').to_owned()))
                    .collect();
            }
            Err(_) => continue,
        }
        out.push(fc);
    }
    Ok(out)
}

/// Undo git's C-style quoting of a path (`"a\tb"`), if quoted.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_owned();
    };
    let mut bytes = Vec::new();
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'"') => bytes.push(b'"'),
            Some(b'\\') => bytes.push(b'\\'),
            Some(d @ b'0'..=b'7') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    if let Some(&n @ b'0'..=b'7') = it.peek() {
                        v = v * 8 + u32::from(n - b'0');
                        it.next();
                    }
                }
                bytes.push(v as u8);
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn side_path(raw: &str, prefix: &str) -> Option<String> {
    let path = unquote(raw.trim_end());
    if path == "/dev/null" {
        return None;
    }
    Some(path.strip_prefix(prefix).unwrap_or(&path).to_owned())
}

/// `@@ -a[,b] +c[,d] @@` → (old start, new start).
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let body = line.strip_prefix("@@ ")?;
    let mut parts = body.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |s: &str| s.split(',').next()?.parse::<u32>().ok();
    Some((start(old)?, start(new)?))
}

/// Position inside the hunks of one file.
#[derive(Default)]
struct Cursor {
    old: u32,
    new: u32,
    in_hunk: bool,
}

/// A `+`/`-`/`\` line inside a hunk; false when the line ends the hunk.
fn hunk_line(cur: &mut FileChange, at: &mut Cursor, line: &str) -> bool {
    if let Some(rest) = line.strip_prefix('+') {
        cur.added.push((at.new, rest.to_owned()));
        at.new += 1;
    } else if let Some(rest) = line.strip_prefix('-') {
        cur.removed.push((at.old, rest.to_owned()));
        at.old += 1;
    } else if !line.starts_with('\\') {
        return false; // "\ No newline at end of file" stays inside
    }
    true
}

/// "Binary files a/x and b/y differ": the new side, else the old one.
fn binary_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Binary files ")?;
    let (a, b) = rest.trim_end_matches(" differ").split_once(" and ")?;
    side_path(b, "b/").or_else(|| side_path(a, "a/"))
}

/// A file header line (mode, rename, `---`/`+++`, binary marker).
fn header_line(cur: &mut FileChange, line: &str) {
    if line.starts_with("new file mode") {
        cur.status = Status::Added;
    } else if line.starts_with("deleted file mode") {
        cur.status = Status::Deleted;
    } else if let Some(p) = line.strip_prefix("rename from ") {
        cur.old_path = Some(unquote(p));
        cur.status = Status::Renamed;
    } else if let Some(p) = line.strip_prefix("rename to ") {
        cur.path = unquote(p);
    } else if let Some(p) = line.strip_prefix("--- ") {
        if cur.path.is_empty()
            && let Some(old) = side_path(p, "a/")
        {
            cur.path = old;
        }
    } else if let Some(p) = line.strip_prefix("+++ ") {
        if let Some(new) = side_path(p, "b/") {
            cur.path = new;
        }
    } else if line.starts_with("Binary files ") {
        cur.binary = true;
        if cur.path.is_empty() {
            cur.path = binary_path(line).unwrap_or_default();
        }
    }
}

/// Parse `git diff -U0` output.
pub fn parse(text: &str) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut at = Cursor::default();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with("diff --git ") {
            files.push(FileChange::new(String::new(), Status::Modified));
            at.in_hunk = false;
            continue;
        }
        let Some(cur) = files.last_mut() else {
            continue;
        };
        if at.in_hunk && hunk_line(cur, &mut at, line) {
            continue;
        }
        at.in_hunk = false;
        if let Some((o, n)) = hunk_starts(line) {
            at = Cursor {
                old: o,
                new: n,
                in_hunk: true,
            };
        } else {
            header_line(cur, line);
        }
    }
    files.retain(|f| !f.path.is_empty());
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.py b/src/a.py
index 1..2 100644
--- a/src/a.py
+++ b/src/a.py
@@ -3 +3,2 @@ def f():
-    return 1
+    return 2  # noqa
+    x = 1
@@ -10,0 +12 @@
+pass
diff --git a/old.py b/new.py
similarity index 90%
rename from old.py
rename to new.py
--- a/old.py
+++ b/new.py
@@ -1 +1 @@
-a
+b
diff --git a/tests/test_x.py b/tests/test_x.py
deleted file mode 100644
--- a/tests/test_x.py
+++ /dev/null
@@ -1,2 +0,0 @@
-def test_x():
-    assert True
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
diff --git \"a/sp ace.txt\" \"b/sp ace.txt\"
new file mode 100644
--- /dev/null
+++ \"b/sp ace.txt\"
@@ -0,0 +1 @@
+hi
\\ No newline at end of file
";

    #[test]
    fn parses_hunks_renames_deletions_binaries_and_quoted_paths() {
        let f = parse(DIFF);
        assert_eq!(f.len(), 5);
        assert_eq!(f[0].path, "src/a.py");
        assert_eq!(f[0].status, Status::Modified);
        assert_eq!(f[0].removed, vec![(3, "    return 1".to_owned())]);
        assert_eq!(
            f[0].added,
            vec![
                (3, "    return 2  # noqa".to_owned()),
                (4, "    x = 1".to_owned()),
                (12, "pass".to_owned())
            ]
        );
        assert_eq!(
            (f[1].status, f[1].path.as_str()),
            (Status::Renamed, "new.py")
        );
        assert_eq!(f[1].old_path.as_deref(), Some("old.py"));
        assert_eq!(
            (f[2].status, f[2].path.as_str()),
            (Status::Deleted, "tests/test_x.py")
        );
        assert_eq!(f[2].removed.len(), 2);
        assert!(f[3].binary && f[3].status == Status::Added && f[3].path == "img.png");
        assert_eq!(f[4].path, "sp ace.txt");
        assert_eq!(f[4].added, vec![(1, "hi".to_owned())]);
    }

    #[test]
    fn unquotes_octal_escapes() {
        assert_eq!(unquote("\"caf\\303\\251.py\""), "café.py");
        assert_eq!(unquote("plain.py"), "plain.py");
    }
}
