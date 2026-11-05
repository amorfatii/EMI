//! Verified git snapshots (port of `emi_snapshot.py`): refs resolve once to
//! pinned commits, and revisions materialize from raw tracked blobs — never
//! `git archive`, whose `export-ignore`/`export-subst` would silently
//! rewrite the measured tree. Gitlinks fail closed; symlinks are recreated
//! (never followed) so the scan reports them.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

/// A name as messages show it: quoted, with anything unprintable escaped.
fn quoted(s: &str) -> String {
    format!("{s:?}")
}

pub type Result<T> = std::result::Result<T, String>;

/// A scratch directory for materialized revisions, created fresh (never
/// one that already exists) and removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> Result<Self> {
        let base = std::env::temp_dir();
        for attempt in 0u32..100 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let path = base.join(format!("{prefix}{}-{nanos}-{attempt}", std::process::id()));
            if std::fs::create_dir(&path).is_ok() {
                return Ok(TempDir { path });
            }
        }
        Err("cannot create a temporary directory".into())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The prefixes of the scratch directories EMI's crates create in the
/// temporary directory, each followed by the creating process's id.
const SCRATCH_PREFIXES: &[&str] = &[
    "emi-verify-",
    "emi-git-view-",
    "emi-profile-",
    "emi-proving-",
    "emi-diff-",
    "emi-measure-",
    "emi-places-",
    "emi-report-",
    "emi-done-index-",
    "emi-seccomp-",
];

/// The process a scratch directory of this name belongs to: the name is a
/// known prefix, then words, then `<pid>-<nanos>` or
/// `<pid>-<nanos>-<attempt>`. `None` for any other name, which is then
/// nobody's to remove.
pub fn scratch_owner(name: &str) -> Option<u32> {
    let rest = SCRATCH_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))?;
    let numbers: Vec<&str> = rest
        .rsplit('-')
        .take_while(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        .collect();
    match numbers.len() {
        // collected from the right: the process id is the last one
        2 | 3 => numbers.last()?.parse().ok(),
        _ => None,
    }
}

/// Remove the scratch directories and files that runs which no longer exist left in
/// the temporary directory. A run that is killed (a harness timeout, an
/// interrupt) cannot remove its copies of the repository, and they would
/// otherwise stay until the machine restarts, often in memory. Only
/// directories this user owns, named as EMI names them, whose process is
/// gone are touched; where the process table cannot be read, nothing is.
/// Returns how many were removed.
pub fn sweep_stale_scratch() -> usize {
    use std::os::unix::fs::MetadataExt;
    let proc = Path::new("/proc");
    let Ok(me) = fs::metadata(proc.join("self")).map(|m| m.uid()) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = scratch_owner(&name.to_string_lossy()) else {
            continue;
        };
        let Some(meta) = entry.metadata().ok().filter(|m| m.uid() == me) else {
            continue;
        };
        if proc.join(pid.to_string()).exists() {
            continue;
        }
        let gone = if meta.is_dir() {
            fs::remove_dir_all(entry.path())
        } else {
            fs::remove_file(entry.path())
        };
        removed += usize::from(gone.is_ok());
    }
    removed
}

fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `git -C repo args...`; failures carry the oracle's message shape.
pub(crate) fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git_bytes_env(repo, &[], args)
}

/// [`git_bytes`] with extra environment variables (a private index file).
fn git_bytes_env(repo: &Path, env: &[(&str, &Path)], args: &[&str]) -> Result<Vec<u8>> {
    let out = emi_git::command(repo)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
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
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    match lines.as_slice() {
        [one] if is_sha(one.trim()) => Some(one.trim().to_owned()),
        _ => None,
    }
}

/// `resolve_ref`: a ref to a verified full commit SHA (annotated tags
/// peeled; option-like, multi-result and non-commit refs refused).
pub fn resolve_ref(repo: &Path, reference: &str) -> Result<String> {
    let shown = quoted(reference);
    if reference.trim().is_empty() {
        return Err(format!("ref must be a nonempty string, got {shown}"));
    }
    if reference.contains('\0') || reference.contains('\n') {
        return Err(format!("ref contains forbidden characters: {shown}"));
    }
    if str::trim(reference).starts_with('-') {
        return Err(format!("ref looks like an option, refusing: {shown}"));
    }
    ensure_repo(repo)?;
    let spec = format!("{reference}^{{commit}}");
    let out = git_text(repo, &["rev-parse", "--verify", "--end-of-options", &spec])?;
    let lines = out.trim().lines().filter(|l| !l.trim().is_empty()).count();
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
        if !is_sha(str::trim(sha)) {
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
    let stripped = str::trim(source);
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

/// Text files at commit `rev` whose path satisfies `keep`, as
/// `(path, text)`; binary blobs (NUL bytes) and files over `max_bytes`
/// are skipped. One `ls-tree` and one `cat-file --batch`.
pub fn text_files_at(
    repo: &Path,
    rev: &str,
    keep: impl Fn(&str) -> bool,
    max_bytes: usize,
) -> Result<Vec<(String, String)>> {
    let entries = parse_ls_tree(&git_bytes(
        repo,
        &["ls-tree", "-r", "-z", "--full-tree", rev, "--"],
    )?)?;
    let wanted: Vec<(String, String)> = entries
        .into_iter()
        .filter(|e| e.kind == "blob" && e.mode != "120000")
        .map(|e| (String::from_utf8_lossy(&e.path).into_owned(), e.sha))
        .filter(|(p, _)| keep(p))
        .collect();
    let shas: Vec<String> = wanted.iter().map(|(_, s)| s.clone()).collect();
    let blobs = read_blobs(repo, &shas)?;
    Ok(wanted
        .into_iter()
        .zip(blobs)
        .filter(|(_, b)| b.len() <= max_bytes && !b.contains(&0))
        .map(|((p, _), b)| (p, String::from_utf8_lossy(&b).into_owned()))
        .collect())
}

/// Read many blobs through one `git cat-file --batch`.
fn read_blobs(repo: &Path, shas: &[String]) -> Result<Vec<Vec<u8>>> {
    let mut child = emi_git::command(repo)
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
    let shown = quoted(&String::from_utf8_lossy(&entry.path));
    let target = dest.join(&rel);
    if let Some(parent) = target.parent() {
        // checked before creating anything: a symlink written earlier must
        // not lead directory creation outside the tree
        assert_contained(dest, parent)?;
        fs::create_dir_all(parent).map_err(|e| format!("cannot write {shown}: {e}"))?;
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
/// Any gitlink (submodule) is refused, as in the oracle.
pub fn materialize(repo: &Path, sha: &str, dest: &Path) -> Result<Inventory> {
    materialize_scoped(repo, sha, dest, None)
}

/// Whether `path` lies within the repo-relative `scope` (`""` is the root).
fn within(path: &str, scope: &str) -> bool {
    scope.is_empty() || path == scope || path.starts_with(&format!("{scope}/"))
}

/// [`materialize`], but with `Some(scope)` gitlinks outside the measured
/// scope are skipped (left unmaterialized) instead of refused: a submodule
/// of assets beside `src/` cannot change what `src/` measures.
pub fn materialize_scoped(
    repo: &Path,
    sha: &str,
    dest: &Path,
    scope: Option<&str>,
) -> Result<Inventory> {
    let sha = str::trim(sha);
    if !is_sha(sha) {
        return Err(format!(
            "materialize needs a commit SHA, got {}",
            quoted(sha)
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
            return Err(format!("tracked path escapes revision: {}", quoted(&text)));
        }
        if entry.mode == "160000" || entry.kind == "commit" {
            if scope.is_none_or(|s| within(&text, s)) {
                gitlinks.push(text);
            }
        } else if entry.mode != "120000" && entry.kind != "blob" {
            return Err(format!(
                "unsupported tracked object {}: {} {}",
                quoted(&text),
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

/// Symlink each of `rels` (ignored dependency directories such as a
/// virtualenv or `node_modules`) from `repo` into the materialized tree at
/// `dest`, where it exists in the repository. The tree must not already
/// hold that path: a committed entry there (a symlink to another
/// virtualenv) would replace the dependencies the caller pinned. Returns
/// the linked sources, which a sandbox mounts back read-only.
pub fn link_dependencies(repo: &Path, dest: &Path, rels: &[String]) -> Result<Vec<PathBuf>> {
    let mut linked = Vec::new();
    for rel in rels {
        if escapes(rel) {
            return Err(format!("dependency link escapes the tree: {rel}"));
        }
        let (src, dst) = (repo.join(rel), dest.join(rel));
        if dst.symlink_metadata().is_ok() {
            return Err(format!(
                "the tree itself contains {rel}, which is linked from the repository; remove it from the tree or from the link list"
            ));
        }
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("cannot link {rel}: {e}"))?;
            }
            symlink(&src, &dst).map_err(|e| format!("cannot link {rel}: {e}"))?;
            linked.push(src);
        }
    }
    Ok(linked)
}

/// Freeze the working tree of `repo` into a commit object inside `view`
/// (a clean view of `repo`'s objects): tracked and untracked-but-not-ignored
/// files as they are on disk, deleted files absent, the parent the current
/// HEAD. Every later read of the candidate goes through this one commit, so
/// an edit made while it is being checked cannot give two gates two
/// different trees. The bytes are hashed raw (`--no-filters`): no clean
/// filter, attribute or hook of the repository runs, and nothing is written
/// to its index, refs or object store. `scratch` holds the private index.
pub fn commit_worktree(repo: &Path, view: &Path, scratch: &Path) -> Result<String> {
    ensure_repo(repo)?;
    fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
    let top = PathBuf::from(git_text(repo, &["rev-parse", "--show-toplevel"])?.trim());
    let (paths, gitlinks) = worktree_paths(repo)?;
    let entries = hash_worktree(view, &top, scratch, &paths, &gitlinks)?;
    let tree = write_tree(view, scratch, &entries)?;
    let head = resolve_ref(repo, "HEAD").ok();
    let mut args = vec!["-c", "user.name=emi", "-c", "user.email=emi@localhost"];
    args.extend(["-c", "commit.gpgsign=false"]);
    args.extend(["commit-tree", tree.as_str(), "-m", "emi worktree snapshot"]);
    if let Some(h) = &head {
        args.extend(["-p", h.as_str()]);
    }
    let out = git_text(view, &args)?;
    single_sha(out.trim()).ok_or_else(|| "commit-tree did not return a commit SHA".to_owned())
}

/// Tracked and untracked-but-not-ignored paths, and the tracked gitlinks
/// (submodules) with their recorded commits.
/// Paths of the working tree, and gitlink paths to their commits.
type WorktreePaths = (Vec<Vec<u8>>, BTreeMap<Vec<u8>, String>);

fn worktree_paths(repo: &Path) -> Result<WorktreePaths> {
    let mut paths = std::collections::BTreeSet::new();
    let mut gitlinks = BTreeMap::new();
    let staged = git_bytes(repo, &["ls-files", "-z", "-s", "--full-name", "--", ":/"])?;
    for entry in staged.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            return Err("cannot parse ls-files -s entry".into());
        };
        let meta = String::from_utf8_lossy(&entry[..tab]).into_owned();
        let path = entry[tab + 1..].to_vec();
        let mut fields = meta.split(' ');
        if let (Some("160000"), Some(sha)) = (fields.next(), fields.next()) {
            gitlinks.insert(path.clone(), sha.to_owned());
        }
        paths.insert(path);
    }
    let others = git_bytes(
        repo,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--full-name",
            "--",
            ":/",
        ],
    )?;
    paths.extend(
        others
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(<[u8]>::to_vec),
    );
    Ok((paths.into_iter().collect(), gitlinks))
}

/// One entry of the snapshot tree: mode, object type, id, path.
struct SnapEntry {
    mode: &'static str,
    kind: &'static str,
    sha: String,
    path: Vec<u8>,
}

#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &fs::Metadata) -> bool {
    false
}

/// Hash every present path into `view`: regular files by content,
/// symlinks by their target (written to a scratch file first), gitlinks
/// by their recorded commit. Missing paths (deleted) are left out.
fn hash_worktree(
    view: &Path,
    top: &Path,
    scratch: &Path,
    paths: &[Vec<u8>],
    gitlinks: &BTreeMap<Vec<u8>, String>,
) -> Result<Vec<SnapEntry>> {
    let (mut pending, mut sources) = (Vec::new(), Vec::new());
    let mut entries = Vec::new();
    for (n, path) in paths.iter().enumerate() {
        let full = top.join(os_path(path));
        let Ok(meta) = full.symlink_metadata() else {
            continue;
        };
        let shown = String::from_utf8_lossy(path).into_owned();
        if let Some(sha) = gitlinks.get(path) {
            entries.push(SnapEntry {
                mode: "160000",
                kind: "commit",
                sha: sha.clone(),
                path: path.clone(),
            });
            continue;
        }
        let (mode, source) = if meta.file_type().is_symlink() {
            let target = fs::read_link(&full).map_err(|e| format!("{shown}: {e}"))?;
            let holder = scratch.join(format!("link-{n}"));
            fs::write(&holder, target.as_os_str().as_encoded_bytes())
                .map_err(|e| format!("{shown}: {e}"))?;
            ("120000", holder)
        } else if meta.is_file() {
            (
                if executable(&meta) {
                    "100755"
                } else {
                    "100644"
                },
                full,
            )
        } else {
            continue;
        };
        let text = source.to_string_lossy().into_owned();
        if text.contains('\n') {
            return Err(format!(
                "cannot snapshot {shown}: its path contains a newline"
            ));
        }
        sources.push(text);
        pending.push((mode, path.clone()));
    }
    let input: String = sources.iter().map(|s| format!("{s}\n")).collect();
    let out = emi_git::run_with(
        view,
        &["hash-object", "-w", "--no-filters", "--stdin-paths"],
        &[],
        Some(input.as_bytes()),
    )?;
    let shas: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_owned)
        .collect();
    if shas.len() != pending.len() {
        return Err(format!(
            "hash-object returned {} ids for {} files",
            shas.len(),
            pending.len()
        ));
    }
    for ((mode, path), sha) in pending.into_iter().zip(shas) {
        entries.push(SnapEntry {
            mode,
            kind: "blob",
            sha,
            path,
        });
    }
    Ok(entries)
}

/// The tree of `entries`, built in `view` through a private index.
fn write_tree(view: &Path, scratch: &Path, entries: &[SnapEntry]) -> Result<String> {
    let index = scratch.join("snapshot.index");
    let env = [("GIT_INDEX_FILE", index.as_path())];
    let mut input = Vec::new();
    for e in entries {
        input.extend_from_slice(format!("{} {} {}\t", e.mode, e.kind, e.sha).as_bytes());
        input.extend_from_slice(&e.path);
        input.push(0);
    }
    emi_git::run_with(
        view,
        &["update-index", "--add", "-z", "--index-info"],
        &env,
        Some(&input),
    )?;
    let tree = emi_git::run_with(view, &["write-tree"], &env, None)?;
    let _ = fs::remove_file(&index);
    Ok(String::from_utf8_lossy(&tree).trim().to_owned())
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
                .map_err(|e| format!("cannot write {}: {e}", quoted(&text)))?;
        }
        if meta.file_type().is_symlink() {
            let link = fs::read_link(&source).map_err(|e| e.to_string())?;
            symlink(&link, &target)
                .map_err(|e| format!("cannot materialize symlink {}: {e}", quoted(&text)))?;
            symlinks.push(text);
        } else if meta.is_file() {
            fs::copy(&source, &target)
                .map_err(|e| format!("cannot write {}: {e}", quoted(&text)))?;
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

    #[test]
    fn a_scratch_name_says_whose_it_is_and_other_names_say_nothing() {
        let cases = [
            ("emi-verify-4242-1790885403020206177", Some(4242)),
            ("emi-git-view-17-99", Some(17)),
            ("emi-diff-1029893-439168965-0", Some(1_029_893)),
            ("emi-profile-baseline-list-77-123", Some(77)),
            ("emi-done-index-5-6-0", Some(5)),
            // not EMI's scratch, or not named the way it names scratch
            ("emi-audit-20261001", None),
            ("emi-verify-4242", None),
            ("emi-verify-a-b", None),
            ("emi-verify-1-2-3-4", None),
            ("other-1-2", None),
        ];
        for (name, want) in cases {
            assert_eq!(scratch_owner(name), want, "{name}");
        }
    }

    #[test]
    fn scratch_of_a_process_that_is_gone_is_swept_and_a_live_ones_is_kept() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .unwrap_or_else(|e| panic!("true: {e}"));
        let gone = child.id();
        child.wait().unwrap_or_else(|e| panic!("wait: {e}"));
        let tmp = std::env::temp_dir();
        let stale = tmp.join(format!("emi-verify-{gone}-1"));
        let live = tmp.join(format!("emi-verify-{}-1", std::process::id()));
        for dir in [&stale, &live] {
            fs::create_dir_all(dir.join("candidate")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        }
        assert!(sweep_stale_scratch() >= 1);
        assert!(!stale.exists());
        assert!(live.exists());
        fs::remove_dir_all(&live).unwrap_or_else(|e| panic!("cleanup: {e}"));
    }
}
