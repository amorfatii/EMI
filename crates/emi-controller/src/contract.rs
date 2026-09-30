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
    if !valid_timeout(d.get("timeout_s")) {
        return Err(format!(
            "check {id} timeout_s must be a number of seconds in (0, {MAX_TIMEOUT_S}]"
        ));
    }
    Ok(())
}

/// Longest run a contract may allow (one week): anything longer is a
/// mistake, and far larger values cannot be represented as a duration.
pub const MAX_TIMEOUT_S: f64 = 604_800.0;

/// Absent, or a number of seconds in `(0, MAX_TIMEOUT_S]`.
fn valid_timeout(v: Option<&Value>) -> bool {
    v.is_none_or(|t| t.as_f64().is_some_and(|t| t > 0.0 && t <= MAX_TIMEOUT_S))
}

/// The execution policy: sandbox, network, linked dependency
/// directories, extra environment. Unknown keys and values are refused
/// rather than read as the default.
pub fn execution(v: Option<&Value>) -> Result<Value> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(json!({}));
    };
    let Some(obj) = v.as_object() else {
        return Err("execution must be an object".into());
    };
    for (k, x) in obj {
        let ok = match k.as_str() {
            "isolation" => matches!(x.as_str(), Some("best" | "bwrap" | "none")),
            "network" => x.is_boolean(),
            "link" => string_list(Some(x), "execution.link", false)
                .is_ok_and(|l| l.iter().all(|p| safe_relative(p))),
            "env" => x
                .as_object()
                .is_some_and(|m| m.values().all(Value::is_string)),
            _ => return Err(format!("execution: unknown key {k}")),
        };
        if !ok {
            return Err(format!(
                "execution.{k} is invalid (isolation: best, bwrap or none; network: a boolean; \
                 link: relative paths; env: an object of strings)"
            ));
        }
    }
    Ok(v.clone())
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
        Some("python" | "js" | "rust" | "go")
    ) {
        return Err("structural.probe must be python, js, rust or go".into());
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
/// - `shake`: `{"rounds": 1..=3}` reruns every passing check with the
///   trusted tests renamed, at another path and hash seed; a check that
///   stops passing needs review.
pub fn verification(v: Option<&Value>) -> Result<Option<Value>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(obj) = v.as_object() else {
        return Err("verification must be an object".into());
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "trusted" | "held_out" | "inventory" | "shake"))
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
            if !valid_timeout(inv.get("timeout_s")) {
                return Err(format!(
                    "verification.inventory.timeout_s must be a number of seconds in (0, {MAX_TIMEOUT_S}]"
                ));
            }
            if !matches!(format, "lines" | "pytest") {
                return Err("verification.inventory.format must be lines or pytest".into());
            }
            json!({"command": command, "format": format,
                   "timeout_s": inv.get("timeout_s").and_then(Value::as_f64).unwrap_or(300.0)})
        }
    };
    let mut out = json!({"trusted": trusted, "held_out": held_out, "inventory": inventory});
    // absent unless asked for, so contracts frozen without it keep their identity
    if let Some(rounds) = shake(obj.get("shake"))? {
        out["shake"] = json!({"rounds": rounds});
    }
    Ok(Some(out))
}

/// The number of shaken rerun rounds (`verification.shake.rounds`).
fn shake(v: Option<&Value>) -> Result<Option<u64>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    match v.get("rounds").and_then(Value::as_u64) {
        Some(n @ 1..=3) if v.as_object().is_some_and(|o| o.len() == 1) => Ok(Some(n)),
        _ => Err("verification.shake must be {\"rounds\": 1, 2 or 3}".into()),
    }
}

fn warning_policy(v: Option<&Value>) -> Result<Value> {
    let treatment = v
        .map_or(Some("review"), |w| {
            w.get("treatment").and_then(Value::as_str)
        })
        .unwrap_or("");
    if matches!(treatment, "blocking" | "review" | "advisory") {
        Ok(json!({"treatment": treatment}))
    } else {
        Err("warning_policy.treatment must be one of advisory, blocking, review".into())
    }
}

fn completion_policy(v: Option<&Value>) -> Result<Value> {
    let Some(v) = v else {
        return Ok(json!({"require_all_requirements_verified": true}));
    };
    let valid = v.as_object().is_some_and(|m| {
        m.iter()
            .all(|(k, x)| k == "require_all_requirements_verified" && x.is_boolean())
    });
    if valid {
        Ok(v.clone())
    } else {
        Err("completion_policy may only set require_all_requirements_verified (a boolean)".into())
    }
}

/// One section of a revision, validated like at freeze (the value stored
/// is the validated one). Identity, baseline and derived fields cannot be
/// revised. Requirement bindings are checked by [`rebind`] once every
/// section of the revision is in place.
pub fn revised(key: &str, v: &Value) -> Result<Value> {
    let some = Some(v);
    match key {
        "objective" if nonempty_str(some) => Ok(json!(v.as_str().unwrap_or("").trim())),
        "objective" => Err("objective must be a nonempty string".into()),
        "requirements" if v.as_array().is_some_and(|r| !r.is_empty()) => Ok(v.clone()),
        "requirements" => Err("requirements must be a nonempty array".into()),
        "checks" => {
            let checks = v
                .as_object()
                .filter(|c| !c.is_empty())
                .ok_or("checks must be a nonempty map of check definitions")?;
            for (id, def) in checks {
                check_definition(id, def)?;
            }
            Ok(v.clone())
        }
        "envelope" => envelope(some),
        "guardrails" => guardrails(some),
        "structural" => structural(some),
        "budgets" => budgets(some),
        "warning_policy" => warning_policy(some),
        "completion_policy" => completion_policy(some),
        "execution" => execution(some),
        "exclusions" => string_list(some, "exclusions", false).map(|l| json!(l)),
        "verification" => verification(some).map(|x| x.unwrap_or(Value::Null)),
        "refactor" => crate::refactor::spec(some).map(|x| x.unwrap_or(Value::Null)),
        _ => Err(format!("{key} cannot be revised")),
    }
}

/// After a revision: every requirement binds to an existing check, and the
/// ids derived from the structural policy follow it.
pub fn rebind(core: &mut Value) -> Result<()> {
    let checks = core["checks"].as_object().cloned().unwrap_or_default();
    requirements(Some(&core["requirements"]), &checks)?;
    let structural = core["structural"].clone();
    core["measurement_contract_id"] = json!(measurement_contract_id(&structural));
    core["gate_policy_id"] = json!(gate_policy_id(&structural));
    Ok(())
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
    let warning_policy = warning_policy(spec.get("warning_policy"))?;
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
        "execution": execution(spec.get("execution"))?,
        "structural": structural,
        "budgets": budgets(spec.get("budgets"))?,
        "warning_policy": warning_policy,
        "completion_policy": completion_policy(spec.get("completion_policy"))?,
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
