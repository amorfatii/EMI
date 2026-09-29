//! Coverage measured by EMI itself rather than read from a report someone
//! produced elsewhere: the coverage commands run in order in one fresh,
//! sandboxed copy of the profiled commit, and the report they write there
//! is read back. The evidence then belongs to exactly the commit being
//! profiled, and the run is recorded next to it.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use emi_exec::Status;
use serde_json::{Value, json};

use crate::coverage::{self, Resolved};
use crate::sandbox::{Runner, tail};

/// The commands that produce a coverage report, and where it is written.
#[derive(Clone, Debug)]
pub struct CoverageRun {
    /// Run in order in the same copy (argv each, no shell).
    pub commands: Vec<Vec<String>>,
    /// Report path relative to the copy's root.
    pub output: String,
}

fn relative(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Run each step; the first that does not exit 0 cleanly is the error.
fn steps(
    runner: &Runner,
    copy: &crate::sandbox::Copy,
    run: &CoverageRun,
) -> (Vec<Value>, Option<String>) {
    let mut records = Vec::new();
    for (n, argv) in run.commands.iter().enumerate() {
        let o = runner.run(copy, argv, runner.timeout);
        let ok = o.status == Status::Exited && o.exit_code == Some(0) && o.receipt.verified_clean;
        records.push(
            json!({"command": argv, "status": o.status, "exit_code": o.exit_code,
                            "duration_ms": o.duration_ms, "isolation": o.receipt.isolation,
                            "stderr_tail": if ok { String::new() } else { tail(&o.stderr, 600) }}),
        );
        if !ok {
            let why = match o.status {
                Status::Exited if !o.receipt.verified_clean => "left processes behind".to_owned(),
                Status::Exited => match o.exit_code {
                    Some(code) => format!("exited {code}"),
                    None => format!("was killed by signal {:?}", o.signal),
                },
                Status::TimedOut => "timed out".to_owned(),
                Status::NotLaunched => {
                    format!("could not start: {}", o.launch_error.unwrap_or_default())
                }
            };
            return (records, Some(format!("coverage step {} {why}", n + 1)));
        }
    }
    (records, None)
}

fn read_report(copy: &Path, output: &str) -> Result<String, String> {
    let path = copy.join(output);
    let meta = path
        .symlink_metadata()
        .map_err(|e| format!("the coverage commands wrote no {output}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{output} is not a regular file"));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("{output}: {e}"))
}

/// Measure coverage at the runner's commit; the report is placed onto
/// `files` (the commit's files). Returns the report or why there is none,
/// and the record of the run either way.
pub fn measure(
    runner: &Runner,
    run: &CoverageRun,
    files: &BTreeSet<String>,
) -> (Result<Resolved, String>, Value) {
    if run.commands.is_empty() || !relative(&run.output) {
        let why = "coverage run needs at least one command and a relative --coverage-output path";
        return (Err(why.into()), json!({"output": run.output}));
    }
    let copy = match runner.copy("coverage") {
        Ok(c) => c,
        Err(e) => return (Err(e), json!({"output": run.output})),
    };
    let (records, failed) = steps(runner, &copy, run);
    let record = json!({"output": run.output, "steps": records});
    if let Some(why) = failed {
        return (Err(why), record);
    }
    let root = std::fs::canonicalize(&copy.dir).unwrap_or_else(|_| copy.dir.clone());
    let resolved = read_report(&copy.dir, &run.output)
        .and_then(|text| coverage::parse(&text))
        .map(|report| coverage::resolve(report, &root, files));
    (resolved, record)
}
