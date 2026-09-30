//! Behavioural drift under a task: the inputs the checks the requirements
//! bind pass to the modified Python functions, recorded on the baseline and
//! replayed against the baseline and the candidate. The comparison is
//! `emi drift`'s (`emi_profile::drift`); only where the commands run
//! differs: in workspaces prepared like the checks' (the baseline as for
//! its inventory, the candidate with trusted files restored and held-out
//! files injected), under the contract's execution policy. It is a review
//! signal and never changes the decision.

use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_exec::{Isolation, Outcome};
use emi_profile::drift::{NOTE, Request, Side, Tree};
use serde_json::{Map, Value, json};

use super::{Mounts, RunSetup, Signals, globs, required_checks, sandbox_required, spec_within};
use crate::contract::Result;

/// Upper bound for one replayed call, as `emi drift` defaults it.
const CALL_TIMEOUT_S: f64 = 2.0;

/// The drift gate, when the contract asks for it: the report of
/// `emi drift` plus `affects_decision: false`. A step that fails makes it
/// `unverified` with the failure, never "no drift".
pub(super) fn gate(s: &Signals) -> Option<Value> {
    if s.setup.contract["verification"]["drift"].is_null() {
        return None;
    }
    let mut gate =
        measure(s).unwrap_or_else(|e| json!({"state": "unverified", "reasons": [e], "note": NOTE}));
    gate["affects_decision"] = json!(false);
    Some(gate)
}

fn measure(s: &Signals) -> Result<Value> {
    let setup = s.setup;
    let checks = required_checks(setup.contract);
    let commands: Vec<Vec<String>> = checks.iter().map(|(_, d)| globs(&d["command"])).collect();
    // the longest check bounds each test run and each side's replay
    let longest = checks
        .iter()
        .map(|(_, d)| d["timeout_s"].as_f64().unwrap_or(120.0))
        .fold(0.0, f64::max);
    let timeout = Duration::try_from_secs_f64(longest)
        .map_err(|_| format!("timeout_s {longest} is not a usable number of seconds"))?;
    let env: Vec<(String, String)> = setup.contract["execution"]["env"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect();
    let side = |baseline| TaskSide {
        setup,
        baseline,
        scratch: s.scratch,
        env: &env,
        timeout,
    };
    let (base_side, head_side) = (side(true), side(false));
    emi_profile::drift::measure(&Request {
        repo: setup.objects,
        base: setup.baseline.to_owned(),
        head: setup.candidate.to_owned(),
        commands: &commands,
        call_timeout_s: CALL_TIMEOUT_S,
        base_side: &base_side,
        head_side: &head_side,
    })
}

/// The baseline or the candidate, as the task's checks see it.
struct TaskSide<'a> {
    setup: &'a RunSetup<'a>,
    baseline: bool,
    scratch: &'a Path,
    env: &'a [(String, String)],
    timeout: Duration,
}

/// A prepared workspace and what its sandbox mounts; removed when dropped.
struct Workspace {
    dir: PathBuf,
    mounts: Mounts,
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Tree for Workspace {
    fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Side for TaskSide<'_> {
    type Tree = Workspace;

    fn tree(&self, tag: &str) -> Result<Workspace> {
        let dir = self.scratch.join(tag);
        let mounts = if self.baseline {
            self.setup.prepare_baseline(&dir)?
        } else {
            self.setup.prepare(&dir)?
        };
        Ok(Workspace { dir, mounts })
    }

    fn run(
        &self,
        tree: &Workspace,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Outcome {
        let mut execution = self.setup.contract["execution"].clone();
        let env: Map<String, Value> = env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        execution["env"] = Value::Object(env);
        let mut spec = spec_within(
            &json!(argv),
            &tree.dir,
            &execution,
            timeout,
            tree.mounts.clone(),
        );
        // a run that cannot be sandboxed must not start rather than run
        // without the isolation the contract asks for
        if sandbox_required(&execution) {
            spec.isolation = Isolation::Bwrap {
                network: execution["network"].as_bool().unwrap_or(false),
            };
        }
        emi_exec::run(&spec)
    }

    fn env(&self) -> &[(String, String)] {
        self.env
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }
}
