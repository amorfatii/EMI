//! What a change does to the names other code can import, and which schema
//! boundaries it touches: places to look for callers the tests of this
//! repository may not cover. A list, never a score or a gate.

use std::collections::BTreeMap;
use std::path::Path;

use emi_ir::exports::Export;
use emi_panel::Lang;
use emi_scan::exports::{Exports, exports};
use emi_scan::walk::fnmatch;
use serde_json::{Value, json};

use crate::functions::lang_name;
use crate::hunks::{self, FileChange, Status};
use crate::snapshot::resolve_ref;

/// How an exported name changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    Added,
    Removed,
    SignatureChanged,
}

impl SurfaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SurfaceKind::Added => "added",
            SurfaceKind::Removed => "removed",
            SurfaceKind::SignatureChanged => "signature_changed",
        }
    }
}

/// One exported name the change adds, removes or re-declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceChange {
    /// Path on the new side (the old side for a deleted file).
    pub file: String,
    pub name: String,
    pub lang: Lang,
    pub kind: SurfaceKind,
    pub old_signature: Option<String>,
    pub new_signature: Option<String>,
}

/// A changed path on a schema boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryTouch {
    pub file: String,
    pub label: &'static str,
}

/// The public-surface changes and boundary touches of a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Sorted by file, then name.
    pub changes: Vec<SurfaceChange>,
    /// Sorted by file, then label.
    pub boundaries: Vec<BoundaryTouch>,
    /// Changed source files whose exported names could not be read on some
    /// side: their surface is unknown, not unchanged.
    pub unparsed: Vec<String>,
    /// Changed files in no language an adapter reads exports of.
    pub unsupported: Vec<String>,
}

/// Paths that are schema boundaries, as `fnmatch` patterns (where `*` spans
/// `/`) matched against `/` + the repository-relative path, so `*/x/*`
/// also finds `x/` at the root.
const BOUNDARIES: [(&str, &str); 16] = [
    ("*/migrations/*", "migration"),
    ("*/alembic/versions/*", "migration"),
    ("*.proto", "protobuf"),
    ("*.sql", "sql"),
    ("*/openapi*.yaml", "openapi"),
    ("*/openapi*.yml", "openapi"),
    ("*/openapi*.json", "openapi"),
    ("*/swagger*.yaml", "openapi"),
    ("*/swagger*.yml", "openapi"),
    ("*/swagger*.json", "openapi"),
    ("*.schema.json", "json_schema"),
    ("*/serializers.py", "serializer"),
    ("*/models.py", "model"),
    ("*.graphql", "graphql"),
    ("*.avsc", "avro"),
    ("*.thrift", "thrift"),
];

/// Boundary labels of one path, each once, in table order.
pub fn boundary_labels(path: &str) -> Vec<&'static str> {
    let rooted = format!("/{path}");
    let mut out: Vec<&'static str> = Vec::new();
    for (pattern, label) in BOUNDARIES {
        if fnmatch(&rooted, pattern) && !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

impl SurfaceChange {
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.file,
            "name": self.name,
            "lang": lang_name(self.lang),
            "kind": self.kind.as_str(),
            "old_signature": self.old_signature,
            "new_signature": self.new_signature,
        })
    }
}

impl Surface {
    /// Render with the canonical encoder (`emi_ledger::pyfmt::dumps_pretty`).
    pub fn to_json(&self) -> Value {
        json!({
            "changes": self.changes.iter().map(SurfaceChange::to_json).collect::<Vec<_>>(),
            "boundaries": self.boundaries.iter()
                .map(|b| json!({"file": b.file, "label": b.label}))
                .collect::<Vec<_>>(),
            "unparsed": self.unparsed,
            "unsupported": self.unsupported,
        })
    }
}

/// The exported-name changes and boundary touches of `base..head`.
pub fn surface_changes(repo: &Path, base: &str, head: &str) -> Result<Surface, String> {
    // the clean view has no refs of its own, so name the commits by sha
    let (base, head) = (resolve_ref(repo, base)?, resolve_ref(repo, head)?);
    let view = emi_git::CleanView::of(repo)?;
    let files = hunks::changes(view.path(), &base, Some(&head))?;
    let mut out = Surface {
        changes: Vec::new(),
        boundaries: Vec::new(),
        unparsed: Vec::new(),
        unsupported: Vec::new(),
    };
    for file in &files {
        let mut paths = vec![file.path.as_str()];
        paths.extend(file.old_path.as_deref());
        for path in paths {
            out.boundaries.extend(
                boundary_labels(path)
                    .into_iter()
                    .map(|label| BoundaryTouch {
                        file: path.to_owned(),
                        label,
                    }),
            );
        }
        match one_file(view.path(), &base, &head, file) {
            Outcome::Changed(mut c) => out.changes.append(&mut c),
            Outcome::Unparsed => out.unparsed.push(file.path.clone()),
            Outcome::Unsupported => out.unsupported.push(file.path.clone()),
        }
    }
    out.changes
        .sort_by(|a, b| (&a.file, &a.name).cmp(&(&b.file, &b.name)));
    out.boundaries
        .sort_by(|a, b| (&a.file, a.label).cmp(&(&b.file, b.label)));
    out.boundaries.dedup();
    out.unparsed.sort();
    out.unsupported.sort();
    Ok(out)
}

enum Outcome {
    Changed(Vec<SurfaceChange>),
    Unparsed,
    Unsupported,
}

/// Exports of one side of a file; `Ok(empty)` when the side does not exist.
fn side(repo: &Path, rev: &str, path: &str, present: bool) -> Result<Vec<Export>, Exports> {
    if !present {
        return Ok(Vec::new());
    }
    let Some(text) = hunks::file_at(repo, Some(rev), path) else {
        return Err(Exports::Unparsed);
    };
    match exports(path, &text) {
        Exports::Names(names) => Ok(names),
        other => Err(other),
    }
}

fn one_file(repo: &Path, base: &str, head: &str, file: &FileChange) -> Outcome {
    let old_path = file.old_path.as_deref().unwrap_or(&file.path);
    let (Some(lang), old_lang) = (Lang::of_path(&file.path), Lang::of_path(old_path)) else {
        return Outcome::Unsupported;
    };
    if file.binary || old_lang != Some(lang) {
        return Outcome::Unparsed;
    }
    let old = side(repo, base, old_path, file.status != Status::Added);
    let new = side(repo, head, &file.path, file.status != Status::Deleted);
    match (old, new) {
        (Ok(old), Ok(new)) => Outcome::Changed(compare(&file.path, lang, &old, &new)),
        (Err(Exports::Unsupported), _) | (_, Err(Exports::Unsupported)) => Outcome::Unsupported,
        _ => Outcome::Unparsed,
    }
}

/// Exports keyed by name and the occurrence of that name, so overloads and
/// redefinitions pair up in source order.
fn keyed(exports: &[Export]) -> BTreeMap<(&str, usize), &Export> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for e in exports {
        let n = seen.entry(e.name.as_str()).or_default();
        out.insert((e.name.as_str(), *n), e);
        *n += 1;
    }
    out
}

fn compare(file: &str, lang: Lang, old: &[Export], new: &[Export]) -> Vec<SurfaceChange> {
    let (old, new) = (keyed(old), keyed(new));
    let change = |name: &str, kind, old: Option<&Export>, new: Option<&Export>| SurfaceChange {
        file: file.to_owned(),
        name: name.to_owned(),
        lang,
        kind,
        old_signature: old.and_then(|e| e.signature.clone()),
        new_signature: new.and_then(|e| e.signature.clone()),
    };
    let mut out = Vec::new();
    for (key, n) in &new {
        match old.get(key) {
            None => out.push(change(key.0, SurfaceKind::Added, None, Some(n))),
            Some(o) if o.signature != n.signature => {
                out.push(change(
                    key.0,
                    SurfaceKind::SignatureChanged,
                    Some(o),
                    Some(n),
                ));
            }
            Some(_) => {}
        }
    }
    for (key, o) in old.iter().filter(|(k, _)| !new.contains_key(*k)) {
        out.push(change(key.0, SurfaceKind::Removed, Some(o), None));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_table() {
        assert_eq!(boundary_labels("app/migrations/0002_x.py"), ["migration"]);
        assert_eq!(boundary_labels("migrations/0001.sql"), ["migration", "sql"]);
        assert_eq!(boundary_labels("api.proto"), ["protobuf"]);
        assert_eq!(boundary_labels("docs/openapi.v1.yaml"), ["openapi"]);
        assert_eq!(boundary_labels("app/models.py"), ["model"]);
        assert!(boundary_labels("app/mymodels.py").is_empty());
        assert!(boundary_labels("app/views.py").is_empty());
    }
}
