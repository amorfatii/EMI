//! Task contracts (`emi-task-contract-0.1`): everything acceptance depends
//! on, frozen and content-addressed before the agent starts.

use std::collections::BTreeSet;

use emi_ledger::pyfmt::digest;
use serde_json::{Map, Value, json};

pub type Result<T> = std::result::Result<T, String>;

pub const SCHEMA: &str = "emi-task-contract-0.1";

const TASK_ID_CHARS: &str = "._-";

pub fn valid_task_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || TASK_ID_CHARS.contains(c))
}

fn nonempty_str(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

fn string_list(v: Option<&Value>, label: &str, nonempty: bool) -> Result<Vec<String>> {
    let list: Option<Vec<String>> = v.and_then(Value::as_array).and_then(|a| {
        a.iter()
            .map(|x| {
                x.as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect()
    });
    match list {
        Some(l) if !nonempty || !l.is_empty() => Ok(l),
        None if v.is_none() && !nonempty => Ok(Vec::new()),
        _ => Err(format!(
            "{label} must be a {}string array",
            if nonempty { "nonempty " } else { "" }
        )),
    }
}

/// `validate_check_definition`: argv (never a shell), success criteria,
/// positive timeout.
fn check_definition(id: &str, def: &Value) -> Result<()> {
    let Some(d) = def.as_object() else {
        return Err(format!("check {id} must be object"));
    };
    let argv = d.get("command").and_then(Value::as_array);
    if !argv.is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_string)) {
        return Err(format!(
            "check {id} command must be nonempty string array (argv, no shell)"
        ));
    }
    if !d.get("success").is_some_and(Value::is_object) {
        return Err(format!("check {id} needs success criteria"));
    }
    let timeout = d.get("timeout_s").map_or(Some(120.0), Value::as_f64);
    if !timeout.is_some_and(|t| t.is_finite() && t > 0.0) {
        return Err(format!("check {id} timeout_s must be positive finite"));
    }
    Ok(())
}

fn envelope(v: Option<&Value>) -> Result<Value> {
    let Some(e) = v.and_then(Value::as_object) else {
        return Err("envelope must be an object".into());
    };
    let permitted = string_list(e.get("permitted_paths"), "envelope.permitted_paths", true)?;
    let kinds = match e.get("change_kinds") {
        None => vec!["add".to_owned(), "modify".into(), "delete".into()],
        Some(k) => string_list(Some(k), "envelope.change_kinds", true)?,
    };
    if kinds
        .iter()
        .any(|k| !matches!(k.as_str(), "add" | "modify" | "delete"))
    {
        return Err("envelope.change_kinds must list add/modify/delete".into());
    }
    let excluded = string_list(e.get("excluded_paths"), "envelope.excluded_paths", false)?;
    let protected = string_list(e.get("protected_paths"), "envelope.protected_paths", false)?;
    Ok(
        json!({"permitted_paths": permitted, "excluded_paths": excluded,
              "protected_paths": protected, "change_kinds": kinds}),
    )
}

fn requirements(v: Option<&Value>, checks: &Map<String, Value>) -> Result<Value> {
    let Some(reqs) = v.and_then(Value::as_array).filter(|r| !r.is_empty()) else {
        return Err("requirements must be a nonempty array".into());
    };
    let mut seen = BTreeSet::new();
    for r in reqs {
        let id = r.get("id").and_then(Value::as_str).unwrap_or("");
        if id.trim().is_empty() || !seen.insert(id.to_owned()) {
            return Err("requirement ids must be unique nonempty strings".into());
        }
        if !nonempty_str(r.get("acceptance")) {
            return Err(format!("requirement {id} needs acceptance condition"));
        }
        for bound in string_list(r.get("checks"), &format!("requirement {id} checks"), true)? {
            if !checks.contains_key(&bound) {
                return Err(format!(
                    "requirement {id:?} binds to {bound:?}, which is not a controller-owned check ID"
                ));
            }
        }
    }
    Ok(Value::Array(reqs.clone()))
}

fn budgets(v: Option<&Value>) -> Result<Value> {
    let b = v.cloned().unwrap_or_else(|| json!({}));
    let Some(map) = b.as_object() else {
        return Err("budgets must be an object".into());
    };
    for (k, x) in map {
        let ok = match k.as_str() {
            "max_retries" | "max_tokens" => x.as_u64().is_some(),
            "max_time_s" => x.as_f64().is_some_and(|t| t >= 0.0),
            _ => return Err(format!("unknown budget key: {k:?}")),
        };
        if !ok {
            return Err(format!("budget {k} must be a nonnegative number"));
        }
    }
    Ok(b)
}

/// Optional structural gate: EMI measures baseline -> candidate.
fn structural(v: Option<&Value>) -> Result<Value> {
    let Some(s) = v.filter(|v| !v.is_null()) else {
        return Ok(Value::Null);
    };
    let Some(m) = s.as_object() else {
        return Err("structural must be an object".into());
    };
    if !matches!(
        m.get("probe").and_then(Value::as_str),
        Some("python" | "js" | "rust")
    ) {
        return Err("structural.probe must be python, js or rust".into());
    }
    if !nonempty_str(m.get("source")) {
        return Err("structural.source must be a scope directory".into());
    }
    for k in ["allowance", "headroom"] {
        if m.get(k)
            .is_some_and(|x| !x.as_f64().is_some_and(|f| f >= 0.0))
        {
            return Err(format!("structural.{k} must be nonnegative"));
        }
    }
    Ok(s.clone())
}

/// The guardrail policy frozen into the contract: `false` disables the
/// guardrails gate; an object (default `{}`) is an `emi_guard::Policy`
/// whose tiers, allowed classes and budgets apply (scope stays with the
/// envelope).
fn guardrails(v: Option<&Value>) -> Result<Value> {
    match v {
        Some(Value::Bool(false)) => Ok(Value::Null),
        None | Some(Value::Bool(true)) => Ok(json!({})),
        Some(policy @ Value::Object(_)) => {
            emi_guard::Policy::from_json(policy).map_err(|e| format!("guardrails: {e}"))?;
            Ok(policy.clone())
        }
        Some(_) => Err("guardrails must be false or a policy object".into()),
    }
}

/// Most bytes of held-out files one contract may carry.
const MAX_HELD_OUT_BYTES: usize = 2_000_000;

/// A workspace-relative path that cannot escape the workspace.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// The verification policy frozen into the contract (absent: none).
///
/// - `trusted`: globs of existing files (tests, their configuration) whose
///   baseline versions are restored before the checks run; new files the
///   candidate adds still run. A candidate that changes one needs review.
/// - `held_out`: files injected into the workspace before the checks run,
///   frozen here, so they never live in the repository:
///   `{"tests/test_hidden.py": {"file": "/path/at/create"}}` or `{"content": …}`.
/// - `inventory`: a command listing test ids (one per line; `format`
///   `pytest` keeps `path::name` lines); an id the baseline has and the
///   candidate lacks rejects the candidate.
pub fn verification(v: Option<&Value>) -> Result<Option<Value>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(obj) = v.as_object() else {
        return Err("verification must be an object".into());
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "trusted" | "held_out" | "inventory"))
    {
        return Err(format!("verification: unknown key {k}"));
    }
    let trusted = string_list(obj.get("trusted"), "verification.trusted", false)?;
    let mut held_out = Map::new();
    let mut total = 0;
    for (dest, src) in obj
        .get("held_out")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if !safe_relative(dest) {
            return Err(format!(
                "verification.held_out: {dest:?} must be a relative path inside the workspace"
            ));
        }
        let content = match (
            src.get("content").and_then(Value::as_str),
            src.get("file").and_then(Value::as_str),
        ) {
            (Some(c), _) => c.to_owned(),
            (None, Some(f)) => std::fs::read_to_string(f)
                .map_err(|e| format!("verification.held_out {dest}: cannot read {f}: {e}"))?,
            _ => {
                return Err(format!(
                    "verification.held_out {dest}: needs content or file"
                ));
            }
        };
        total += content.len();
        held_out.insert(
            dest.clone(),
            json!({"content": content, "digest": digest("held-out", &json!(content))}),
        );
    }
    if total > MAX_HELD_OUT_BYTES {
        return Err(format!(
            "verification.held_out: {total} bytes exceeds {MAX_HELD_OUT_BYTES}"
        ));
    }
    let inventory = match obj.get("inventory") {
        None | Some(Value::Null) => Value::Null,
        Some(inv) => {
            let command = string_list(inv.get("command"), "verification.inventory.command", true)?;
            let format = inv.get("format").and_then(Value::as_str).unwrap_or("lines");
            if !matches!(format, "lines" | "pytest") {
                return Err("verification.inventory.format must be lines or pytest".into());
            }
            json!({"command": command, "format": format,
                   "timeout_s": inv.get("timeout_s").and_then(Value::as_f64).unwrap_or(300.0)})
        }
    };
    Ok(Some(
        json!({"trusted": trusted, "held_out": held_out, "inventory": inventory}),
    ))
}

/// The spec a controller writes; `baseline` is a ref resolved at freeze.
pub struct Frozen {
    pub contract: Value,
}

/// Validate a task spec and assemble the frozen contract (revision 1).
pub fn freeze(spec: &Value, repo_path: &str, baseline_sha: &str) -> Result<Frozen> {
    let task_id = spec.get("task_id").and_then(Value::as_str).unwrap_or("");
    if !valid_task_id(task_id) {
        return Err(format!(
            "task_id must be [A-Za-z0-9][A-Za-z0-9._-]{{0,127}}, got {task_id:?}"
        ));
    }
    if !nonempty_str(spec.get("objective")) {
        return Err("objective must be a nonempty string".into());
    }
    let Some(checks) = spec
        .get("checks")
        .and_then(Value::as_object)
        .filter(|c| !c.is_empty())
    else {
        return Err("checks must be a nonempty map of check definitions".into());
    };
    for (id, def) in checks {
        check_definition(id, def)?;
    }
    let structural = structural(spec.get("structural"))?;
    let treatment = spec
        .pointer("/warning_policy/treatment")
        .and_then(Value::as_str)
        .unwrap_or("review");
    if !matches!(treatment, "blocking" | "review" | "advisory") {
        return Err("warning_policy.treatment must be one of advisory, blocking, review".into());
    }
    let verification = verification(spec.get("verification"))?;
    let refactor = crate::refactor::spec(spec.get("refactor"))?;
    let mut core = json!({
        "schema_version": SCHEMA,
        "task_id": task_id,
        "revision": 1,
        "predecessor": null,
        "objective": spec["objective"].as_str().unwrap_or("").trim(),
        "requirements": requirements(spec.get("requirements"), checks)?,
        "exclusions": string_list(spec.get("exclusions"), "exclusions", false)?,
        "repo": {"path": repo_path},
        "baseline": {"sha": baseline_sha, "ref": spec.get("baseline").cloned().unwrap_or(Value::Null)},
        "measurement_contract_id": measurement_contract_id(&structural),
        "gate_policy_id": gate_policy_id(&structural),
        "checks": checks,
        "envelope": envelope(spec.get("envelope"))?,
        "guardrails": guardrails(spec.get("guardrails"))?,
        "execution": spec.get("execution").cloned().unwrap_or_else(|| json!({})),
        "structural": structural,
        "budgets": budgets(spec.get("budgets"))?,
        "warning_policy": {"treatment": treatment},
        "completion_policy": spec.get("completion_policy").cloned().unwrap_or_else(|| json!({"require_all_requirements_verified": true})),
        "exceptions": [],
    });
    // only when configured, so contracts without it keep their ids
    if let Some(v) = verification {
        core["verification"] = v;
    }
    if let Some(r) = refactor {
        core["refactor"] = r;
    }
    if let Some(d) = crate::deps::pin(std::path::Path::new(repo_path), &core["execution"])? {
        core["dependencies"] = d;
    }
    Ok(Frozen {
        contract: finalize(core),
    })
}

/// Stamp `contract_id` over the core.
pub fn finalize(mut core: Value) -> Value {
    if let Some(m) = core.as_object_mut() {
        m.remove("contract_id");
    }
    let id = digest("task-contract", &core);
    core["contract_id"] = json!(id);
    core
}

fn measurement_contract_id(structural: &Value) -> String {
    digest(
        "measurement-contract",
        &json!({"structural": structural, "engine": emi_scan::ENGINE}),
    )
}

fn gate_policy_id(structural: &Value) -> String {
    let allowance = structural
        .get("allowance")
        .and_then(Value::as_f64)
        .unwrap_or(10.0);
    let headroom = structural
        .get("headroom")
        .and_then(Value::as_f64)
        .unwrap_or(allowance);
    emi_ledger::gate::gate_policy_id(allowance, headroom)
}

/// Contract integrity: the stored id must match the content.
pub fn verify_integrity(contract: &Value) -> Result<()> {
    let stored = contract
        .get("contract_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recomputed = finalize(contract.clone());
    if recomputed["contract_id"].as_str() == Some(stored) && contract["schema_version"] == SCHEMA {
        Ok(())
    } else {
        Err("task contract content does not match its contract_id (tampered?)".into())
    }
}
