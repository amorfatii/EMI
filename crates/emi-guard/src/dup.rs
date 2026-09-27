//! `dup.introduced`: new product code that repeats code the repository
//! already has (or repeats itself within the change).
//!
//! A shingle is `SHINGLE` consecutive *significant* lines (blank,
//! comment-only and trivial lines like `}` or `else:` are skipped;
//! whitespace is normalized). The corpus is the tree the change produces:
//! unchanged baseline files plus the new versions of changed files, so a
//! block that was moved (its old copy deleted) is not duplication.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

use emi_diff::hunks::{FileChange, Status, file_at};
use emi_diff::snapshot::text_files_at;
use emi_scan::classify::{PathClass, classify};

use crate::checks::Finding;

/// Significant lines per shingle (EMI's whole-function clone floor is 6).
pub const SHINGLE: usize = 6;
const MAX_FILE_BYTES: usize = 2_000_000;

/// The normalized form of a significant line, or `None`.
fn significant(line: &str) -> Option<String> {
    let t = line.trim();
    if t.is_empty()
        || ["#", "//", "/*", "*", "--", "\"\"\"", "'''"]
            .iter()
            .any(|p| t.starts_with(p))
    {
        return None;
    }
    if t.chars().filter(|c| c.is_alphanumeric()).count() < 5 {
        return None;
    }
    Some(t.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// `(hash, first line)` of every shingle over `lines` (1-based line, text).
fn shingles<'a>(lines: impl Iterator<Item = (u32, &'a str)>) -> Vec<(u64, u32)> {
    let sig: Vec<(u32, String)> = lines
        .filter_map(|(n, l)| significant(l).map(|s| (n, s)))
        .collect();
    sig.windows(SHINGLE)
        .map(|w| {
            let mut h = DefaultHasher::new();
            for (_, s) in w {
                s.hash(&mut h);
            }
            (h.finish(), w[0].0)
        })
        .collect()
}

fn ext(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

/// A block of added lines: `(1-based line, text)`.
type Block<'a> = Vec<(u32, &'a str)>;

/// Runs of consecutive added lines.
fn added_blocks(fc: &FileChange) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = Vec::new();
    for (n, text) in &fc.added {
        match blocks.last_mut() {
            Some(b) if b.last().is_some_and(|(prev, _)| prev + 1 == *n) => {
                b.push((*n, text.as_str()))
            }
            _ => blocks.push(vec![(*n, text.as_str())]),
        }
    }
    blocks.retain(|b| b.iter().filter(|(_, l)| significant(l).is_some()).count() >= SHINGLE);
    blocks
}

/// Every shingle of the post-change tree for the given extensions:
/// hash -> locations `(path, line)`.
fn corpus(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    files: &[FileChange],
    exts: &HashSet<&str>,
) -> Result<HashMap<u64, Vec<(String, u32)>>, String> {
    let changed: HashSet<&str> = files
        .iter()
        .flat_map(|f| [Some(f.path.as_str()), f.old_path.as_deref()])
        .flatten()
        .collect();
    let keep = |p: &str| {
        exts.contains(ext(p)) && classify(p) == PathClass::Product && !changed.contains(p)
    };
    let mut texts = text_files_at(repo, base, keep, MAX_FILE_BYTES)?;
    for fc in files
        .iter()
        .filter(|f| f.status != Status::Deleted && !f.binary)
    {
        if exts.contains(ext(&fc.path))
            && let Some(text) = file_at(repo, head, &fc.path)
        {
            texts.push((fc.path.clone(), text));
        }
    }
    let mut index: HashMap<u64, Vec<(String, u32)>> = HashMap::new();
    for (path, text) in &texts {
        let lines = text.lines().enumerate().map(|(i, l)| (i as u32 + 1, l));
        for (h, line) in shingles(lines) {
            index.entry(h).or_default().push((path.clone(), line));
        }
    }
    Ok(index)
}

/// `dup.introduced` findings: one per added block that repeats code found
/// elsewhere in the post-change tree.
pub fn introduced(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    files: &[FileChange],
) -> Result<Vec<Finding>, String> {
    let candidates: Vec<(&FileChange, Vec<Block<'_>>)> = files
        .iter()
        .filter(|f| {
            f.status != Status::Deleted && !f.binary && classify(&f.path) == PathClass::Product
        })
        .map(|f| (f, added_blocks(f)))
        .filter(|(_, blocks)| !blocks.is_empty())
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let exts: HashSet<&str> = candidates.iter().map(|(f, _)| ext(&f.path)).collect();
    let index = corpus(repo, base, head, files, &exts)?;
    let mut out = Vec::new();
    for (fc, blocks) in candidates {
        for block in blocks {
            let hit = shingles(block.iter().copied())
                .into_iter()
                .find_map(|(h, line)| {
                    index
                        .get(&h)?
                        .iter()
                        .find(|(p, l)| !(p == &fc.path && *l == line))
                        .map(|other| (line, other.clone()))
                });
            if let Some((line, (path, other_line))) = hit {
                out.push(Finding {
                    id: "dup.introduced",
                    path: fc.path.clone(),
                    line: Some(line),
                    evidence: format!("{SHINGLE}+ significant lines repeat {path}:{other_line}"),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn significance_and_shingles() {
        assert!(significant("    }").is_none());
        assert!(significant("  # a comment").is_none());
        assert!(significant("else:").is_none());
        assert_eq!(
            significant("  x  =   compute(a, b)").as_deref(),
            Some("x = compute(a, b)")
        );
        let text = "a = one(1)\nb = two(2)\n\nc = three(3)\n# note\nd = four(4)\ne = five(5)\nf = six(6)\ng = seven(7)\n";
        let s = shingles(text.lines().enumerate().map(|(i, l)| (i as u32 + 1, l)));
        assert_eq!(s.len(), 2, "7 significant lines give 2 shingles of 6");
        assert_eq!(s[0].1, 1);
        assert_eq!(s[1].1, 2);
    }
}
