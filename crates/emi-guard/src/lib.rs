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
//! | `prohibited.lockfile/ci/secret/vendored` | changes to those file classes unless allowed |
//! | `suppress.added` | lint, type-check or EMI suppressions added |
//! | `dup.introduced` | added product code that repeats code elsewhere in the tree |
//! | `growth.net_sloc/files` | growth beyond the task's budget |

pub mod checks;
pub mod dup;
pub mod patterns;
pub mod policy;

use std::collections::BTreeMap;
use std::path::Path;

use emi_diff::hunks::{FileChange, changes};
use serde_json::{Value, json};

pub use checks::Finding;
pub use policy::{Policy, Tier};

pub const SCHEMA: &str = "emi-guard-0.1";

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
pub fn guard(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    policy: &Policy,
) -> Result<Report, String> {
    let files = changes(repo, base, head)?;
    Ok(evaluate(repo, base, head, &files, policy))
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
    found.extend(checks::suppress(files));
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
