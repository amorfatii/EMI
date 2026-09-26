//! Verified git snapshots (port of `emi_snapshot.py`): refs resolve once to
//! pinned commits, and revisions materialize from raw tracked blobs — never
//! `git archive`, whose `export-ignore`/`export-subst` would silently
//! rewrite the measured tree. Gitlinks fail closed; symlinks are recreated
//! (never followed) so the scan reports them.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use emi_ir::pyrepr;

pub type Result<T> = std::result::Result<T, String>;

fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `git -C repo args...`; failures carry the oracle's message shape.
fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "emi diff requires a git binary on PATH".to_owned(),
            _ => format!(
                "git {} failed: {e}",
                args.iter().take(3).copied().collect::<Vec<_>>().join(" ")
            ),
        })?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let raw = if out.stderr.is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    let detail = String::from_utf8_lossy(&raw[..raw.len().min(300)])
        .trim()
        .to_owned();
    Err(format!(
        "git {} failed: {detail}",
        args.iter().take(3).copied().collect::<Vec<_>>().join(" ")
    ))
}

fn git_text(repo: &Path, args: &[&str]) -> Result<String> {
    git_bytes(repo, args).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn ensure_repo(repo: &Path) -> Result<()> {
    git_text(repo, &["rev-parse", "--git-dir"]).map(|_| ())
}

fn object_type(repo: &Path, sha: &str) -> Result<String> {
    Ok(
        git_text(repo, &["cat-file", "-t", "--end-of-options", sha])?
            .trim()
            .to_owned(),
    )
}

fn single_sha(out: &str) -> Option<String> {
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| !emi_ir::pytext::is_blank(l))
        .collect();
    match lines.as_slice() {
        [one] if is_sha(one.trim()) => Some(one.trim().to_owned()),
        _ => None,
    }
}

/// `resolve_ref`: a ref to a verified full commit SHA (annotated tags
/// peeled; option-like, multi-result and non-commit refs refused).
pub fn resolve_ref(repo: &Path, reference: &str) -> Result<String> {
    let shown = pyrepr::string(reference);
    if emi_ir::pytext::is_blank(reference) {
        return Err(format!("ref must be a nonempty string, got {shown}"));
    }
    if reference.contains('\0') || reference.contains('\n') {
        return Err(format!("ref contains forbidden characters: {shown}"));
    }
    if emi_ir::pytext::strip(reference).starts_with('-') {
        return Err(format!("ref looks like an option, refusing: {shown}"));
    }
    ensure_repo(repo)?;
    let spec = format!("{reference}^{{commit}}");
    let out = git_text(repo, &["rev-parse", "--verify", "--end-of-options", &spec])?;
    let lines = out
        .trim()
        .lines()
        .filter(|l| !emi_ir::pytext::is_blank(l))
        .count();
    if lines != 1 {
        return Err(format!(
            "ref {shown} resolved to {lines} results, need exactly one"
        ));
    }
    let Some(sha) = single_sha(out.trim()) else {
        return Err(format!("ref {shown} did not resolve to a commit SHA"));
    };
    let kind = object_type(repo, &sha)?;
    if kind != "commit" {
        return Err(format!("ref {shown} is a {kind}, need a commit"));
    }
    Ok(sha)
}

/// `merge_base` of two pinned commit SHAs.
pub fn merge_base(repo: &Path, a: &str, b: &str) -> Result<String> {
    for (label, sha) in [("a", a), ("b", b)] {
        if !is_sha(emi_ir::pytext::strip(sha)) {
            return Err(format!("merge-base {label} must be a commit SHA"));
        }
    }
    ensure_repo(repo)?;
    let out = git_text(
        repo,
        &["merge-base", "--end-of-options", a.trim(), b.trim()],
    )?;
    single_sha(out.trim()).ok_or_else(|| "merge-base did not return a single commit SHA".to_owned())
}

const ESCAPE: &str = "source must be a repository-relative path without '..' (audit F06)";

fn escapes(path: &str) -> bool {
    path.starts_with('/') || path.split('/').any(|p| p == "..")
}

/// `normalize_source`: repository-relative scope, `""` for the root.
pub fn normalize_source(source: &str) -> Result<String> {
    if source.contains('\0') {
        return Err("source contains NUL byte".into());
    }
    let stripped = emi_ir::pytext::strip(source);
    if stripped.is_empty() || stripped == "." {
        return Ok(String::new());
    }
    if escapes(stripped) {
        return Err(ESCAPE.into());
    }
    let mut norm = stripped.trim_end_matches('/');
    while let Some(rest) = norm.strip_prefix("./") {
        norm = rest;
    }
    if norm.is_empty() || norm == "." {
        return Ok(String::new());
    }
    if escapes(norm) {
        return Err(ESCAPE.into());
    }
    Ok(norm.to_owned())
}

/// `Path.resolve()` (non-strict): canonicalize the longest existing
/// ancestor, then apply the remaining components lexically.
fn resolve_lenient(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while fs::canonicalize(&existing).is_err() {
        match (
            existing.file_name().map(ToOwned::to_owned),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = fs::canonicalize(&existing).unwrap_or(existing);
    for part in rest.into_iter().rev() {
        match Path::new(&part).components().next() {
            Some(Component::ParentDir) => {
                out.pop();
            }
            Some(Component::CurDir) => {}
            _ => out.push(part),
        }
    }
    out
}

/// `assert_contained`: structural containment, never a string prefix.
pub fn assert_contained(root: &Path, scope: &Path) -> Result<()> {
    if resolve_lenient(scope).starts_with(resolve_lenient(root)) {
        Ok(())
    } else {
        Err("source must stay inside the exported revision (audit F06)".into())
    }
}

struct Entry {
    mode: String,
    kind: String,
    sha: String,
    path: Vec<u8>,
}

fn parse_ls_tree(raw: &[u8]) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for chunk in raw.split(|b| *b == 0).filter(|c| !c.is_empty()) {
        let tab = chunk.iter().position(|b| *b == b'\t');
        let Some(tab) = tab else {
            return Err(
                "cannot parse ls-tree entry: not enough values to unpack (expected 2, got 1)"
                    .into(),
            );
        };
        let meta = String::from_utf8_lossy(&chunk[..tab]).into_owned();
        let fields: Vec<&str> = meta.splitn(3, ' ').collect();
        let [mode, kind, sha] = fields.as_slice() else {
            return Err("cannot parse ls-tree entry: bad metadata".into());
        };
        entries.push(Entry {
            mode: (*mode).into(),
            kind: (*kind).into(),
            sha: (*sha).into(),
            path: chunk[tab + 1..].to_vec(),
        });
    }
    Ok(entries)
}

#[cfg(unix)]
fn os_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Read many blobs through one `git cat-file --batch`.
fn read_blobs(repo: &Path, shas: &[String]) -> Result<Vec<Vec<u8>>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("git cat-file --batch failed: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git cat-file --batch: no stdin")?;
    let request: String = shas.iter().map(|s| format!("{s}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
    let stdout = child
        .stdout
        .take()
        .ok_or("git cat-file --batch: no stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut blobs = Vec::with_capacity(shas.len());
    for sha in shas {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(|e| e.to_string())?;
        let size: usize = header
            .split_whitespace()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("git cat-file blob {sha} failed: {}", header.trim()))?;
        let mut blob = vec![0; size + 1];
        reader.read_exact(&mut blob).map_err(|e| e.to_string())?;
        blob.pop();
        blobs.push(blob);
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(blobs)
}

/// What a materialized revision contained.
pub struct Inventory {
    pub commit: String,
    pub tree: String,
    pub files: Vec<String>,
    pub symlinks: Vec<String>,
}

fn write_entry(dest: &Path, entry: &Entry, blob: &[u8]) -> Result<()> {
    let rel = os_path(&entry.path);
    let shown = pyrepr::string(&String::from_utf8_lossy(&entry.path));
    let target = dest.join(&rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot write {shown}: {e}"))?;
        assert_contained(dest, parent)?;
    }
    if entry.mode == "120000" {
        let link = os_path(blob);
        if target.symlink_metadata().is_ok() {
            fs::remove_file(&target).map_err(|e| format!("cannot clear {shown}: {e}"))?;
        }
        return symlink(&link, &target)
            .map_err(|e| format!("cannot materialize symlink {shown}: {e}"));
    }
    fs::write(&target, blob).map_err(|e| format!("cannot write {shown}: {e}"))?;
    if entry.mode == "100755" {
        set_executable(&target);
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(link: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link, target)
}

#[cfg(not(unix))]
fn symlink(_link: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "symlinks unsupported on this platform",
    ))
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

/// `materialize_revision`: raw tracked blobs of commit `sha` into `dest`.
pub fn materialize(repo: &Path, sha: &str, dest: &Path) -> Result<Inventory> {
    let sha = emi_ir::pytext::strip(sha);
    if !is_sha(sha) {
        return Err(format!(
            "materialize needs a commit SHA, got {}",
            pyrepr::string(sha)
        ));
    }
    ensure_repo(repo)?;
    let kind = object_type(repo, sha)?;
    if kind != "commit" {
        return Err(format!("materialize needs a commit, got {kind}"));
    }
    let tree_spec = format!("{sha}^{{tree}}");
    let tree = git_text(
        repo,
        &["rev-parse", "--verify", "--end-of-options", &tree_spec],
    )?
    .trim()
    .to_owned();
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let entries = parse_ls_tree(&git_bytes(
        repo,
        &["ls-tree", "-r", "-z", "--full-tree", sha, "--"],
    )?)?;
    let (mut gitlinks, mut wanted) = (Vec::new(), Vec::new());
    for entry in &entries {
        let text = String::from_utf8_lossy(&entry.path).into_owned();
        if escapes(&text) {
            return Err(format!(
                "tracked path escapes revision: {}",
                pyrepr::string(&text)
            ));
        }
        if entry.mode == "160000" || entry.kind == "commit" {
            gitlinks.push(text);
        } else if entry.mode != "120000" && entry.kind != "blob" {
            return Err(format!(
                "unsupported tracked object {}: {} {}",
                pyrepr::string(&text),
                entry.mode,
                entry.kind
            ));
        } else {
            wanted.push(entry);
        }
    }
    if !gitlinks.is_empty() {
        gitlinks.sort();
        gitlinks.truncate(10);
        return Err(format!(
            "gitlink/submodule entries unsupported (not materialized): {}",
            gitlinks.join(", ")
        ));
    }
    let shas: Vec<String> = wanted.iter().map(|e| e.sha.clone()).collect();
    let blobs = read_blobs(repo, &shas)?;
    let (mut files, mut symlinks) = (Vec::new(), Vec::new());
    for (entry, blob) in wanted.iter().zip(&blobs) {
        write_entry(dest, entry, blob)?;
        let text = String::from_utf8_lossy(&entry.path).into_owned();
        if entry.mode == "120000" {
            symlinks.push(text)
        } else {
            files.push(text)
        }
    }
    files.sort();
    symlinks.sort();
    Ok(Inventory {
        commit: sha.to_owned(),
        tree,
        files,
        symlinks,
    })
}

/// The working tree as a commit of it would contain it: tracked and
/// untracked-but-not-ignored files, copied from disk (deleted files are
/// absent). Lets an agent gate uncommitted work.
pub fn materialize_worktree(repo: &Path, dest: &Path) -> Result<Inventory> {
    ensure_repo(repo)?;
    let top = git_text(repo, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_owned();
    let top = PathBuf::from(top);
    let listed = git_bytes(
        repo,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--full-name",
            "--",
            ":/",
        ],
    )?;
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let (mut files, mut symlinks) = (Vec::new(), Vec::new());
    let mut seen = std::collections::HashSet::new();
    for raw in listed.split(|b| *b == 0).filter(|c| !c.is_empty()) {
        let text = String::from_utf8_lossy(raw).into_owned();
        if escapes(&text) || !seen.insert(text.clone()) {
            continue;
        }
        let rel = os_path(raw);
        let source = top.join(&rel);
        let Ok(meta) = source.symlink_metadata() else {
            continue;
        };
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot write {}: {e}", pyrepr::string(&text)))?;
        }
        if meta.file_type().is_symlink() {
            let link = fs::read_link(&source).map_err(|e| e.to_string())?;
            symlink(&link, &target).map_err(|e| {
                format!("cannot materialize symlink {}: {e}", pyrepr::string(&text))
            })?;
            symlinks.push(text);
        } else if meta.is_file() {
            fs::copy(&source, &target)
                .map_err(|e| format!("cannot write {}: {e}", pyrepr::string(&text)))?;
            files.push(text);
        }
    }
    files.sort();
    symlinks.sort();
    Ok(Inventory {
        commit: "worktree".into(),
        tree: "worktree".into(),
        files,
        symlinks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_normalization_matches_oracle() {
        assert_eq!(normalize_source(" ./src/ ").as_deref(), Ok("src"));
        assert_eq!(normalize_source(".").as_deref(), Ok(""));
        assert_eq!(normalize_source("././").as_deref(), Ok(""));
        assert!(normalize_source("../x").is_err());
        assert!(normalize_source("/abs").is_err());
        assert!(normalize_source("a/../b").is_err());
    }
}
