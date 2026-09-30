//! Reporting a contract conflict. Agents under test pressure sometimes
//! "pass" by bending the tests; a sanctioned way to say the tests or the
//! contract contradict the objective makes that unnecessary. An escalation
//! is evidence, not a decision: it charges no budget and runs no checks,
//! but while it is open no candidate under its contract is accepted
//! without a person approving it by name. A revision makes a new contract
//! id, which is what resolves it.

use std::path::PathBuf;

use emi_diff::snapshot;
use serde_json::{Value, json};

use crate::contract::Result;
use crate::store::Store;

/// Record that the current contract conflicts with the task's objective,
/// optionally naming the candidate that showed it.
pub fn escalate(store: &Store, candidate: Option<&str>, reason: &str) -> Result<Value> {
    if reason.trim().is_empty() {
        return Err("an escalation needs a reason".into());
    }
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let candidate_id = match candidate {
        Some(r) => Some(snapshot::resolve_ref(&repo, r)?),
        None => None,
    };
    let cid = contract["contract_id"].as_str().unwrap_or("");
    let mut record = json!({"task_id": contract["task_id"], "contract_id": cid,
                            "candidate_id": candidate_id, "reason": reason.trim()});
    store.append("escalation", &record)?;
    record["index"] = json!(open_escalations(store, cid)?.len());
    Ok(record)
}

/// The escalations filed under contract `contract_id`, oldest first, each
/// with its 1-based `index` among them.
pub fn open_escalations(store: &Store, contract_id: &str) -> Result<Vec<Value>> {
    Ok(store
        .rows("escalation")?
        .into_iter()
        .filter(|e| e["contract_id"] == contract_id)
        .enumerate()
        .map(|(n, mut e)| {
            e["index"] = json!(n + 1);
            e
        })
        .collect())
}

/// The escalation gate of a decision: `none`, `open` (with each
/// escalation), or `unverified` when the log could not be read, which is
/// never the same as none.
pub fn gate(store: &Store, contract_id: &str) -> Value {
    match open_escalations(store, contract_id) {
        Ok(open) if open.is_empty() => json!({"state": "none"}),
        Ok(open) => {
            let rows: Vec<Value> = open
                .iter()
                .map(|e| {
                    json!({"index": e["index"], "reason": e["reason"],
                           "candidate_id": e["candidate_id"]})
                })
                .collect();
            json!({"state": "open", "escalations": rows})
        }
        Err(e) => json!({"state": "unverified", "detail": e}),
    }
}
