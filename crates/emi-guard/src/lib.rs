//! `emi guard`: verifiable guardrails on a change. Each guardrail is a
//! deterministic check whose correctness can be verified directly against
//! the diff (a skip marker was added, a lockfile changed, a test file was
//! deleted), not a prediction. Violations carry stable ids, and a policy
//! sets each id's tier: `block` (reject), `review` (needs a human), `off`.
//!
//! | id | detects |
//! |---|---|
//! | `scope.outside/excluded/protected/kind` | edits outside the task envelope |
//! | `tests.deleted` | a test file deleted or moved out of the test layout |
//! | `tests.fewer` / `tests.fewer_assertions` | fewer test cases / assertions in a changed test file |
//! | `tests.skip_added` | skip, ignore or xfail markers added |
//! | `tests.config_changed` | test or coverage configuration changed |
//! | `tests.unverified` | a changed test file that could not be read, so it was not counted |
//! | `prohibited.lockfile/ci/secret/vendored` | changes to those file classes unless allowed |
//! | `suppress.added` | lint, type-check or EMI suppressions added |
//! | `code.early_exit` | product code that ends the process or hooks into the test runner |
//! | `dup.introduced` | added product code that repeats code elsewhere in the tree |
//! | `gaming.test_sniff/always_equal/test_literals` | product code that passes the tests without doing what they check (Python) |
//! | `growth.net_sloc/files` | growth beyond the task's budget |

pub mod checks;
pub mod dup;
pub mod gaming;
pub mod patterns;
pub mod policy;

use std::collections::BTreeMap;
use std::path::Path;

use emi_diff::hunks::{FileChange, changes};
use serde_json::{Value, json};

pub use checks::Finding;
pub use policy::{Policy, Tier};

pub const SCHEMA: &str = "emi-guard-0.1";

/// What a guardrail's finding is evidence of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An observation of what the diff did (a path left the scope, a test
    /// was deleted, a suppression was added): any change doing it is found.
    Structural,
    /// A recognised shape of test gaming: a useful review hint, but code
    /// written differently passes it.
    Pattern,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Structural => "structural",
            Kind::Pattern => "pattern",
        }
    }
}

/// The kind of every guardrail id, by exact id or by family prefix
/// (ending in `.`). An id with no entry has no kind, never a default.
pub const KINDS: &[(&str, Kind)] = &[
    ("scope.", Kind::Structural),
    ("prohibited.", Kind::Structural),
    ("tests.", Kind::Structural),
    ("suppress.added", Kind::Structural),
    ("growth.", Kind::Structural),
    ("dup.", Kind::Structural),
    ("gaming.", Kind::Pattern),
    ("code.early_exit", Kind::Pattern),
];

/// The kind of guardrail `id` (None: unclassified).
pub fn kind(id: &str) -> Option<Kind> {
    KINDS
        .iter()
        .find(|(key, _)| {
            if key.ends_with('.') {
                id.starts_with(key)
            } else {
                id == *key
            }
        })
        .map(|(_, k)| *k)
}

/// A finding with its enforcement tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub id: &'static str,
    pub tier: Tier,
    pub path: String,
    pub line: Option<u32>,
    pub evidence: String,
}

impl Violation {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "tier": self.tier.as_str(), "path": self.path,
               "line": self.line, "evidence": self.evidence})
    }
}

/// The guard's result on one change.
#[derive(Clone, Debug)]
pub struct Report {
    pub base: String,
    pub head: Option<String>,
    pub files: usize,
    pub violations: Vec<Violation>,
}

impl Report {
    /// `block` if any violation blocks, else `review` if any needs review,
    /// else `pass`.
    pub fn decision(&self) -> &'static str {
        match self.violations.iter().map(|v| v.tier).max() {
            Some(Tier::Block) => "block",
            Some(Tier::Review) => "review",
            _ => "pass",
        }
    }

    pub fn to_json(&self) -> Value {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for v in &self.violations {
            *counts.entry(v.id).or_default() += 1;
        }
        json!({
            "schema": SCHEMA,
            "base": self.base,
            "head": self.head.clone().unwrap_or_else(|| "worktree".to_owned()),
            "decision": self.decision(),
            "files": self.files,
            "counts": counts,
            "violations": self.violations.iter().map(Violation::to_json).collect::<Vec<_>>(),
        })
    }
}

/// Run every guardrail on the change from `base` to `head` (the working
/// tree when `None`). `base` and `head` must be resolved commits.
///
/// Everything is read through a clean view of the repository's objects:
/// the repository's own configuration and attributes (a textconv driver,
/// a `-diff` attribute that makes tests "binary") could otherwise change
/// what the guard sees, and run programs. The working tree is first frozen
/// into a commit in that view, byte for byte.
pub fn guard(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    policy: &Policy,
) -> Result<Report, String> {
    let view = emi_git::CleanView::of(repo)?;
    let rev = match head {
        Some(h) => h.to_owned(),
        None => emi_diff::snapshot::commit_worktree(
            repo,
            view.path(),
            &view.path().join("emi-snapshot"),
        )?,
    };
    let mut report = guard_commits(view.path(), base, &rev, policy)?;
    report.head = head.map(str::to_owned);
    Ok(report)
}

/// [`guard`] between two commits of `objects`, a repository whose
/// configuration EMI controls (a clean view).
pub fn guard_commits(
    objects: &Path,
    base: &str,
    head: &str,
    policy: &Policy,
) -> Result<Report, String> {
    let files = changes(objects, base, Some(head))?;
    Ok(evaluate(objects, base, Some(head), &files, policy))
}

/// Guardrails over an already-computed change.
pub fn evaluate(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    files: &[FileChange],
    policy: &Policy,
) -> Report {
    let mut found = checks::scope(files, policy);
    found.extend(checks::prohibited(files, policy));
    found.extend(checks::tests(repo, base, head, files));
    found.extend(checks::suppress(repo, head, files));
    found.extend(checks::early_exit(repo, head, files));
    found.extend(gaming::gaming(repo, head, files));
    found.extend(checks::growth(files, policy));
    match dup::introduced(repo, base, head, files) {
        Ok(d) => found.extend(d),
        Err(e) => found.push(Finding {
            id: "dup.unverified",
            path: ".".into(),
            line: None,
            evidence: format!("duplication not checked: {e}"),
        }),
    }
    let mut violations: Vec<Violation> = found
        .into_iter()
        .map(|f| Violation {
            tier: policy.tier(f.id),
            id: f.id,
            path: f.path,
            line: f.line,
            evidence: f.evidence,
        })
        .collect();
    violations.sort_by(|a, b| (&a.path, a.line, a.id).cmp(&(&b.path, b.line, b.id)));
    Report {
        base: base.to_owned(),
        head: head.map(str::to_owned),
        files: files.len(),
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every guardrail id written in this crate's source: a quoted
    /// `family.name`, leaving out file names (`conftest.py`, `tox.ini`).
    fn ids_in_source() -> Vec<String> {
        const FILE_SUFFIXES: &[&str] = &["py", "ini", "cfg", "toml", "go", "rs", "json", "js"];
        let sources = [
            include_str!("lib.rs"),
            include_str!("checks.rs"),
            include_str!("dup.rs"),
            include_str!("gaming.rs"),
            include_str!("patterns.rs"),
            include_str!("policy.rs"),
        ];
        let mut ids: Vec<String> = sources
            .iter()
            .flat_map(|src| src.split('"').skip(1).step_by(2))
            .filter(|lit| {
                lit.split_once('.').is_some_and(|(family, name)| {
                    !family.is_empty()
                        && family.chars().all(|c| c.is_ascii_lowercase())
                        && !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                        && !FILE_SUFFIXES.contains(&name)
                })
            })
            .map(str::to_owned)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    #[test]
    fn every_guardrail_id_is_classified() {
        let ids = ids_in_source();
        assert!(ids.len() >= 25, "the id scan found too few: {ids:?}");
        let unclassified: Vec<&String> = ids.iter().filter(|id| kind(id).is_none()).collect();
        assert!(unclassified.is_empty(), "unclassified: {unclassified:?}");
        assert_eq!(kind("gaming.test_sniff"), Some(Kind::Pattern));
        assert_eq!(kind("code.early_exit"), Some(Kind::Pattern));
        assert_eq!(kind("tests.skip_added"), Some(Kind::Structural));
        assert_eq!(kind("code.other"), None);
    }
}
