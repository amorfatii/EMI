//! Sampled mutation runs. Each mutant is applied to a fresh copy of the
//! profiled commit and the test command runs against it in the sandbox:
//! a failing run means the tests noticed the wrong code ("killed"), a
//! passing run means they did not ("survived"). The unmutated commit runs
//! first; if its tests do not pass, no mutant result would mean anything,
//! and none is reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_diff::snapshot;
use emi_exec::{Isolation, Spec, Status};
use emi_panel::mutate::Mutant;
use serde_json::{Value, json};

use crate::Result;

/// How to run the tests.
pub struct Plan<'a> {
    pub repo: &'a Path,
    /// The commit whose copies are mutated.
    pub rev: &'a str,
    pub command: &'a [String],
    /// Ignored dependency directories linked into each copy.
    pub link: &'a [String],
    pub env: &'a [(String, String)],
    /// Upper bound for one run.
    pub timeout: Duration,
    pub network: bool,
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
    pub survivors: Vec<String>,
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
               "errors": self.errors, "score": self.score(), "survivors": self.survivors})
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

/// Scratch directory for one run's copy, removed when dropped.
struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_copy(plan: &Plan, n: usize) -> Result<(Copy, Vec<PathBuf>)> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("emi-mutant-{}-{nanos}-{n}", std::process::id()));
    let copy = Copy(dir);
    snapshot::materialize(plan.repo, plan.rev, &copy.0)?;
    let linked = snapshot::link_dependencies(plan.repo, &copy.0, plan.link)?;
    Ok((copy, linked))
}

fn spec(plan: &Plan, dir: &Path, linked: Vec<PathBuf>, timeout: Duration) -> Spec {
    let argv: Vec<&str> = plan.command.iter().map(String::as_str).collect();
    let mut spec = Spec::new(&argv, dir);
    spec.timeout = timeout;
    spec.isolation = Isolation::Best {
        network: plan.network,
    };
    // imports must come from the copy, never the live checkout
    spec.hidden = vec![plan.repo.to_path_buf()];
    spec.read_only = linked;
    spec.env.extend(plan.env.iter().cloned());
    spec
}

/// Write the mutated file, refusing to write through a link in the tree.
fn write_mutant(dir: &Path, file: &str, mutant: &Mutant) -> Result<()> {
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

fn tail(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(600)..].iter().collect()
}

/// Run the unmutated commit's tests; `(record, passed, duration)`.
fn baseline(plan: &Plan) -> Result<(Value, bool, Duration, String)> {
    let (copy, linked) = fresh_copy(plan, 0)?;
    let o = emi_exec::run(&spec(plan, &copy.0, linked, plan.timeout));
    let passed = matches!(verdict(&o), Verdict::Survived);
    let record = json!({"passed": passed, "status": o.status, "exit_code": o.exit_code,
                        "duration_ms": o.duration_ms, "isolation": o.receipt.isolation,
                        "stderr_tail": if passed { String::new() } else { tail(&o.stderr) }});
    Ok((
        record,
        passed,
        Duration::from_millis(o.duration_ms),
        o.receipt.isolation.clone(),
    ))
}

/// One mutant's verdict (errors preparing the copy count as errors).
fn run_mutant(plan: &Plan, key: &FunctionKey, m: &Mutant, n: usize, timeout: Duration) -> Verdict {
    let prepared = fresh_copy(plan, n)
        .and_then(|(copy, linked)| write_mutant(&copy.0, &key.0, m).map(|()| (copy, linked)));
    match prepared {
        Ok((copy, linked)) => verdict(&emi_exec::run(&spec(plan, &copy.0, linked, timeout))),
        Err(_) => Verdict::Error,
    }
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
    // a mutant that loops forever must not cost the whole budget
    let timeout = plan
        .timeout
        .min((took * 3).max(took + Duration::from_secs(10)));
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
                    tally.survivors.push(m.describe());
                }
            }
            n += 1;
        }
    }
    Ok(results)
}
