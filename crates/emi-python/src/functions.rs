//! Which functions a change adds, modifies or removes, and where they sit on
//! each side. Built from the line-level change (`hunks::changes`) and the
//! Python adapter's function spans, so everything that needs "the functions
//! this change touched" reads one answer. Python files only; any other
//! changed file is listed as unsupported.

use std::collections::BTreeMap;
use std::path::Path;

use crate::syntax::{self, Function};
use serde_json::{Value, json};

use emi_tree::hunks::{self, FileChange, Status};
use emi_tree::snapshot::resolve_ref;

/// How a function changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Removed => "removed",
        }
    }
}

/// One changed function. Spans are 1-based and inclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFn {
    /// Path on the new side (the old side for a removed function).
    pub file: String,
    /// The name its definition gives.
    pub name: String,
    pub old: Option<(u32, u32)>,
    pub new: Option<(u32, u32)>,
    /// The line the body starts on at the candidate: lines before it are
    /// the header (decorators, signature), which runs on import.
    pub new_body: Option<u32>,
    pub kind: ChangeKind,
    /// Head-side lines added inside `new`.
    pub added_lines: Vec<u32>,
    /// Base-side lines removed inside `old`.
    pub removed_lines: Vec<u32>,
}

/// The changed functions of a change, plus the files it could not answer for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFunctions {
    /// Sorted by file, then by the new (old for removed) start line.
    pub functions: Vec<ChangedFn>,
    /// Changed files in a supported language that did not parse, or could not
    /// be read, on some side: their functions are unknown, not unchanged.
    pub unparsed: Vec<String>,
    /// Changed files in no supported language.
    pub unsupported: Vec<String>,
}

fn span(s: Option<(u32, u32)>) -> Value {
    s.map_or(Value::Null, |(a, b)| json!([a, b]))
}

impl ChangedFn {
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.file,
            "name": self.name,
            "kind": self.kind.as_str(),
            "old": span(self.old),
            "new": span(self.new),
            "added_lines": self.added_lines,
            "removed_lines": self.removed_lines,
        })
    }
}

impl ChangedFunctions {
    /// The JSON form, for the canonical encoder.
    pub fn to_json(&self) -> Value {
        json!({
            "functions": self.functions.iter().map(ChangedFn::to_json).collect::<Vec<_>>(),
            "unparsed": self.unparsed,
            "unsupported": self.unsupported,
        })
    }
}

/// Added lines per file, read between two commits through a clean view.
pub fn added_lines(
    repo: &Path,
    base: &str,
    head: &str,
) -> Result<BTreeMap<String, Vec<u32>>, String> {
    let view = emi_git::CleanView::of(repo)?;
    let files = hunks::changes(view.path(), base, Some(head))?;
    Ok(files
        .into_iter()
        .filter(|f| !f.added.is_empty())
        .map(|f| (f.path, f.added.into_iter().map(|(l, _)| l).collect()))
        .collect())
}

/// The functions `base..head` adds, modifies or removes.
pub fn changed_functions(repo: &Path, base: &str, head: &str) -> Result<ChangedFunctions, String> {
    // the clean view has no refs of its own, so name the commits by sha
    let (base, head) = (resolve_ref(repo, base)?, resolve_ref(repo, head)?);
    let (base, head) = (base.as_str(), head.as_str());
    let view = emi_git::CleanView::of(repo)?;
    let files = hunks::changes(view.path(), base, Some(head))?;
    let mut out = ChangedFunctions {
        functions: Vec::new(),
        unparsed: Vec::new(),
        unsupported: Vec::new(),
    };
    for file in &files {
        match one_file(view.path(), base, head, file) {
            FileOutcome::Changed(mut fns) => out.functions.append(&mut fns),
            FileOutcome::Unparsed => out.unparsed.push(file.path.clone()),
            FileOutcome::Unsupported => out.unsupported.push(file.path.clone()),
        }
    }
    out.functions.sort_by(|a, b| {
        let key = |f: &ChangedFn| f.new.or(f.old).map_or(0, |s| s.0);
        (&a.file, key(a)).cmp(&(&b.file, key(b)))
    });
    out.unparsed.sort();
    out.unsupported.sort();
    Ok(out)
}

enum FileOutcome {
    Changed(Vec<ChangedFn>),
    Unparsed,
    Unsupported,
}

/// A side of a file: its functions, or why there are none.
enum Side {
    Absent,
    Parsed(Vec<Function>),
    Unreadable,
}

fn read_side(repo: &Path, rev: &str, path: &str) -> Side {
    let Some(text) = hunks::file_at(repo, Some(rev), path) else {
        return Side::Unreadable;
    };
    syntax::functions(text.as_bytes()).map_or(Side::Unreadable, Side::Parsed)
}

fn is_python(path: &str) -> bool {
    path.ends_with(".py")
}

fn one_file(repo: &Path, base: &str, head: &str, file: &FileChange) -> FileOutcome {
    let old_path = file.old_path.as_deref().unwrap_or(&file.path);
    if !is_python(&file.path) {
        return FileOutcome::Unsupported;
    }
    // a rename from another language cannot be matched function by function
    if file.binary || !is_python(old_path) {
        return FileOutcome::Unparsed;
    }
    let old = match file.status {
        Status::Added => Side::Absent,
        _ => read_side(repo, base, old_path),
    };
    let new = match file.status {
        Status::Deleted => Side::Absent,
        _ => read_side(repo, head, &file.path),
    };
    match (old, new) {
        (Side::Unreadable, _) | (_, Side::Unreadable) => FileOutcome::Unparsed,
        (old, new) => {
            let fns = |s: Side| match s {
                Side::Parsed(f) => f,
                _ => Vec::new(),
            };
            FileOutcome::Changed(classify(file, &fns(old), &fns(new)))
        }
    }
}

fn lines_in(lines: &[(u32, String)], (start, end): (u32, u32)) -> Vec<u32> {
    lines
        .iter()
        .map(|(l, _)| *l)
        .filter(|l| (start..=end).contains(l))
        .collect()
}

/// Functions keyed by enclosing scope, name, and the occurrence of that
/// `(scope, name)` in the file, so overloads pair up in source order while
/// methods of different classes stay separate.
fn keyed(fns: &[Function]) -> BTreeMap<(&str, &str, usize), &Function> {
    let mut seen: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for f in fns {
        let pair = (f.scope.as_str(), f.name.as_str());
        let n = seen.entry(pair).or_default();
        out.insert((pair.0, pair.1, *n), f);
        *n += 1;
    }
    out
}

fn classify(file: &FileChange, old: &[Function], new: &[Function]) -> Vec<ChangedFn> {
    let (old, new) = (keyed(old), keyed(new));
    let mut out = Vec::new();
    for (key, n) in &new {
        let new_span = (n.start, n.end);
        let added = lines_in(&file.added, new_span);
        let (old_span, removed) = match old.get(key) {
            Some(o) => (
                (Some((o.start, o.end))),
                lines_in(&file.removed, (o.start, o.end)),
            ),
            None => (None, Vec::new()),
        };
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        out.push(ChangedFn {
            file: file.path.clone(),
            name: key.1.to_owned(),
            old: old_span,
            new: Some(new_span),
            new_body: Some(n.body_start),
            kind: if old_span.is_some() {
                ChangeKind::Modified
            } else {
                ChangeKind::Added
            },
            added_lines: added,
            removed_lines: removed,
        });
    }
    for (key, o) in old.iter().filter(|(k, _)| !new.contains_key(*k)) {
        let removed = lines_in(&file.removed, (o.start, o.end));
        if removed.is_empty() {
            continue;
        }
        out.push(ChangedFn {
            file: file.old_path.clone().unwrap_or_else(|| file.path.clone()),
            name: key.1.to_owned(),
            old: Some((o.start, o.end)),
            new: None,
            new_body: None,
            kind: ChangeKind::Removed,
            added_lines: Vec::new(),
            removed_lines: removed,
        });
    }
    out
}
