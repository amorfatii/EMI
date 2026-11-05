//! Task contracts (`emi-task-contract-0.1`): everything acceptance depends
//! on, frozen and content-addressed before the agent starts.

use std::collections::BTreeSet;

use emi_kernel::json::digest;
use serde_json::{Map, Value, json};

pub type Result<T> = std::result::Result<T, String>;

pub const SCHEMA: &str = "emi-task-contract-0.2";

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
/// positive timeout. Every key must be one the verifier reads.
fn check_definition(id: &str, def: &Value) -> Result<()> {
    let Some(d) = def.as_object() else {
        return Err(format!("check {id} must be object"));
    };
    known_keys("check", d, &format!("check {id}"))?;
    let argv = d.get("command").and_then(Value::as_array);
    let guards = [
        (
            argv.is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_string)),
            format!("check {id} command must be nonempty string array (argv, no shell)"),
        ),
        (
            d.get("success").is_some_and(Value::is_object),
            format!("check {id} needs success criteria"),
        ),
        (
            valid_timeout(d.get("timeout_s")),
            format!("check {id} timeout_s must be a number of seconds in (0, {MAX_TIMEOUT_S}]"),
        ),
    ];
    if let Some((_, message)) = guards.into_iter().find(|(ok, _)| !ok) {
        return Err(message);
    }
    success_criteria(id, &d["success"])?;
    evidence(id, def).map(|_| ())
}

/// The only criterion the verifier evaluates is the exit status. Any other
/// key would be read as satisfied without ever being looked at, and a
/// missing status would silently mean 0, so both are refused.
fn success_criteria(id: &str, success: &Value) -> Result<()> {
    let fields = success.as_object().cloned().unwrap_or_default();
    known_keys("check.success", &fields, &format!("check {id}: success"))?;
    if fields
        .get("exit_code")
        .and_then(Value::as_u64)
        .is_some_and(|c| c <= MAX_EXIT_STATUS)
    {
        Ok(())
    } else {
        Err(format!(
            "check {id} success.exit_code must be an integer exit status in 0..={MAX_EXIT_STATUS}"
        ))
    }
}

/// The largest status a process can exit with.
const MAX_EXIT_STATUS: u64 = 255;

/// Execution evidence a check must show besides its exit status
/// (`check.evidence`): absent unless the spec declares it, so contracts
/// frozen without it keep their meaning (a check passes on its exit code
/// alone, and the decision says so in `limits` when there is no test
/// inventory).
fn evidence(id: &str, def: &Value) -> Result<Option<String>> {
    let Some(ev) = def.get("evidence").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(fields) = ev.as_object() else {
        return Err(format!("check {id} evidence must be an object"));
    };
    known_keys("check.evidence", fields, &format!("check {id}: evidence"))?;
    match fields.get("format").and_then(Value::as_str) {
        Some("pytest") => Ok(Some("pytest".into())),
        _ => Err(format!(
            "check {id} evidence.format must be pytest (the only execution report the controller reads)"
        )),
    }
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

/// The keys each part of a task spec may hold: everything else is refused,
/// so a misspelt key (`helt_out`) cannot silently switch a protection off.
/// Each list is what the controller and the spec generators read.
const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "spec",
        &[
            "task_id",
            "repo",
            "baseline",
            "objective",
            "requirements",
            "checks",
            "execution",
            "verification",
        ],
    ),
    ("requirement", &["id", "acceptance", "checks"]),
    ("check", &["command", "success", "timeout_s", "evidence"]),
    ("check.success", &["exit_code"]),
    ("check.evidence", &["format"]),
    (
        "verification",
        &["trusted", "held_out", "inventory", "runner_config"],
    ),
    ("verification.held_out", &["content", "file"]),
    (
        "verification.inventory",
        &["command", "format", "timeout_s", "collection_errors"],
    ),
];

/// Refuse a key of `obj` that [`KNOWN_KEYS`] does not list for `part`;
/// `at` names the object in the error.
fn known_keys(part: &str, obj: &Map<String, Value>, at: &str) -> Result<()> {
    let known = KNOWN_KEYS
        .iter()
        .find(|(p, _)| *p == part)
        .map_or(&[][..], |(_, keys)| *keys);
    match obj.keys().find(|k| !known.contains(&k.as_str())) {
        Some(k) => Err(format!("{at}: unknown key {k}")),
        None => Ok(()),
    }
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
        let Some(fields) = r.as_object() else {
            return Err(format!("requirement {id} must be an object"));
        };
        known_keys("requirement", fields, &format!("requirement {id}"))?;
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
/// - `runner_config`: what added test-runner configuration does to the
///   decision, `reject` (the default) or `review`.
/// - `drift`: `{}` replays the baseline tests' inputs against the old and
///   new Python functions; a review signal only.
pub fn verification(v: Option<&Value>) -> Result<Option<Value>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(obj) = v.as_object() else {
        return Err("verification must be an object".into());
    };
    known_keys("verification", obj, "verification")?;
    let trusted = string_list(obj.get("trusted"), "verification.trusted", false)?;
    let mut out = json!({"trusted": trusted, "held_out": held_out(obj.get("held_out"))?,
                         "inventory": inventory(obj.get("inventory"))?});
    // absent unless asked for, so contracts frozen without them keep their identity
    if let Some(policy) = runner_config_policy(obj.get("runner_config"))? {
        out["runner_config"] = json!(policy);
    }
    Ok(Some(out))
}

/// The held-out files, read and frozen with their digests.
fn held_out(v: Option<&Value>) -> Result<Map<String, Value>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Map::new());
    };
    let Some(files) = v.as_object() else {
        return Err("verification.held_out must be an object of destination paths".into());
    };
    let mut held_out = Map::new();
    let mut total = 0;
    for (dest, src) in files {
        if !safe_relative(dest) {
            return Err(format!(
                "verification.held_out: {dest:?} must be a relative path inside the workspace"
            ));
        }
        let Some(fields) = src.as_object() else {
            return Err(format!(
                "verification.held_out {dest}: must be an object with content or file"
            ));
        };
        known_keys(
            "verification.held_out",
            fields,
            &format!("verification.held_out {dest}"),
        )?;
        let content = match (
            fields.get("content").and_then(Value::as_str),
            fields.get("file").and_then(Value::as_str),
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
    Ok(held_out)
}

/// The test inventory command (`null` when absent).
fn inventory(v: Option<&Value>) -> Result<Value> {
    let Some(inv) = v.filter(|v| !v.is_null()) else {
        return Ok(Value::Null);
    };
    let Some(fields) = inv.as_object() else {
        return Err("verification.inventory must be an object".into());
    };
    known_keys("verification.inventory", fields, "verification.inventory")?;
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
    let mut out = json!({"command": command, "format": format,
               "timeout_s": inv.get("timeout_s").and_then(Value::as_f64).unwrap_or(300.0)});
    // absent unless stated, so contracts frozen without it keep their id
    match inv.get("collection_errors").filter(|v| !v.is_null()) {
        None => {}
        Some(Value::String(s)) if s == "tolerate" && format == "pytest" => {
            out["collection_errors"] = json!("tolerate");
        }
        Some(_) => {
            return Err(
                "verification.inventory.collection_errors must be tolerate, with format pytest"
                    .into(),
            );
        }
    }
    Ok(out)
}

/// What added test-runner configuration does (`verification.runner_config`):
/// `reject`, the default, or `review`.
fn runner_config_policy(v: Option<&Value>) -> Result<Option<&str>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if matches!(s.as_str(), "reject" | "review") => Ok(Some(s)),
        Some(_) => Err("verification.runner_config must be reject or review".into()),
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
        "execution" => execution(some),
        "verification" => verification(some).map(|x| x.unwrap_or(Value::Null)),
        _ => Err(format!("{key} cannot be revised")),
    }
}

/// After a revision: every requirement binds to an existing check, and the
/// ids derived from the structural policy follow it.
pub fn rebind(core: &mut Value) -> Result<()> {
    let checks = core["checks"].as_object().cloned().unwrap_or_default();
    requirements(Some(&core["requirements"]), &checks)?;
    Ok(())
}

/// The spec a controller writes; `baseline` is a ref resolved at freeze.
pub struct Frozen {
    pub contract: Value,
}

/// Validate a task spec and assemble the frozen contract (revision 1).
pub fn freeze(spec: &Value, repo_path: &str, baseline_sha: &str) -> Result<Frozen> {
    let Some(fields) = spec.as_object() else {
        return Err("the task spec must be an object".into());
    };
    known_keys("spec", fields, "spec")?;
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
    let verification = verification(spec.get("verification"))?;
    let mut core = json!({
        "schema_version": SCHEMA,
        "task_id": task_id,
        "revision": 1,
        "predecessor": null,
        "objective": spec["objective"].as_str().unwrap_or("").trim(),
        "requirements": requirements(spec.get("requirements"), checks)?,
        "repo": {"path": repo_path},
        "baseline": {"sha": baseline_sha, "ref": spec.get("baseline").cloned().unwrap_or(Value::Null)},
        "checks": checks,
        "execution": execution(spec.get("execution"))?,
    });
    // only when configured, so contracts without it keep their ids
    if let Some(v) = verification {
        core["verification"] = v;
    }
    if let Some(d) = crate::engine::deps::pin(std::path::Path::new(repo_path), &core["execution"])?
    {
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
