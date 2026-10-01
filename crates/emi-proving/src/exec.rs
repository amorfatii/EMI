//! Sandboxed runs of the test command: listing tests, running chosen ones
//! and running them under coverage. Every run goes through `emi-profile`'s
//! sandbox runner (and so `emi-exec`): a fresh copy of a commit, no network
//! unless asked, the live repository hidden, dependency directories
//! read-only, the process group cleaned up and the cleanup verified.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use emi_exec::{Outcome, Status};
use emi_profile::coverage::Resolved;
use emi_profile::covrun::{self, CoverageRun};
use emi_profile::sandbox::{Copy, Runner};

use crate::junit::{self, Observed, RunExit};
use crate::pytest::{self, Command};

/// Reports larger than this are not read: a real one is a few kilobytes
/// per hundred tests.
const MAX_REPORT_BYTES: u64 = 64 << 20;
/// Where the runs write their files inside a copy; a dot directory, so
/// test discovery skips it.
const WORK: &str = ".emi-proving";

pub struct Session<'a> {
    pub runner: Runner<'a>,
    /// The repository that holds the commits (a clean view of the live one).
    pub objects: &'a Path,
    pub command: &'a Command,
    isolations: RefCell<BTreeSet<String>>,
}

/// How a run ended, in words for the receipt. Nothing in the words varies
/// from one run of the same inputs to the next (no durations, no scratch
/// paths): the receipt is deterministic.
pub fn exit_of(o: &Outcome, limit: Duration) -> RunExit {
    match (o.status, o.exit_code) {
        (Status::NotLaunched, _) => RunExit::Failed(format!(
            "could not start: {}",
            o.launch_error.as_deref().unwrap_or("unknown launch error")
        )),
        (Status::TimedOut, _) => {
            RunExit::Failed(format!("timed out (limit {} s)", limit.as_secs()))
        }
        (Status::Exited, _) if !o.receipt.verified_clean => {
            RunExit::Failed("the run left processes behind".into())
        }
        (Status::Exited, Some(code)) => RunExit::Code(code),
        (Status::Exited, None) => RunExit::Failed(format!("killed by signal {:?}", o.signal)),
    }
}

/// What failed, from pytest's output: the last three of its `E   ...`
/// lines (the exception is last), without the scratch directory's path.
/// The rest of the output holds timings that differ between runs.
pub fn error_lines(out: &str, dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    let lines: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("E ").map(str::trim))
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(3)..]
        .iter()
        .map(|l| l.replace(dir.as_ref(), "<tree>"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `: detail`, or nothing when there is none.
fn with(detail: String) -> String {
    if detail.is_empty() {
        detail
    } else {
        format!(": {detail}")
    }
}

/// A report the run wrote in `copy`, read with the checks that do not need
/// the run's other results.
fn read_report(copy: &Path, rel: &str) -> Result<junit::Report, String> {
    let path = copy.join(rel);
    let meta = path
        .symlink_metadata()
        .map_err(|e| format!("the run wrote no report: {e}"))?;
    if !meta.is_file() {
        return Err("the report is not a regular file".into());
    }
    if meta.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "the report is larger than {MAX_REPORT_BYTES} bytes"
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("the report: {e}"))?;
    junit::parse(&text)
}

impl<'a> Session<'a> {
    pub fn new(runner: Runner<'a>, objects: &'a Path, command: &'a Command) -> Session<'a> {
        Session {
            runner,
            objects,
            command,
            isolations: RefCell::new(BTreeSet::new()),
        }
    }

    /// A fresh copy of `rev`.
    pub fn tree(&self, tag: &str, rev: &str) -> Result<Copy, String> {
        self.runner.copy_of(tag, self.objects, rev)
    }

    fn run(&self, copy: &Copy, argv: &[String]) -> Outcome {
        let o = self.runner.run(copy, argv, self.runner.timeout);
        self.isolations
            .borrow_mut()
            .insert(o.receipt.isolation.clone());
        o
    }

    /// The isolation the runs had: `bwrap`, or the weaker one that was
    /// used instead (never claimed stronger than it was). `None` when
    /// nothing ran.
    pub fn isolation(&self) -> Option<String> {
        let seen = self.isolations.borrow();
        match seen.len() {
            0 => None,
            _ if seen.iter().all(|i| i == "bwrap") => Some("bwrap".into()),
            _ => Some(seen.iter().cloned().collect::<Vec<_>>().join("+")),
        }
    }

    /// The node ids the test command lists in `copy`, or why it could not.
    pub fn list(&self, copy: &Copy, what: &str) -> Result<BTreeSet<String>, String> {
        let o = self.run(copy, &self.command.list_argv());
        match exit_of(&o, self.runner.timeout) {
            // the executor keeps a bounded amount of output: a longer list
            // would silently lose its tail, and with it added tests
            RunExit::Code(0) if o.stdout_bytes > o.stdout.len() as u64 => Err(format!(
                "listing the tests at the {what} printed more than {} bytes, which is more than is kept",
                o.stdout.len()
            )),
            RunExit::Code(0) => pytest::parse_listing(&o.stdout)
                .map_err(|e| format!("listing the tests at the {what}: {e}")),
            RunExit::Code(code) => Err(format!(
                "listing the tests at the {what} exited {code}{}",
                with(error_lines(&o.stdout, &copy.dir))
            )),
            RunExit::Failed(why) => Err(format!("listing the tests at the {what} {why}")),
        }
    }

    /// Run exactly `ids` in `copy` and say what each did. `tag` keeps the
    /// report files of different runs in one copy apart.
    pub fn observe(&self, copy: &Copy, ids: &[String], tag: &str) -> BTreeMap<String, Observed> {
        let mut seen = BTreeMap::new();
        for (n, batch) in pytest::batches(ids).into_iter().enumerate() {
            let rel = format!("{WORK}/junit-{tag}-{n}.xml");
            // a file committed there would be read as this run's if the
            // run died before writing its own
            let _ = std::fs::remove_file(copy.dir.join(&rel));
            let o = self.run(copy, &self.command.run_argv(batch, &rel));
            let report = read_report(&copy.dir, &rel);
            let exit = exit_of(&o, self.runner.timeout);
            seen.extend(junit::observe(&exit, report, batch));
        }
        seen
    }

    /// Run exactly `ids` under coverage.py in `copy` and place its report
    /// on the files the copy holds.
    pub fn coverage(
        &self,
        copy: &Copy,
        ids: &[String],
        sources: &[String],
    ) -> Result<Resolved, String> {
        let output = format!("{WORK}/coverage.json");
        let run = CoverageRun {
            commands: self.command.coverage_argv(ids, sources, &output),
            output,
        };
        let files: BTreeSet<String> = copy.files.iter().cloned().collect();
        let (resolved, record) = covrun::measure_in(&self.runner, copy, &run, &files);
        let steps = record["steps"].as_array().map_or(&[][..], Vec::as_slice);
        for step in steps {
            if let Some(isolation) = step["isolation"].as_str() {
                self.isolations.borrow_mut().insert(isolation.to_owned());
            }
        }
        // the step that failed is the last one recorded; its last error
        // line says why (the rest of its output holds timings)
        resolved.map_err(|why| {
            let last = steps
                .last()
                .and_then(|s| s["stderr_tail"].as_str())
                .and_then(|err| err.lines().rev().find(|l| !l.trim().is_empty()))
                .map(|l| {
                    l.trim()
                        .replace(copy.dir.to_string_lossy().as_ref(), "<tree>")
                });
            format!("{why}{}", with(last.unwrap_or_default()))
        })
    }
}

#[cfg(test)]
mod tests;
