//! Scope enumeration with the v0.5 probes' selection rules.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

/// Scope-relative file with its posix path.
#[derive(Clone, Debug)]
pub struct ScopeFile {
    pub path: PathBuf,
    pub rel: String,
}

#[derive(Clone, Debug)]
pub struct WalkError {
    pub rel: String,
    pub message: String,
}

fn excluded_part(part: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|e| e == part) || part.ends_with(".egg-info")
}

fn rel_of(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Order of CPython `PurePosixPath` comparison: component-wise.
pub fn path_order(a: &str, b: &str) -> Ordering {
    a.split('/').cmp(b.split('/'))
}

/// Candidate files for one adapter. `wants` decides by file name; the walk
/// prunes excluded directories by name and never follows symlinked
/// directories. Symlinked files are returned so callers can report them.
pub fn walk(
    root: &Path,
    excludes: &[String],
    wants: impl Fn(&str) -> bool,
) -> (Vec<ScopeFile>, Vec<String>, Vec<WalkError>) {
    let (mut files, mut pruned, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                errors.push(WalkError {
                    rel: rel_of(root, &dir),
                    message: e.to_string(),
                });
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let rel = rel_of(root, &path);
            if kind.is_dir() {
                if rel.split('/').any(|p| excluded_part(p, excludes)) {
                    pruned.push(rel);
                } else {
                    stack.push(path);
                }
            } else if wants(&name) && !rel.split('/').any(|p| excluded_part(p, excludes)) {
                files.push(ScopeFile { path, rel });
            }
        }
    }
    files.sort_by(|a, b| path_order(&a.rel, &b.rel));
    pruned.sort();
    pruned.dedup();
    (files, pruned, errors)
}

/// `fnmatch.fnmatch` on posix: `*` spans `/`, `?` one char, `[...]` sets.
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    matches_at(&n, &p)
}

fn class_end(p: &[char]) -> Option<usize> {
    let mut j = 1;
    if p.get(j) == Some(&'!') {
        j += 1;
    }
    if p.get(j) == Some(&']') {
        j += 1;
    }
    while j < p.len() && p[j] != ']' {
        j += 1;
    }
    (j < p.len()).then_some(j)
}

fn class_matches(set: &[char], c: char) -> bool {
    let (negate, set) = match set.first() {
        Some('!') => (true, &set[1..]),
        _ => (false, set),
    };
    let mut hit = false;
    let mut i = 0;
    while i < set.len() {
        if i + 2 < set.len() && set[i + 1] == '-' {
            hit |= set[i] <= c && c <= set[i + 2];
            i += 3;
        } else {
            hit |= set[i] == c;
            i += 1;
        }
    }
    hit != negate
}

fn matches_at(n: &[char], p: &[char]) -> bool {
    match p.first() {
        None => n.is_empty(),
        Some('*') => (0..=n.len()).any(|k| matches_at(&n[k..], &p[1..])),
        Some('?') => !n.is_empty() && matches_at(&n[1..], &p[1..]),
        Some('[') => match class_end(p) {
            Some(end) => {
                !n.is_empty()
                    && class_matches(&p[1..end], n[0])
                    && matches_at(&n[1..], &p[end + 1..])
            }
            None => n.first() == Some(&'[') && matches_at(&n[1..], &p[1..]),
        },
        Some(&c) => n.first() == Some(&c) && matches_at(&n[1..], &p[1..]),
    }
}

/// Excludes win; non-empty includes restrict.
pub fn globs_keep(rel: &str, exclude: &[String], include: &[String]) -> bool {
    if exclude.iter().any(|g| fnmatch(rel, g)) {
        return false;
    }
    include.is_empty() || include.iter().any(|g| fnmatch(rel, g))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnmatch_matches_python() {
        assert!(fnmatch("a/b/c.test.js", "*.test.*"));
        assert!(fnmatch("pkg/x.py", "pkg/?.py"));
        assert!(fnmatch("a1", "a[0-9]"));
        assert!(!fnmatch("ab", "a[!b]"));
        assert!(fnmatch("a[", "a["));
        assert!(!fnmatch("x.py", "*.js"));
    }

    #[test]
    fn path_order_is_componentwise() {
        assert_eq!(path_order("a/b.py", "a-c.py"), Ordering::Less);
        assert_eq!(path_order("a.py", "a/b.py"), Ordering::Greater);
    }
}
