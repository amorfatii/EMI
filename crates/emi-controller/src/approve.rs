//! Approving a `needs_review` candidate. A review-tier finding is a real
//! event that usually needs no intervention (a CI edit, a changed trusted
//! test), so a task must be completable without weakening its contract.
//! An approval names a person and a reason, and accepts exactly the review
//! items of one decision about one commit under one contract: a new
//! commit, a new verification or a revision needs a new approval, and no
//! item is waived for anything else.

use std::path::PathBuf;

use emi_diff::snapshot;
use emi_ledger::pyfmt::digest;
use serde_json::{Value, json};

use crate::contract::Result;
use crate::store::Store;

fn strs(v: &Value) -> impl Iterator<Item = &str> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str)
}

/// The items a person must look at before a `needs_review` decision can
/// complete, one per finding: `guard:<id>:<path>`, `trusted:<path>`,
/// `runner_config:<path>`, `untrusted_test:<path>`,
/// `shake:disagree`, `structural:warn`, `refactor:erosion_rose`,
/// `escalation:<n>`.
pub fn review_items(decision: &Value) -> Vec<String> {
    let g = &decision["gates"];
    let mut items: Vec<String> = g["guardrails"]["violations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["tier"] == "review")
        .map(|v| match v["path"].as_str() {
            Some(p) => format!("guard:{}:{p}", v["id"].as_str().unwrap_or("")),
            None => format!("guard:{}", v["id"].as_str().unwrap_or("")),
        })
        .collect();
    if g["verification"]["state"] == "needs_review" {
        items.extend(strs(&g["verification"]["restored"]).map(|p| format!("trusted:{p}")));
        items.extend(
            strs(&g["verification"]["added_runner_config"]).map(|p| format!("runner_config:{p}")),
        );
        items.extend(
            strs(&g["verification"]["untrusted_test_changes"])
                .map(|p| format!("untrusted_test:{p}")),
        );
        if g["verification"]["shake"]["state"] == "disagree" {
            items.push("shake:disagree".into());
        }
    }
    if g["structural"]["state"] == "needs_review" {
        items.push("structural:warn".into());
    }
    if g["refactor"]["state"] == "needs_review" {
        items.push("refactor:erosion_rose".into());
    }
    if g["escalation"]["state"] == "open" {
        items.extend(
            g["escalation"]["escalations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| format!("escalation:{}", e["index"])),
        );
    }
    items.sort();
    items.dedup();
    items
}

/// The identity an approval is bound to.
fn decision_id(decision: &Value) -> String {
    digest("task-decision", decision)
}

/// Why `items` does not approve exactly `wanted` (None: it does).
fn mismatch(wanted: &[String], items: &[String]) -> Option<String> {
    let unknown: Vec<&str> = items
        .iter()
        .filter(|i| !wanted.contains(i))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Some(format!(
            "not review items of this decision: {}",
            unknown.join(", ")
        ));
    }
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|w| !items.contains(w))
        .map(String::as_str)
        .collect();
    (!missing.is_empty()).then(|| {
        format!(
            "every review item must be approved by name; not approved: {}",
            missing.join(", ")
        )
    })
}

/// Record an approval of the latest `needs_review` decision about the
/// commit `reference` under the current contract.
pub fn approve(
    store: &Store,
    reference: &str,
    actor: &str,
    reason: &str,
    items: &[String],
) -> Result<Value> {
    if actor.trim().is_empty() || reason.trim().is_empty() {
        return Err("an approval needs an actor and a reason".into());
    }
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let sha = snapshot::resolve_ref(&repo, reference)?;
    let cid = contract["contract_id"].as_str().unwrap_or("");
    let Some(d) = store.latest_decision(&sha, cid)? else {
        return Err(format!(
            "{sha} has no verification under the current contract"
        ));
    };
    if d["decision"] != "needs_review" {
        return Err(format!(
            "the latest decision about {sha} is {}; only needs_review can be approved",
            d["decision"]
        ));
    }
    let wanted = review_items(&d);
    if let Some(why) = mismatch(&wanted, items) {
        return Err(why);
    }
    let record = json!({"task_id": contract["task_id"], "contract_id": cid, "candidate_id": sha,
                        "decision_id": decision_id(&d), "items": wanted,
                        "actor": actor.trim(), "reason": reason.trim()});
    store.append("approval", &record)?;
    Ok(record)
}

/// The approval covering `decision`, if one was recorded.
pub fn approval_for(store: &Store, decision: &Value) -> Result<Option<Value>> {
    let id = decision_id(decision);
    Ok(store.rows("approval")?.into_iter().rev().find(|a| {
        a["decision_id"] == id.as_str()
            && a["candidate_id"] == decision["candidate_id"]
            && a["contract_id"] == decision["contract_id"]
    }))
}
