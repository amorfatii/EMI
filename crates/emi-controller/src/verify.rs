//! Verify one candidate against the frozen contract and reduce everything
//! to a decision: `accepted`, `rejected`, `needs_review` or `incomplete`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use emi_diff::snapshot;
use emi_exec::{Isolation, Spec, Status};
use emi_ledger::pyfmt::digest;
use serde_json::{Value, json};

use crate::contract::Result;
use crate::store::Store;

/// What is being verified.
#[derive(Clone, Debug)]
pub enum Candidate {
    Ref(String),
    /// Uncommitted work (never completable, only verifiable).
    Worktree,
}

pub const WORKTREE: &str = "worktree";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("emi-verify-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `(path, kind)` of every change from the baseline, renames split into
/// delete + add.
fn changes(repo: &Path, baseline: &str, candidate: &str) -> Result<Vec<(String, &'static str)>> {
    let mut args = vec!["diff", "--name-status", "-z", "-M", baseline];
    if candidate != WORKTREE {
        args.push(candidate);
    }
    let raw = git(repo, &args)?;
    let tokens: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let status = tokens[i].clone();
        let letter = status.chars().next().unwrap_or(' ');
        match letter {
            'R' | 'C' if i + 2 < tokens.len() => {
                if letter == 'R' {
                    out.push((tokens[i + 1].clone(), "delete"));
                }
                out.push((tokens[i + 2].clone(), "add"));
                i += 3;
            }
            'A' | 'M' | 'D' | 'T' if i + 1 < tokens.len() => {
                let kind = match letter {
                    'A' => "add",
                    'D' => "delete",
                    _ => "modify",
                };
                out.push((tokens[i + 1].clone(), kind));
                i += 2;
            }
            _ => i += 1,
        }
    }
    if candidate == WORKTREE {
        let untracked = git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for p in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            out.push((String::from_utf8_lossy(p).into_owned(), "add"));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn globs(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

/// The envelope gate: violations of the frozen scope.
fn envelope_gate(contract: &Value, changed: &[(String, &'static str)]) -> Value {
    let env = &contract["envelope"];
    let matches = |path: &str, key: &str| {
        globs(&env[key])
            .iter()
            .any(|g| emi_scan::walk::fnmatch(path, g))
    };
    let kinds = globs(&env["change_kinds"]);
    let mut violations = Vec::new();
    for (path, kind) in changed {
        let reason = if matches(path, "protected_paths") {
            Some("protected check/runtime configuration path; needs a controller revision")
        } else if matches(path, "excluded_paths") {
            Some("explicitly excluded from this task")
        } else if !kinds.iter().any(|k| k == kind) {
            Some("change kind not permitted by the envelope")
        } else if !matches(path, "permitted_paths") {
            Some("outside the permitted task scope")
        } else {
            None
        };
        if let Some(reason) = reason {
            violations.push(json!({"path": path, "change_kind": kind, "reason": reason}));
        }
    }
    json!({"state": if violations.is_empty() { "pass" } else { "fail" }, "changed": changed.len(), "violations": violations})
}

/// The guardrails gate: verifiable checks on the change (weakened tests,
/// prohibited files, new suppressions, growth). Scope is the envelope's.
fn guardrails_gate(contract: &Value, repo: &Path, baseline: &str, candidate: &str) -> Value {
    let spec = &contract["guardrails"];
    if spec.is_null() {
        return json!({"state": "not_required"});
    }
    let mut policy = match emi_guard::Policy::from_json(spec) {
        Ok(p) => p,
        Err(e) => return json!({"state": "unverified", "error": e}),
    };
    policy.tiers.insert("scope".into(), emi_guard::Tier::Off);
    let head = (candidate != WORKTREE).then_some(candidate);
    match emi_guard::guard(repo, baseline, head, &policy) {
        Ok(report) => {
            let state = match report.decision() {
                "block" => "fail",
                "review" => "needs_review",
                _ => "pass",
            };
            let active: Vec<Value> = report
                .violations
                .iter()
                .filter(|v| v.tier != emi_guard::Tier::Off)
                .map(emi_guard::Violation::to_json)
                .collect();
            json!({"state": state, "schema": emi_guard::SCHEMA, "violations": active})
        }
        Err(e) => json!({"state": "unverified", "error": e}),
    }
}

/// `id` list of the guardrail violations with the given tier.
fn guard_ids(gate: &Value, tier: &str) -> String {
    let mut ids: Vec<&str> = gate["violations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["tier"] == tier)
        .filter_map(|v| v["id"].as_str())
        .collect();
    ids.dedup();
    ids.join(", ")
}

fn materialize(repo: &Path, candidate: &str, dest: &Path, contract: &Value) -> Result<()> {
    if candidate == WORKTREE {
        snapshot::materialize_worktree(repo, dest)?;
    } else {
        snapshot::materialize(repo, candidate, dest)?;
    }
    // Ignored dependency directories (node_modules, .venv) the checks need.
    for rel in globs(&contract["execution"]["link"]) {
        let (src, dst) = (repo.join(&rel), dest.join(&rel));
        if src.exists() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&src, &dst)
                .map_err(|e| format!("cannot link {rel}: {e}"))?;
        }
    }
    Ok(())
}

/// Restore the baseline version of every trusted file the candidate
/// modified or deleted; files it added stay. Returns the restored paths.
fn restore_trusted(
    repo: &Path,
    baseline: &str,
    workspace: &Path,
    trusted: &[String],
    changed: &[(String, &'static str)],
) -> Result<Vec<String>> {
    let mut restored = Vec::new();
    for (path, kind) in changed {
        let is_trusted = trusted.iter().any(|g| emi_scan::walk::fnmatch(path, g));
        if !is_trusted || !matches!(*kind, "modify" | "delete") {
            continue;
        }
        let bytes = git(repo, &["show", &format!("{baseline}:{path}")])?;
        let dest = workspace.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("restore {path}: {e}"))?;
        }
        std::fs::write(&dest, bytes).map_err(|e| format!("restore {path}: {e}"))?;
        restored.push(path.clone());
    }
    Ok(restored)
}

/// Write the contract's held-out files into the workspace (they win over
/// any file of the same name the candidate wrote).
fn inject_held_out(workspace: &Path, held_out: &Value) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (dest, file) in held_out.as_object().into_iter().flatten() {
        let path = workspace.join(dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("held-out {dest}: {e}"))?;
        }
        std::fs::write(&path, file["content"].as_str().unwrap_or(""))
            .map_err(|e| format!("held-out {dest}: {e}"))?;
        written.push(dest.clone());
    }
    Ok(written)
}

/// Test ids the inventory command lists in `workspace`, or why it could not.
fn inventory_ids(
    inv: &Value,
    workspace: &Path,
    execution: &Value,
) -> std::result::Result<std::collections::BTreeSet<String>, String> {
    let argv: Vec<&str> = inv["command"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut spec = Spec::new(&argv, workspace);
    spec.timeout = Duration::from_secs_f64(inv["timeout_s"].as_f64().unwrap_or(300.0));
    spec.isolation = isolation(execution);
    if let Some(env) = execution["env"].as_object() {
        spec.env.extend(
            env.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))),
        );
    }
    let o = emi_exec::run(&spec);
    if o.status != Status::Exited || o.exit_code != Some(0) {
        return Err(format!(
            "inventory command did not succeed ({:?}, exit {:?}): {}",
            o.status,
            o.exit_code,
            tail(&o.stderr, 400)
        ));
    }
    let pytest = inv["format"] == "pytest";
    Ok(o.stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && (!pytest || l.contains("::")))
        .map(str::to_owned)
        .collect())
}

/// The verification gate: trusted files restored, held-out files injected
/// (both before the checks run), and the test inventory compared.
fn verification_gate(
    contract: &Value,
    repo: &Path,
    baseline: &str,
    restored: Vec<String>,
    held_out: Vec<String>,
    candidate_ids: Option<std::result::Result<std::collections::BTreeSet<String>, String>>,
    scratch: &Scratch,
) -> Value {
    let spec = &contract["verification"];
    if spec.is_null() {
        return json!({"state": "not_required"});
    }
    let mut gate =
        json!({"restored": restored, "held_out": held_out, "inventory": {"state": "not_required"}});
    let mut state = if restored.is_empty() {
        "pass"
    } else {
        "needs_review"
    };
    if let Some(candidate_ids) = candidate_ids {
        let base_ws = scratch.0.join("baseline");
        let base_ids = materialize(repo, baseline, &base_ws, contract)
            .and_then(|()| inventory_ids(&spec["inventory"], &base_ws, &contract["execution"]));
        match (base_ids, candidate_ids) {
            (Ok(base), Ok(cand)) => {
                let missing: Vec<&String> = base.difference(&cand).collect();
                if !missing.is_empty() {
                    state = "fail";
                }
                gate["inventory"] = json!({"state": if missing.is_empty() { "pass" } else { "fail" },
                    "baseline": base.len(), "candidate": cand.len(), "missing": missing});
            }
            (Err(e), _) | (_, Err(e)) => {
                if state != "fail" {
                    state = "unverified";
                }
                gate["inventory"] = json!({"state": "unverified", "error": e});
            }
        }
    }
    gate["state"] = json!(state);
    gate
}

fn isolation(execution: &Value) -> Isolation {
    let network = execution["network"].as_bool().unwrap_or(false);
    match execution["isolation"].as_str().unwrap_or("best") {
        "none" => Isolation::None,
        "bwrap" => Isolation::Bwrap { network },
        _ => Isolation::Best { network },
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// Run one frozen check; `(status, attribution, run record)`.
fn run_check(
    id: &str,
    def: &Value,
    workspace: &Path,
    execution: &Value,
) -> (String, String, Value) {
    let argv: Vec<&str> = def["command"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut spec = Spec::new(&argv, workspace);
    spec.timeout = Duration::from_secs_f64(def["timeout_s"].as_f64().unwrap_or(120.0));
    spec.isolation = isolation(execution);
    if let Some(env) = execution["env"].as_object() {
        spec.env.extend(
            env.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))),
        );
    }
    let expected = def["success"]["exit_code"].as_i64().unwrap_or(0);
    let o = emi_exec::run(&spec);
    let (status, attribution) = match o.status {
        Status::NotLaunched => ("error", "environment"),
        Status::TimedOut => ("timeout", "unresolved"),
        Status::Exited if !o.receipt.verified_clean => ("error", "environment"),
        Status::Exited if o.exit_code.map(i64::from) == Some(expected) => ("passed", "candidate"),
        Status::Exited => ("failed", "candidate"),
    };
    let record = json!({
        "check_id": id, "check_definition_id": digest("check-definition", def),
        "status": status, "attribution": attribution, "exit_code": o.exit_code, "signal": o.signal,
        "duration_ms": o.duration_ms, "launch_error": o.launch_error,
        "stdout_tail": tail(&o.stdout, 2000), "stderr_tail": tail(&o.stderr, 2000),
        "receipt": o.receipt,
    });
    (status.to_owned(), attribution.to_owned(), record)
}

/// The structural gate: EMI measures baseline -> candidate itself.
fn structural_gate(contract: &Value, repo: &Path, baseline: &str, candidate: &str) -> Value {
    let s = &contract["structural"];
    if s.is_null() {
        return json!({"state": "not_required"});
    }
    let probe = emi_scan::Probe::from_name(s["probe"].as_str().unwrap_or("python"))
        .unwrap_or(emi_scan::Probe::Python);
    let mut req = emi_diff::Request::new(
        repo,
        s["source"].as_str().unwrap_or("src"),
        baseline,
        candidate,
        probe,
    );
    req.worktree = candidate == WORKTREE;
    req.tolerate_syntax_errors = true;
    req.profile = if s["profile"] == "parity" {
        emi_scan::Profile::Parity
    } else {
        emi_scan::Profile::Precise
    };
    let allowance = s["allowance"].as_f64().unwrap_or(10.0);
    let headroom = s["headroom"].as_f64();
    let verdict = emi_diff::build_change(&req)
        .and_then(|c| emi_ledger::review(&c))
        .and_then(|r| {
            let policy = emi_ledger::gate::Policy {
                compact: true,
                allowance,
                headroom,
                ..emi_ledger::gate::Policy::default()
            };
            emi_ledger::gate::apply(r, &policy).map(|g| g.output)
        });
    let treatment = contract["warning_policy"]["treatment"]
        .as_str()
        .unwrap_or("review");
    match verdict {
        Err(e) => json!({"state": "unverified", "detail": e}),
        Ok(v) => {
            let state = match (v["decision"].as_str(), treatment) {
                (Some("block"), _) | (Some("warn"), "blocking") => "fail",
                (Some("warn"), "review") => "needs_review",
                (Some("pass" | "warn"), _) => "pass",
                _ => "unverified",
            };
            json!({"state": state, "verdict": v})
        }
    }
}

/// Requirement ledger: verified when every bound check passed.
fn ledger(contract: &Value, statuses: &BTreeMap<String, String>) -> Vec<Value> {
    contract["requirements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let bound = globs(&r["checks"]);
            let states: Vec<&str> = bound
                .iter()
                .map(|c| statuses.get(c).map_or("not_run", String::as_str))
                .collect();
            let state = if states.iter().all(|s| *s == "passed") {
                "verified"
            } else if states.contains(&"failed") {
                "failed"
            } else {
                "unverified"
            };
            json!({"id": r["id"], "state": state, "checks": bound, "check_states": states})
        })
        .collect()
}

/// The gates a decision reduces over.
struct Gates<'a> {
    envelope: &'a Value,
    structural: &'a Value,
    guardrails: &'a Value,
    verification: &'a Value,
}

/// Ids of the requirements in `state`.
fn ids_in(ledger: &[Value], state: &str) -> Vec<String> {
    ledger
        .iter()
        .filter(|r| r["state"] == state)
        .map(|r| r["id"].as_str().unwrap_or("").to_owned())
        .collect()
}

/// Why the candidate is rejected (empty: it is not).
fn rejections(ledger: &[Value], g: &Gates) -> Vec<String> {
    let mut reasons = Vec::new();
    let failed = ids_in(ledger, "failed");
    if !failed.is_empty() {
        reasons.push(format!(
            "requirements failed their checks: {}",
            failed.join(", ")
        ));
    }
    if g.envelope["state"] == "fail" {
        reasons.push(format!(
            "{} change(s) outside the task envelope",
            g.envelope["violations"].as_array().map_or(0, Vec::len)
        ));
    }
    if g.guardrails["state"] == "fail" {
        reasons.push(format!(
            "blocking guardrail violations: {}",
            guard_ids(g.guardrails, "block")
        ));
    }
    if g.verification["state"] == "fail" {
        reasons.push(format!(
            "{} test(s) of the baseline are missing from the candidate's inventory",
            g.verification["inventory"]["missing"]
                .as_array()
                .map_or(0, Vec::len)
        ));
    }
    if g.structural["state"] == "fail" {
        reasons.push(format!(
            "structural gate: {}",
            g.structural["verdict"]["reasons"][0]
                .as_str()
                .unwrap_or("block")
        ));
    }
    reasons
}

/// What could not be verified (empty: everything was).
fn gaps(ledger: &[Value], g: &Gates, require_all: bool) -> Vec<String> {
    let mut reasons = Vec::new();
    let unverified = ids_in(ledger, "unverified");
    if require_all && !unverified.is_empty() {
        reasons.push(format!(
            "requirements not verified (errors, timeouts or unverified cleanup): {}",
            unverified.join(", ")
        ));
    }
    if g.structural["state"] == "unverified" {
        reasons.push("structural gate could not measure the candidate".into());
    }
    if g.guardrails["state"] == "unverified" {
        reasons.push("guardrails could not be checked".into());
    }
    if g.verification["state"] == "unverified" {
        reasons.push("the test inventory could not be taken".into());
    }
    reasons
}

/// What a person should look at before accepting (empty: nothing).
fn reviews(g: &Gates) -> Vec<String> {
    let mut reasons = Vec::new();
    if g.structural["state"] == "needs_review" {
        reasons.push("structural gate warned; the task's warning policy asks for review".into());
    }
    if g.guardrails["state"] == "needs_review" {
        reasons.push(format!(
            "guardrails need review: {}",
            guard_ids(g.guardrails, "review")
        ));
    }
    if g.verification["state"] == "needs_review" {
        reasons.push(format!(
            "trusted files changed (verified against their baseline versions): {}",
            globs(&g.verification["restored"]).join(", ")
        ));
    }
    reasons
}

fn reduce(ledger: &[Value], gates: &Gates, require_all: bool) -> (&'static str, Vec<String>) {
    let rejected = rejections(ledger, gates);
    if !rejected.is_empty() {
        return ("rejected", rejected);
    }
    let missing = gaps(ledger, gates, require_all);
    if !missing.is_empty() {
        return ("incomplete", missing);
    }
    let review = reviews(gates);
    if !review.is_empty() {
        return ("needs_review", review);
    }
    (
        "accepted",
        vec!["every requirement verified by its frozen checks; all gates pass".into()],
    )
}

fn budget_exhausted(contract: &Value, state: &Value) -> Option<String> {
    let used = state["cumulative"]["retries"].as_u64().unwrap_or(0);
    let spent = state["cumulative"]["time_s"].as_f64().unwrap_or(0.0);
    if let Some(max) = contract["budgets"]["max_retries"]
        .as_u64()
        .filter(|m| used >= *m)
    {
        return Some(format!("retry budget exhausted ({used} of {max})"));
    }
    contract["budgets"]["max_time_s"]
        .as_f64()
        .filter(|m| spent >= *m)
        .map(|m| format!("time budget exhausted ({spent:.0}s of {m:.0}s)"))
}

/// Run every frozen check in `workspace`, recording each run.
fn run_checks(
    store: &Store,
    contract: &Value,
    workspace: &Path,
    candidate_id: &str,
) -> Result<(BTreeMap<String, String>, Vec<Value>)> {
    let mut statuses = BTreeMap::new();
    let mut runs = Vec::new();
    for (id, def) in contract["checks"].as_object().into_iter().flatten() {
        let (status, _, mut record) = run_check(id, def, workspace, &contract["execution"]);
        record["candidate_id"] = json!(candidate_id);
        record["contract_id"] = contract["contract_id"].clone();
        store.append("check_run", &record)?;
        statuses.insert(id.clone(), status);
        runs.push(record);
    }
    Ok((statuses, runs))
}

fn check_summary(r: &Value) -> Value {
    json!({"check_id": r["check_id"], "status": r["status"], "attribution": r["attribution"],
           "exit_code": r["exit_code"], "duration_ms": r["duration_ms"],
           "verified_clean": r["receipt"]["verified_clean"], "isolation": r["receipt"]["isolation"]})
}

/// Verify a candidate: run every frozen check, compute the gates, reduce,
/// charge the budget, and record the evidence.
pub fn verify(store: &Store, candidate: &Candidate) -> Result<Value> {
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let baseline = contract["baseline"]["sha"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let candidate_id = match candidate {
        Candidate::Ref(r) => snapshot::resolve_ref(&repo, r)?,
        Candidate::Worktree => WORKTREE.to_owned(),
    };
    let mut state = store.state()?;
    let head = |decision: &str, reasons: Vec<String>| {
        json!({"task_id": contract["task_id"], "contract_id": contract["contract_id"],
               "candidate_id": candidate_id, "decision": decision, "reasons": reasons})
    };
    if let Some(why) = budget_exhausted(&contract, &state) {
        let d = head("rejected", vec![why]);
        store.append("decision", &d)?;
        return Ok(d);
    }
    let started = std::time::Instant::now();
    let changed = changes(&repo, &baseline, &candidate_id)?;
    let envelope = envelope_gate(&contract, &changed);
    let scratch = Scratch::new()?;
    let workspace = scratch.0.join("candidate");
    materialize(&repo, &candidate_id, &workspace, &contract)?;
    // verification policy: the checks see trusted files as they were at the
    // baseline, plus the held-out files the candidate never saw
    let vspec = &contract["verification"];
    let restored = restore_trusted(
        &repo,
        &baseline,
        &workspace,
        &globs(&vspec["trusted"]),
        &changed,
    )?;
    let held_out = inject_held_out(&workspace, &vspec["held_out"])?;
    let candidate_ids = (!vspec["inventory"].is_null())
        .then(|| inventory_ids(&vspec["inventory"], &workspace, &contract["execution"]));
    let (statuses, runs) = run_checks(store, &contract, &workspace, &candidate_id)?;
    let structural = structural_gate(&contract, &repo, &baseline, &candidate_id);
    let guardrails = guardrails_gate(&contract, &repo, &baseline, &candidate_id);
    let verification = verification_gate(
        &contract,
        &repo,
        &baseline,
        restored,
        held_out,
        candidate_ids,
        &scratch,
    );
    let ledger = ledger(&contract, &statuses);
    let require_all = contract["completion_policy"]["require_all_requirements_verified"]
        .as_bool()
        .unwrap_or(true);
    let gates = Gates {
        envelope: &envelope,
        structural: &structural,
        guardrails: &guardrails,
        verification: &verification,
    };
    let (decision, reasons) = reduce(&ledger, &gates, require_all);
    let elapsed = started.elapsed().as_secs_f64();
    state["cumulative"]["retries"] =
        json!(state["cumulative"]["retries"].as_u64().unwrap_or(0) + 1);
    state["cumulative"]["time_s"] =
        json!(state["cumulative"]["time_s"].as_f64().unwrap_or(0.0) + elapsed);
    store.save_state(&state)?;
    let mut d = head(decision, reasons);
    d["requirements"] = Value::Array(ledger);
    d["gates"] = json!({"envelope": envelope, "structural": structural, "guardrails": guardrails,
                        "verification": verification});
    d["checks"] = Value::Array(runs.iter().map(check_summary).collect());
    d["budget"] =
        json!({"retries_used": state["cumulative"]["retries"], "budgets": contract["budgets"]});
    store.append("decision", &d)?;
    Ok(d)
}

/// Completion: only a commit whose latest decision under the current
/// contract is `accepted`.
pub fn complete(store: &Store, reference: &str) -> Result<(bool, Value)> {
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let sha = snapshot::resolve_ref(&repo, reference)?;
    let cid = contract["contract_id"].as_str().unwrap_or("");
    let latest = store.latest_decision(&sha, cid);
    let accepted = latest.as_ref().is_some_and(|d| d["decision"] == "accepted");
    let reason = match &latest {
        None => "no verification of this commit under the current contract".to_owned(),
        Some(d) if !accepted => format!("latest decision is {}", d["decision"]),
        Some(_) => "accepted under the current contract".to_owned(),
    };
    let record = json!({"task_id": contract["task_id"], "contract_id": cid, "candidate_id": sha,
                        "completed": accepted, "reason": reason});
    store.append("completion", &record)?;
    if accepted {
        crate::store::atomic_write(&store.dir.join("completion.json"), &record)?;
    }
    Ok((accepted, record))
}

/// Pre-push check: the pushed commit must be the completed candidate.
pub fn hook_check(store: &Store, reference: &str) -> Result<(bool, String)> {
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let sha = snapshot::resolve_ref(&repo, reference)?;
    let cid = contract["contract_id"].as_str().unwrap_or("");
    let done = store.rows("completion").into_iter().any(|c| {
        c["candidate_id"] == sha.as_str() && c["contract_id"] == cid && c["completed"] == true
    });
    let task = contract["task_id"].as_str().unwrap_or("");
    let msg = if done {
        format!("{sha} completed task {task}")
    } else {
        format!("{sha} has not completed task {task} under its current contract")
    };
    Ok((done, msg))
}
