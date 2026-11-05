//! The verification engine: a frozen contract (checks bound to
//! requirements, trusted files, the test inventory, the execution policy)
//! kept outside the agent's repository; each candidate is verified by
//! running the frozen checks in an isolated snapshot with `emi-exec`, with
//! trusted files restored, and reducing a requirement ledger to
//! `accepted`, `rejected`, `needs_review` or `incomplete`.

pub mod contract;
pub mod deps;
pub mod evidence;
pub mod runner_config;
pub mod store;
pub mod verify;

pub use store::Store;
pub use verify::{Candidate, verify};

use std::path::Path;

/// Freeze a new task from a spec (baseline ref resolved now).
pub fn create(authority: &Path, spec: &serde_json::Value) -> contract::Result<serde_json::Value> {
    let repo = spec["repo"].as_str().unwrap_or(".");
    let repo = std::fs::canonicalize(repo).map_err(|e| format!("repo {repo}: {e}"))?;
    let baseline = spec["baseline"].as_str().unwrap_or("HEAD");
    let sha = emi_tree::snapshot::resolve_ref(&repo, baseline)?;
    let task_id = spec["task_id"].as_str().unwrap_or("");
    let store = Store::open(authority, task_id, &repo)?;
    let frozen = contract::freeze(spec, &repo.to_string_lossy(), &sha)?;
    store.create(&frozen.contract)?;
    Ok(frozen.contract)
}
