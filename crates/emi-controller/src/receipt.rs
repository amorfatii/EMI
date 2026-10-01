//! The verification receipt: one recorded decision, rendered for the
//! person who has to rely on it. It says what the decision is, what was
//! verified and how, what kind of evidence carries it, what is still open,
//! how much the tests pin the change, and, as an explicit list, what was
//! not verified. It is read-only: it runs nothing and records nothing.
//!
//! [`receipt`] builds the one JSON object the CLI and MCP share;
//! [`markdown`] renders it.

mod markdown;

use std::path::{Path, PathBuf};

use emi_diff::snapshot;
use serde_json::{Map, Value, json};

use crate::contract::Result;
use crate::store::Store;
use crate::verify::{Candidate, WORKTREE};

pub use markdown::{SECTIONS, markdown};

pub const SCHEMA: &str = "emi-task-receipt-0.1";

/// Said whenever a pattern signal is among the evidence.
pub const PATTERN_CAVEAT: &str =
    "pattern findings are review hints; code written differently would pass them";

/// What a receipt says about something it has no measurement of.
fn not_measured(reason: &str) -> Value {
    json!({"state": "not_measured", "reason": reason})
}

/// The receipt for the latest decision about `candidate` under the current
/// contract; an error when there is none.
pub fn receipt(store: &Store, candidate: &Candidate) -> Result<Value> {
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let id = match candidate {
        Candidate::Worktree => WORKTREE.to_owned(),
        Candidate::Ref(r) => snapshot::resolve_ref(&repo, r)?,
    };
    let cid = contract["contract_id"].as_str().unwrap_or("");
    let Some((decision, at)) = store.latest_decision_at(&id, cid)? else {
        return Err(format!(
            "{id} has no decision under the current contract (revision {}); run emi task verify first",
            contract["revision"]
        ));
    };
    let places = places_to_look(&repo, &contract, &decision);
    Ok(build(&contract, &decision, &at, places))
}

/// The receipt of `decision` (recorded at `at`) under `contract`, with the
/// places to look read from the repository: every section always present,
/// in reading order.
pub fn build(contract: &Value, decision: &Value, at: &str, places: Value) -> Value {
    json!({
        "schema": SCHEMA,
        "task_id": contract["task_id"],
        "decision": decision_section(contract, decision, at),
        "verified": verified_section(contract, decision),
        "evidence_kind": evidence_section(decision),
        "escalations": escalation_section(decision),
        "scope": scope_section(decision),
        "pinning": pinning_section(contract, decision),
        "places_to_look": places,
        "not_verified": not_verified(contract, decision),
    })
}

/// A gate of the decision by path (`verification.shake`), if recorded.
fn gate<'a>(decision: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(&decision["gates"], |v, k| v.get(k))
        .filter(|v| v.is_object())
}

fn decision_section(contract: &Value, d: &Value, at: &str) -> Value {
    let content = match d.get("content_id") {
        Some(c) => c.clone(),
        None => not_measured("the decision ran no verification"),
    };
    json!({"decision": d["decision"], "candidate_id": d["candidate_id"], "content_id": content,
           "contract_id": d["contract_id"], "revision": contract["revision"], "at": at,
           "reasons": d["reasons"]})
}

/// Why nothing in the verification gate was measured, when it was not.
fn verification_missing(d: &Value) -> Option<&'static str> {
    match gate(d, "verification") {
        None => Some("the decision ran no verification"),
        Some(v) if v.get("restored").is_none() => Some("the contract has no verification section"),
        Some(_) => None,
    }
}

/// `pick(gate)` of the verification gate, or why it was not measured.
fn from_verification(d: &Value, pick: impl Fn(&Value) -> Value) -> Value {
    match (verification_missing(d), gate(d, "verification")) {
        (None, Some(v)) => pick(v),
        (why, _) => not_measured(why.unwrap_or("the decision ran no verification")),
    }
}

fn requirement_rows(contract: &Value, d: &Value) -> Value {
    let Some(ledger) = d["requirements"].as_array() else {
        return not_measured("no check ran for this decision");
    };
    let acceptance = |id: &Value| {
        contract["requirements"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["id"] == *id)
            .map_or(Value::Null, |r| r["acceptance"].clone())
    };
    let rows: Vec<Value> = ledger
        .iter()
        .map(|r| {
            let states = r["check_states"].as_array().cloned().unwrap_or_default();
            let checks: Vec<Value> = r["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .zip(states.iter().chain(std::iter::repeat(&Value::Null)))
                .map(|(c, s)| json!({"check": c, "status": s}))
                .collect();
            json!({"id": r["id"], "acceptance": acceptance(&r["id"]), "state": r["state"],
                   "checks": checks})
        })
        .collect();
    Value::Array(rows)
}

fn inventory(v: &Value) -> Value {
    let inv = &v["inventory"];
    match inv["state"].as_str() {
        Some("not_required") | None => not_measured("the contract has no test inventory"),
        Some("unverified") => json!({"state": "unverified", "detail": inv["error"]}),
        Some(state) => json!({"state": state, "baseline": inv["baseline"],
                              "candidate": inv["candidate"], "missing": inv["missing"]}),
    }
}

fn shake(v: &Value) -> Value {
    let s = &v["shake"];
    match s["state"].as_str() {
        Some("not_required") | None => not_measured("the contract asks for no shaken reruns"),
        Some(state) => json!({"state": state, "rounds": s["rounds"],
                              "tests_renamed": s["tests_renamed"], "changed": s["changed"]}),
    }
}

fn verified_section(contract: &Value, d: &Value) -> Value {
    json!({
        "requirements": requirement_rows(contract, d),
        "trusted_restored": from_verification(d, |v| v["restored"].clone()),
        "held_out_injected": from_verification(d, |v| v["held_out"].clone()),
        "inventory": from_verification(d, inventory),
        "shaken_reruns": from_verification(d, shake),
    })
}

fn evidence_section(d: &Value) -> Value {
    let a = &d["assurance"];
    if !a.is_object() {
        return not_measured("the decision records no assurance");
    }
    let mut out = json!({"catch_kind": a["catch_kind"], "structural": a["structural"],
                         "pattern": a["pattern"], "isolation": a["isolation"]});
    if let Some(u) = a.get("unclassified") {
        out["unclassified"] = u.clone();
    }
    if a["pattern"].as_array().is_some_and(|p| !p.is_empty()) {
        out["caveat"] = json!(PATTERN_CAVEAT);
    }
    out
}

fn escalation_section(d: &Value) -> Value {
    let Some(g) = gate(d, "escalation") else {
        return not_measured("the decision ran no verification");
    };
    match g["state"].as_str() {
        Some("open") => {
            let open: Vec<Value> = g["escalations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| json!({"index": e["index"], "reason": e["reason"]}))
                .collect();
            json!({"state": "open", "open": open})
        }
        Some("none") => json!({"state": "none", "open": []}),
        _ => json!({"state": g["state"], "detail": g["detail"]}),
    }
}

fn scope_section(d: &Value) -> Value {
    let envelope = match gate(d, "envelope") {
        Some(e) => e["violations"].clone(),
        None => not_measured("the decision ran no verification"),
    };
    let (added, policy) = match gate(d, "verification") {
        None => (
            not_measured("the decision ran no verification"),
            not_measured("the decision ran no verification"),
        ),
        Some(v) => (
            json!(
                v["added_runner_config"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            ),
            match v["runner_config"].as_str() {
                Some(p) => json!(p),
                None => json!("review, as the contract has no verification section"),
            },
        ),
    };
    json!({"envelope_violations": envelope, "runner_config_added": added,
           "runner_config_policy": policy,
           "untrusted_test_changes": from_verification(d, |v| v["untrusted_test_changes"].clone())})
}

/// The keys of `g` named in `keys` that it has.
fn subset(g: &Value, keys: &[&str]) -> Value {
    let picked: Map<String, Value> = keys
        .iter()
        .filter_map(|k| g.get(*k).map(|v| ((*k).to_owned(), v.clone())))
        .collect();
    Value::Object(picked)
}

/// Every listed drift, with the function it is in.
fn drifts(g: &Value) -> Vec<Value> {
    g["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|f| {
            f["drifts"].as_array().into_iter().flatten().map(move |x| {
                json!({"file": f["file"], "function": f["name"], "input": x["input"],
                       "old": x["old"], "new": x["new"]})
            })
        })
        .collect()
}

/// Why a review signal the decision does not carry is missing.
fn signal_missing(contract: &Value, key: &str) -> Value {
    if contract["verification"][key].is_null() {
        not_measured(&format!("the contract does not set verification.{key}"))
    } else {
        not_measured("the decision ran no verification")
    }
}

fn pinning_section(contract: &Value, d: &Value) -> Value {
    let mutation = gate(d, "verification.mutation").map_or_else(
        || signal_missing(contract, "mutation"),
        |g| {
            subset(
                g,
                &[
                    "state",
                    "candidates",
                    "run",
                    "killed",
                    "survived",
                    "timed_out",
                    "errors",
                    "survivors",
                    "reason",
                    "detail",
                    "note",
                ],
            )
        },
    );
    let drift = gate(d, "verification.drift").map_or_else(
        || signal_missing(contract, "drift"),
        |g| {
            let mut out = subset(g, &["state", "reasons", "note"]);
            out["counts"] = subset(&g["counts"], &["targets", "compared", "with_drift"]);
            out["drifts"] = json!(drifts(g));
            out
        },
    );
    json!({"mutation": mutation, "drift": drift})
}

/// Where a reviewer should look beyond the tests, read from the repository
/// between the contract's baseline and the decided commit: exported names
/// and schema files the change touches, files that usually change with the
/// ones it touched but did not, and files importing what it changed. It
/// reads git history and source; it runs nothing. A working-tree decision
/// has no commit to read, so it is not measured.
fn places_to_look(repo: &Path, contract: &Value, decision: &Value) -> Value {
    let (Some(base), Some(head)) = (
        contract["baseline"]["sha"].as_str(),
        decision["candidate_id"].as_str().filter(|c| *c != WORKTREE),
    ) else {
        let absent = not_measured(
            "the decision is of a working tree; verify a commit to see places to look",
        );
        return json!({"public_surfaces": absent, "co_change_partners": absent, "importers": absent});
    };
    let surfaces = match emi_diff::surface::surface_changes(repo, base, head) {
        Ok(s) => s.to_json(),
        Err(e) => json!({"state": "unverified", "reason": e}),
    };
    let opts = emi_diff::impact::ImpactOptions::default();
    let (co_change, importers) = match emi_diff::impact::co_change_missing(repo, base, head, &opts)
    {
        Ok(i) => {
            let j = i.to_json(&opts);
            (j["co_change"].clone(), j["dependants"].clone())
        }
        Err(e) => {
            let failed = json!({"status": "unverified", "reason": e});
            (failed.clone(), failed)
        }
    };
    json!({"public_surfaces": surfaces, "co_change_partners": co_change, "importers": importers})
}

/// States that mean a gate or signal did not verify anything.
const OPEN_STATES: &[&str] = &[
    "not_required",
    "not_run",
    "unverified",
    "nothing_to_mutate",
    "nothing_to_compare",
    "absent",
];

/// The gates and signals the Not verified list looks at: (name in the
/// receipt, path in the decision's gates).
const SIGNALS: &[(&str, &str)] = &[
    ("structural gate", "structural"),
    ("guardrails", "guardrails"),
    ("verification", "verification"),
    ("test inventory", "verification.inventory"),
    ("shaken reruns", "verification.shake"),
    ("refactor", "refactor"),
    ("dependency pins", "dependencies"),
    ("escalations", "escalation"),
    ("mutation", "verification.mutation"),
    ("drift", "verification.drift"),
];

/// What each open state of each signal means for a reader.
const MEANINGS: &[(&str, &str, &str)] = &[
    (
        "structural gate",
        "not_required",
        "the contract has no structural policy; growth in maintenance burden was not measured",
    ),
    (
        "structural gate",
        "unverified",
        "the candidate could not be measured; growth in maintenance burden is unknown",
    ),
    (
        "guardrails",
        "not_required",
        "the contract turns the guardrails off; weakened tests, suppressions and prohibited files were not looked for",
    ),
    (
        "guardrails",
        "unverified",
        "the guardrails could not run; weakened tests, suppressions and prohibited files are unknown",
    ),
    (
        "verification",
        "not_required",
        "the contract has no verification section; trusted tests were not restored and no held-out test ran",
    ),
    (
        "verification",
        "unverified",
        "part of the verification could not complete (the test inventory or a shaken rerun)",
    ),
    (
        "test inventory",
        "not_required",
        "the contract has no test inventory; each check passed on its exit code alone",
    ),
    (
        "test inventory",
        "unverified",
        "the test inventory could not be taken; tests dropped through configuration would not be seen",
    ),
    (
        "test inventory",
        "nothing_to_compare",
        "the baseline's inventory listed no test, so there was none to keep; tests the candidate added were not compared with anything",
    ),
    (
        "shaken reruns",
        "not_required",
        "not requested by the contract; code that depends on how it is tested was not looked for",
    ),
    (
        "shaken reruns",
        "unverified",
        "a shaken rerun could not complete; code that depends on how it is tested may be missed",
    ),
    (
        "refactor",
        "not_required",
        "not a refactoring task; simplification was not measured",
    ),
    (
        "refactor",
        "unverified",
        "the refactor targets could not be measured",
    ),
    (
        "dependency pins",
        "not_required",
        "the contract links no dependency directory; there was nothing to pin",
    ),
    (
        "dependency pins",
        "unverified",
        "linked dependency directories differ from their pins; the checks may have run other tools than the frozen ones",
    ),
    (
        "escalations",
        "unverified",
        "the escalation log could not be read; an open conflict report may be missed",
    ),
    (
        "mutation",
        "not_required",
        "not requested by the contract; the tests' strength on the changed lines is unknown",
    ),
    (
        "mutation",
        "not_run",
        "the unmutated checks did not all pass, so no mutant ran; the tests' strength on the changed lines is unknown",
    ),
    (
        "mutation",
        "unverified",
        "the mutants could not all be run to a verdict; the tests' strength on the changed lines is only partly known",
    ),
    (
        "mutation",
        "nothing_to_mutate",
        "the change added no mutable line inside a function; mutation says nothing about it",
    ),
    (
        "drift",
        "not_required",
        "not requested by the contract; whether the change answers differently for the tests' inputs is unknown",
    ),
    (
        "drift",
        "unverified",
        "the comparison could not complete; whether the change answers differently for the tests' inputs is unknown",
    ),
    (
        "drift",
        "nothing_to_compare",
        "no modified Python function had inputs from the tests to replay; behaviour changes there are unknown",
    ),
];

/// Always true of any decision, whatever it verified.
const NEVER_VERIFIED: &[(&str, &str)] = &[
    (
        "behaviour outside the tests' inputs",
        "the checks exercise only the inputs they contain; other inputs may behave differently",
    ),
    ("performance", "no check measures speed or resource use"),
    (
        "security properties beyond the guardrails",
        "nothing here looks for vulnerabilities; the guardrails only flag known risky edits",
    ),
];

fn meaning(what: &str, state: &str) -> &'static str {
    let generic = match state {
        "not_required" => "not required by the contract; nothing is known about it",
        "unverified" => "could not be verified; nothing is known about it",
        "absent" => {
            "not recorded in this decision (it ran no verification); nothing is known about it"
        }
        _ => "not verified",
    };
    MEANINGS
        .iter()
        .find(|(w, s, _)| *w == what && *s == state)
        .map_or(generic, |(_, _, m)| m)
}

/// The state of a signal as the Not verified list reads it: a part of a
/// verification section the contract does not have, or a review signal it
/// does not ask for, is `not_required`; anything else missing is `absent`.
fn open_state(contract: &Value, d: &Value, what: &str, path: &str) -> String {
    let requested = |key: &str| !contract["verification"][key].is_null();
    let no_section = path.starts_with("verification.")
        && gate(d, "verification").is_some_and(|v| v["state"] == "not_required");
    match gate(d, path).and_then(|g| g["state"].as_str()) {
        Some(state) => state.to_owned(),
        None if no_section => "not_required".into(),
        None if matches!(what, "mutation" | "drift") && !requested(what) => "not_required".into(),
        None => "absent".into(),
    }
}

/// The detail a gate gives for not verifying, if any.
fn detail_of(d: &Value, path: &str) -> Option<Value> {
    let g = gate(d, path)?;
    ["detail", "reason", "error"]
        .iter()
        .find_map(|k| g.get(*k).filter(|v| v.is_string()).cloned())
}

/// Every gate or signal that verified nothing, every requirement left
/// unverified, and what is never verified, each with what it means.
fn not_verified(contract: &Value, d: &Value) -> Value {
    let mut out: Vec<Value> = d["requirements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["state"] == "unverified")
        .map(|r| {
            json!({"what": format!("requirement {}", r["id"].as_str().unwrap_or("?")),
                   "state": "unverified",
                   "meaning": "its checks errored, timed out or could not be verified clean; whether it holds is unknown"})
        })
        .collect();
    for (what, path) in SIGNALS {
        let state = open_state(contract, d, what, path);
        if !OPEN_STATES.contains(&state.as_str()) {
            continue;
        }
        let mut row = json!({"what": what, "state": state, "meaning": meaning(what, &state)});
        if let Some(detail) = detail_of(d, path) {
            row["detail"] = detail;
        }
        out.push(row);
    }
    out.extend(
        NEVER_VERIFIED
            .iter()
            .map(|(what, m)| json!({"what": what, "state": "not_measured", "meaning": m})),
    );
    Value::Array(out)
}
