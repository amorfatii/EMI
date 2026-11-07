//! Linked dependency directories (a virtualenv, `node_modules`) are not part
//! of the candidate's tree, yet the checks run the tools inside them: an
//! agent that edits its test runner there could make any check pass
//! without touching a tracked file. The contract pins a digest of each
//! linked directory; a verification whose directories no longer match
//! cannot count its checks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value, json};

use crate::engine::contract::Result;

/// What a linked path held when it was pinned, if it did not exist.
const ABSENT: &str = "absent";

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FileStamp {
    dev: u64,
    ino: u64,
    size: u64,
    mode: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(unix)]
fn stamp_if_settled(meta: &std::fs::Metadata) -> Option<FileStamp> {
    use std::os::unix::fs::MetadataExt;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())?;
    let mtime = meta.mtime();
    let ctime = meta.ctime();
    // Only trust stat stamps for files that settled at least 2 seconds ago
    // (Git's racy-clean rule), so sub-second in-place overwrites of equal
    // byte length always read and hash the file contents.
    if now.saturating_sub(mtime) < 2 || now.saturating_sub(ctime) < 2 {
        return None;
    }
    Some(FileStamp {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.len(),
        mode: mode(meta),
        mtime,
        mtime_nsec: meta.mtime_nsec(),
        ctime,
        ctime_nsec: meta.ctime_nsec(),
    })
}

#[cfg(unix)]
fn digest_cache() -> &'static Mutex<HashMap<FileStamp, String>> {
    static CACHE: OnceLock<Mutex<HashMap<FileStamp, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_digest(path: &Path, meta: &std::fs::Metadata) -> Result<String> {
    #[cfg(unix)]
    let stamp = stamp_if_settled(meta);
    #[cfg(unix)]
    if let Some(s) = stamp
        && let Ok(guard) = digest_cache().lock()
        && let Some(cached) = guard.get(&s)
    {
        return Ok(cached.clone());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digest = emi_kernel::hash::sha256_hex(&bytes);
    #[cfg(unix)]
    if let Some(s) = stamp
        && let Ok(mut guard) = digest_cache().lock()
    {
        guard.insert(s, digest.clone());
    }
    Ok(digest)
}

fn file_line((rel, path, meta): &(String, PathBuf, std::fs::Metadata)) -> Result<String> {
    let digest = file_digest(path, meta)?;
    Ok(format!("{rel}\0f {} {digest}\n", mode(meta)))
}

/// Digest of everything under `root` (not following links): every entry's
/// relative path and kind, file contents and link targets.
pub fn tree_digest(root: &Path) -> Result<String> {
    let mut lines = Vec::new();
    let mut files = Vec::new();
    walk(root, root, &mut lines, &mut files)?;
    if files.len() <= 32 {
        for item in &files {
            lines.push(file_line(item)?);
        }
    } else {
        let workers = std::thread::available_parallelism()
            .map_or(4, std::num::NonZeroUsize::get)
            .min(files.len());
        let chunk_size = files.len().div_ceil(workers);
        let chunks: Vec<Result<Vec<String>>> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .chunks(chunk_size)
                .map(|chunk| s.spawn(move || chunk.iter().map(file_line).collect()))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("file digest worker panicked".to_owned()))
                })
                .collect()
        });
        for chunk in chunks {
            lines.extend(chunk?);
        }
    }
    lines.sort();
    Ok(emi_kernel::hash::sha256_hex(lines.concat().as_bytes()))
}

fn walk(
    root: &Path,
    dir: &Path,
    lines: &mut Vec<String>,
    files: &mut Vec<(String, PathBuf, std::fs::Metadata)>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let meta = path
            .symlink_metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if meta.file_type().is_symlink() {
            let target =
                std::fs::read_link(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            lines.push(format!("{rel}\0l {}\n", target.to_string_lossy()));
        } else if meta.is_dir() {
            walk(root, &path, lines, files)?;
            lines.push(format!("{rel}\0d\n"));
        } else if meta.is_file() {
            files.push((rel, path, meta));
        } else {
            lines.push(format!("{rel}\0o\n"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111
}

#[cfg(not(unix))]
fn mode(_meta: &std::fs::Metadata) -> u32 {
    0
}

fn linked(execution: &Value) -> Vec<String> {
    execution["link"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

fn state_of(repo: &Path, rel: &str) -> Result<String> {
    let path = repo.join(rel);
    let Ok(meta) = path.symlink_metadata() else {
        return Ok(ABSENT.to_owned());
    };
    // a single linked file (a generated module the tests import) is pinned
    // by its own content and mode
    if meta.is_file() {
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot pin linked {rel}: {e}"))?;
        let line = format!(
            "f {} {}\n",
            mode(&meta),
            emi_kernel::hash::sha256_hex(&bytes)
        );
        return Ok(emi_kernel::hash::sha256_hex(line.as_bytes()));
    }
    tree_digest(&path).map_err(|e| format!("cannot pin linked {rel}: {e}"))
}

/// `{rel: digest}` for every linked path of `execution` (`None` without
/// links, so contracts without them keep their ids).
pub fn pin(repo: &Path, execution: &Value) -> Result<Option<Value>> {
    let rels = linked(execution);
    if rels.is_empty() {
        return Ok(None);
    }
    let mut pins = Map::new();
    for rel in rels {
        let state = state_of(repo, &rel)?;
        pins.insert(rel, json!(state));
    }
    Ok(Some(Value::Object(pins)))
}

/// Linked paths that differ from their pins (each with why).
pub fn drift(repo: &Path, contract: &Value) -> Vec<String> {
    let Some(pins) = contract["dependencies"].as_object() else {
        return Vec::new();
    };
    pins.iter()
        .filter_map(|(rel, pinned)| match state_of(repo, rel) {
            Ok(now) if Some(now.as_str()) == pinned.as_str() => None,
            Ok(_) => Some(format!("{rel} changed since the contract was frozen")),
            Err(e) => Some(e),
        })
        .collect()
}

/// The dependencies gate: `pass`, `not_required`, or `unverified` with the
/// drifted paths. `before` is the drift seen before the checks ran; the
/// paths are checked again now, after them.
pub fn gate(repo: &Path, contract: &Value, before: Vec<String>) -> Value {
    if contract["dependencies"].is_null() {
        return json!({"state": "not_required"});
    }
    let mut changed = before;
    changed.extend(drift(repo, contract));
    changed.sort();
    changed.dedup();
    let state = if changed.is_empty() {
        "pass"
    } else {
        "unverified"
    };
    json!({"state": state, "pinned": contract["dependencies"], "changed": changed})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linked_file_is_pinned_by_its_content() {
        let dir = std::env::temp_dir().join(format!("emi-deps-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        std::fs::write(dir.join("_version.py"), "v = '1'\n").unwrap_or_default();
        let execution = json!({"link": ["_version.py", "gone.py"]});
        let first = pin(&dir, &execution).unwrap_or_default();
        std::fs::write(dir.join("_version.py"), "v = '2'\n").unwrap_or_default();
        let second = pin(&dir, &execution).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let (first, second) = (first.unwrap_or_default(), second.unwrap_or_default());
        assert_ne!(first["_version.py"], second["_version.py"]);
        assert_eq!(first["gone.py"], ABSENT);
    }
}
