//! Mutants on the lines the candidate added, run through the task's own
//! checks: each mutant is applied in a workspace prepared exactly like a
//! check's (trusted files restored, held-out files injected, the same
//! execution policy and isolation) and the checks the requirements bind
//! run against it. A mutant is killed when one of them fails.
//!
//! The mutants are `emi diff-mutate`'s (the same selection and sampling);
//! only where they run differs. It is a review signal: the gate says how
//! much the tests pin the change and never changes the decision.

use std::collections::BTreeMap;
use std::time::Duration;

use emi_diff::functions::{ChangedFunctions, changed_functions};
use emi_panel::mutate::Mutant;
use emi_profile::diffmut::{self, DiffMutation, NotMutated, State};
use emi_profile::mutation::{FunctionKey, Tally, Target, mutant_timeout, write_mutant};
use serde_json::{Value, json};

use super::{Signals, globs, required_checks, run_check_within, trusted_changes};
use crate::contract::Result;

/// The mutation gate, when the contract asks for it: the report of
/// `emi diff-mutate` plus `affects_decision: false`. A step that fails
/// makes it `unverified` with the failure, never zero survivors.
pub(super) fn gate(s: &Signals) -> Option<Value> {
    let max = s.setup.contract["verification"]["mutation"]["max_mutants"].as_u64()?;
    let mut gate = measure(s, usize::try_from(max).unwrap_or(usize::MAX))
        .unwrap_or_else(|e| json!({"state": "unverified", "detail": e, "note": diffmut::NOTE}));
    gate["affects_decision"] = json!(false);
    Some(gate)
}

/// The requirement checks that did not pass unmutated, as `id: status`.
fn failing(s: &Signals) -> Vec<String> {
    required_checks(s.setup.contract)
        .into_iter()
        .filter_map(|(id, _)| {
            let status = s.statuses.get(id).map_or("not_run", String::as_str);
            (status != "passed").then(|| format!("{id}: {status}"))
        })
        .collect()
}

fn measure(s: &Signals, max: usize) -> Result<Value> {
    let failing = failing(s);
    if !failing.is_empty() {
        return Ok(json!({"state": "not_run",
            "reason": format!("the unmutated checks did not all pass ({}); a failing check would kill every mutant",
                              failing.join(", ")),
            "note": diffmut::NOTE}));
    }
    let setup = s.setup;
    let mut changed = changed_functions(setup.objects, setup.baseline, setup.candidate)?;
    let excluded = exclude(s, &mut changed);
    let mut not_mutated = NotMutated {
        unparsed: changed.unparsed.clone(),
        unsupported: changed.unsupported.clone(),
        ..NotMutated::default()
    };
    let all = diffmut::candidates(s.tree, &changed, &mut not_mutated);
    let mut out = DiffMutation {
        state: State::NothingToMutate,
        base: setup.baseline.to_owned(),
        head: setup.candidate.to_owned(),
        candidates: all.iter().map(|t| t.mutants.len()).sum(),
        run: Some(0),
        tally: None,
        survivors: None,
        not_mutated,
        baseline: json!({"passed": true, "checks": s.statuses}),
        isolation: None,
    };
    if out.candidates > 0 {
        let targets = diffmut::sample(all, max);
        let (tallies, isolation) = run_all(s, &targets)?;
        let (tally, survivors) = diffmut::gather(&targets, &tallies);
        out.state = State::Measured;
        out.run = Some(targets.iter().map(|t| t.mutants.len()).sum());
        out.tally = Some(tally);
        out.survivors = Some(survivors);
        out.isolation = isolation;
    }
    let mut v = out.to_json();
    v["not_mutated"]["excluded"] = json!(excluded);
    let errors = v["errors"].as_u64().unwrap_or(0);
    if errors > 0 {
        v["state"] = json!("unverified");
        v["detail"] = json!(format!(
            "{errors} mutant(s) could not be run to a verdict (the workspace could not be prepared, or the run failed to launch or clean up)"
        ));
    }
    Ok(v)
}

/// Drop the functions of files no mutant can say anything about, and say
/// why: test files (mutating a test says nothing about how it pins the
/// change), and files the checks see in another version than the
/// candidate's (restored trusted files, held-out files).
fn exclude(s: &Signals, changed: &mut ChangedFunctions) -> Vec<Value> {
    let v = &s.setup.contract["verification"];
    let mut replaced = trusted_changes(&globs(&v["trusted"]), s.setup.changed);
    replaced.extend(
        v["held_out"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, _)| k.clone()),
    );
    let why = |file: &str| {
        if replaced.iter().any(|r| r == file) {
            Some("the checks run another version of this file (restored or held out)")
        } else if emi_scan::classify::is_test(file) {
            Some("a test file: mutating it says nothing about how the tests pin the change")
        } else {
            None
        }
    };
    let mut excluded: BTreeMap<String, &str> = BTreeMap::new();
    changed.functions.retain(|f| match why(&f.file) {
        Some(reason) => {
            excluded.insert(f.file.clone(), reason);
            false
        }
        None => true,
    });
    excluded
        .into_iter()
        .map(|(file, reason)| json!({"file": file, "reason": reason}))
        .collect()
}

/// What one mutant did to the checks.
enum Verdict {
    Killed,
    Survived,
    TimedOut,
    Error,
}

/// Run every sampled mutant; the tallies per function and the isolation
/// the runs had.
fn run_all(
    s: &Signals,
    targets: &[Target],
) -> Result<(BTreeMap<FunctionKey, Tally>, Option<String>)> {
    let mut tallies: BTreeMap<FunctionKey, Tally> = BTreeMap::new();
    let mut isolation = None;
    let mut n = 0;
    for t in targets {
        for m in &t.mutants {
            n += 1;
            let verdict = run_mutant(s, &t.key, m, n, &mut isolation)?;
            let tally = tallies.entry(t.key.clone()).or_default();
            match verdict {
                Verdict::Killed => tally.killed += 1,
                Verdict::TimedOut => tally.timed_out += 1,
                Verdict::Error => tally.errors += 1,
                Verdict::Survived => {
                    tally.survived += 1;
                    tally.survivors.push(m.clone());
                }
            }
        }
    }
    Ok((tallies, isolation))
}

/// How long the unmutated run of `id` took.
fn unmutated_duration(s: &Signals, id: &str) -> Duration {
    let ms = s
        .runs
        .iter()
        .find(|r| r["check_id"] == id)
        .and_then(|r| r["duration_ms"].as_u64())
        .unwrap_or(0);
    Duration::from_millis(ms)
}

/// Apply mutant `n` in a fresh workspace per check and run the checks the
/// requirements bind, stopping at the first that does not pass. Every run
/// is recorded as a `check_run` naming the mutant.
fn run_mutant(
    s: &Signals,
    key: &FunctionKey,
    m: &Mutant,
    n: usize,
    isolation: &mut Option<String>,
) -> Result<Verdict> {
    let contract = s.setup.contract;
    let mutant = json!({"n": n, "file": key.0, "function": key.1, "line": m.line,
                        "original": m.original, "replacement": m.replacement});
    for (i, (id, def)) in required_checks(contract).into_iter().enumerate() {
        let dir = s.scratch.join(format!("mutant-{n}-{i}"));
        let prepared = s
            .setup
            .prepare(&dir)
            .and_then(|mounts| write_mutant(&dir, &key.0, m).map(|()| mounts));
        let limit = Duration::try_from_secs_f64(def["timeout_s"].as_f64().unwrap_or(120.0))
            .unwrap_or(Duration::from_secs(120));
        let timeout = mutant_timeout(limit, unmutated_duration(s, id)).as_secs_f64();
        let (status, mut record) = match prepared {
            Ok(mounts) => {
                let (status, _, record) = run_check_within(
                    id,
                    def,
                    timeout,
                    &dir,
                    &contract["execution"],
                    mounts,
                    false,
                );
                (status, record)
            }
            Err(e) => (
                "error".to_owned(),
                json!({"check_id": id, "status": "error", "attribution": "environment", "launch_error": e}),
            ),
        };
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(iso) = record["receipt"]["isolation"].as_str() {
            isolation.get_or_insert_with(|| iso.to_owned());
        }
        record["candidate_id"] = json!(s.candidate_id);
        record["contract_id"] = contract["contract_id"].clone();
        record["mutant"] = mutant.clone();
        s.store.append("check_run", &record)?;
        match status.as_str() {
            "passed" => {}
            "failed" => return Ok(Verdict::Killed),
            "timeout" => return Ok(Verdict::TimedOut),
            _ => return Ok(Verdict::Error),
        }
    }
    Ok(Verdict::Survived)
}
