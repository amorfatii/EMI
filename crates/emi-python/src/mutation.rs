//! Sampled mutation runs. Each mutant is applied to a fresh copy of the
//! commit being checked and the test command runs against it in the sandbox:
//! a failing run means the tests noticed the wrong code ("killed"), a
//! passing run means they did not ("survived"). The unmutated commit runs
//! first; if its tests do not pass, no mutant result would mean anything,
//! and none is reported.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::mutate::Mutant;
use emi_exec::Status;
use serde_json::{Value, json};

use crate::Result;
use crate::sandbox::{Runner, tail};

/// How to run the tests: the sandbox and the test command.
pub struct Plan<'a> {
    pub runner: Runner<'a>,
    pub command: &'a [String],
}

/// The mutants chosen for one function.
pub struct Target {
    pub key: FunctionKey,
    pub mutants: Vec<Mutant>,
}

/// `(repository-relative file, function name, first line)`.
pub type FunctionKey = (String, String, u32);

/// What the mutants of one function did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub killed: u32,
    pub survived: u32,
    pub timed_out: u32,
    pub errors: u32,
    /// Mutants the tests did not notice.
    pub survivors: Vec<Mutant>,
}

impl Tally {
    /// Mutants with a verdict (errors have none).
    pub fn resolved(&self) -> u32 {
        self.killed + self.survived + self.timed_out
    }

    /// Share of resolved mutants the tests noticed (a timeout counts as
    /// noticed: the wrong code did not finish like the right one).
    pub fn score(&self) -> Option<f64> {
        let resolved = self.resolved();
        (resolved > 0).then(|| f64::from(self.killed + self.timed_out) / f64::from(resolved))
    }

    pub fn to_json(&self) -> Value {
        json!({"killed": self.killed, "survived": self.survived, "timed_out": self.timed_out,
               "errors": self.errors, "score": self.score(),
               "survivors": self.survivors.iter().map(Mutant::describe).collect::<Vec<_>>()})
    }
}

/// The outcome of the whole mutation stage.
pub struct Results {
    /// The unmutated run; mutant results exist only if it passed.
    pub baseline: Value,
    pub passed_baseline: bool,
    pub tallies: BTreeMap<FunctionKey, Tally>,
    /// The isolation the runs actually had.
    pub isolation: String,
}

/// Write the mutated file, refusing to write through a link in the tree.
pub fn write_mutant(dir: &Path, file: &str, mutant: &Mutant) -> Result<()> {
    let path = dir.join(file);
    let meta = path
        .symlink_metadata()
        .map_err(|e| format!("{file}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{file} is not a regular file in the tree"));
    }
    let src = std::fs::read(&path).map_err(|e| format!("{file}: {e}"))?;
    std::fs::write(&path, mutant.apply(&src)).map_err(|e| format!("{file}: {e}"))
}

enum Verdict {
    Killed,
    Survived,
    TimedOut,
    Error,
}

fn verdict(o: &emi_exec::Outcome) -> Verdict {
    match o.status {
        Status::NotLaunched => Verdict::Error,
        _ if !o.receipt.verified_clean => Verdict::Error,
        Status::TimedOut => Verdict::TimedOut,
        Status::Exited if o.exit_code == Some(0) => Verdict::Survived,
        Status::Exited => Verdict::Killed,
    }
}

/// Run the unmutated commit's tests; `(record, passed, duration, isolation)`.
fn baseline(plan: &Plan) -> Result<(Value, bool, Duration, String)> {
    let copy = plan.runner.copy("baseline")?;
    let o = plan.runner.run(&copy, plan.command, plan.runner.timeout);
    let passed = matches!(verdict(&o), Verdict::Survived);
    let record = json!({"passed": passed, "status": o.status, "exit_code": o.exit_code,
                        "duration_ms": o.duration_ms, "isolation": o.receipt.isolation,
                        "stderr_tail": if passed { String::new() } else { tail(&o.stderr, 600) }});
    Ok((
        record,
        passed,
        Duration::from_millis(o.duration_ms),
        o.receipt.isolation.clone(),
    ))
}

/// One mutant's verdict (errors preparing the copy count as errors).
fn run_mutant(plan: &Plan, key: &FunctionKey, m: &Mutant, n: usize, timeout: Duration) -> Verdict {
    let prepared = plan
        .runner
        .copy(&format!("mutant-{n}"))
        .and_then(|copy| write_mutant(&copy.dir, &key.0, m).map(|()| copy));
    match prepared {
        Ok(copy) => verdict(&plan.runner.run(&copy, plan.command, timeout)),
        Err(_) => Verdict::Error,
    }
}

/// The bound for one mutant's run: a mutant that loops forever must not
/// cost the whole budget, so at most three times the unmutated run (and
/// ten seconds more than it), never above `limit`.
pub fn mutant_timeout(limit: Duration, unmutated: Duration) -> Duration {
    limit.min((unmutated * 3).max(unmutated + Duration::from_secs(10)))
}

/// Run every target's mutants; nothing runs if the baseline fails.
pub fn run(plan: &Plan, targets: &[Target]) -> Result<Results> {
    let (record, passed, took, isolation) = baseline(plan)?;
    let mut results = Results {
        baseline: record,
        passed_baseline: passed,
        tallies: BTreeMap::new(),
        isolation,
    };
    if !passed {
        return Ok(results);
    }
    let timeout = mutant_timeout(plan.runner.timeout, took);
    let mut n = 1;
    for t in targets {
        let tally = results.tallies.entry(t.key.clone()).or_default();
        for m in &t.mutants {
            match run_mutant(plan, &t.key, m, n, timeout) {
                Verdict::Killed => tally.killed += 1,
                Verdict::TimedOut => tally.timed_out += 1,
                Verdict::Error => tally.errors += 1,
                Verdict::Survived => {
                    tally.survived += 1;
                    tally.survivors.push(m.clone());
                }
            }
            n += 1;
        }
    }
    Ok(results)
}
