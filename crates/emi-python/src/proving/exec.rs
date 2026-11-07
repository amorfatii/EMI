//! Sandboxed runs of the test command: listing tests, running chosen ones
//! and running them under coverage. Every run goes through this crate's
//! sandbox runner (and so `emi-exec`): a fresh copy of a commit, no network
//! unless asked, the live repository hidden, dependency directories
//! read-only, the process group cleaned up and the cleanup verified.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use crate::covrun::{self, CoverageRun};
pub use crate::proving::contexts::Coverage;
use crate::sandbox::{Copy, Runner};
use emi_exec::{Outcome, Status};

use crate::proving::junit::{self, Observed, RunExit};
use crate::proving::pytest::{self, Command};

/// The coverage plugin that names each test's context.
pub(crate) const CONTEXTS: &str = include_str!("emi_proving_contexts.py");

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
    isolations: Mutex<BTreeSet<String>>,
}

/// What the test command lists at one revision.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub ids: BTreeSet<String>,
    /// Test files the listing could not collect.
    pub broken: BTreeSet<String>,
    /// What the listing said went wrong, when it exited nonzero.
    pub problem: Option<String>,
}

impl Listing {
    /// A listing kept by [`Listing::write`], or nothing when there is none
    /// or it does not read back as one.
    pub fn read(path: &Path) -> Option<Listing> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let set = |key: &str| -> Option<BTreeSet<String>> {
            v[key]
                .as_array()?
                .iter()
                .map(|i| i.as_str().map(str::to_owned))
                .collect()
        };
        Some(Listing {
            ids: set("ids")?,
            broken: set("broken")?,
            problem: v["problem"].as_str().map(str::to_owned),
        })
    }

    /// Keep the listing at `path`, written whole or not at all.
    pub fn write(&self, path: &Path) {
        let v =
            serde_json::json!({"ids": self.ids, "broken": self.broken, "problem": self.problem});
        let part = path.with_extension("part");
        let written = std::fs::create_dir_all(path.parent().unwrap_or(path))
            .and_then(|()| std::fs::write(&part, v.to_string()))
            .and_then(|()| std::fs::rename(&part, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&part);
        }
    }
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

/// Candidate-owned links must not redirect the controller's writes, even
/// before a sandbox starts. Reserved-name collisions are unknown evidence.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create reserved coverage input: {e}"))?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

/// What a run of chosen tests showed about each.
pub struct Observation {
    pub seen: BTreeMap<String, Observed>,
    /// What each failing test said.
    pub said: BTreeMap<String, String>,
    /// The inputs each test that draws them ran on, or why that could not
    /// be read.
    pub examples: BTreeMap<String, Result<emi_driver::Examples, String>>,
}

impl<'a> Session<'a> {
    pub fn new(runner: Runner<'a>, objects: &'a Path, command: &'a Command) -> Session<'a> {
        Session {
            runner,
            objects,
            command,
            isolations: Mutex::new(BTreeSet::new()),
        }
    }

    /// A fresh copy of `rev`.
    pub fn tree(&self, tag: &str, rev: &str) -> Result<Copy, String> {
        self.runner.copy_of(tag, self.objects, rev)
    }

    fn run(&self, copy: &Copy, argv: &[String]) -> Outcome {
        self.run_with(&self.runner, copy, argv)
    }

    fn run_with(&self, runner: &Runner, copy: &Copy, argv: &[String]) -> Outcome {
        let o = runner.run(copy, argv, runner.timeout);
        if let Ok(mut seen) = self.isolations.lock() {
            seen.insert(o.receipt.isolation.clone());
        }
        o
    }

    /// The isolation the runs had: `bwrap`, or the weaker one that was
    /// used instead (never claimed stronger than it was). `None` when
    /// nothing ran.
    pub fn isolation(&self) -> Option<String> {
        let seen = self.isolations.lock().ok()?;
        match seen.len() {
            0 => None,
            _ if seen.iter().all(|i| i == "bwrap") => Some("bwrap".into()),
            _ => Some(seen.iter().cloned().collect::<Vec<_>>().join("+")),
        }
    }

    /// The node ids the test command lists in `copy`, or why it could not.
    /// A test file that cannot be imported makes the listing exit 1 or 2
    /// and still name every test of the files that can: those files are
    /// returned as `broken`, with what the listing said about them, so the
    /// caller can tell a file the baseline already could not collect from
    /// one the candidate broke.
    pub fn list(&self, copy: &Copy, what: &str) -> Result<Listing, String> {
        let o = self.run(copy, &self.command.list_argv());
        let exit = exit_of(&o, self.runner.timeout);
        let code = match exit {
            RunExit::Code(code @ 0..=2) => code,
            RunExit::Code(code) => {
                return Err(format!(
                    "listing the tests at the {what} exited {code}{}",
                    with(error_lines(&o.stdout, &copy.dir))
                ));
            }
            RunExit::Failed(why) => return Err(format!("listing the tests at the {what} {why}")),
        };
        // the executor keeps a bounded amount of output: a longer list
        // would silently lose its tail, and with it added tests
        if o.stdout_bytes > o.stdout.len() as u64 {
            return Err(format!(
                "listing the tests at the {what} printed more than {} bytes, which is more than is kept",
                o.stdout.len()
            ));
        }
        let dir = copy.dir.to_string_lossy();
        // a test parametrized by a path under its own checkout is named
        // after the copy it was listed in: without the copy's path it has
        // one name at both revisions
        let ids = pytest::parse_listing(&o.stdout)
            .map_err(|e| format!("listing the tests at the {what}: {e}"))?
            .iter()
            .map(|id| id.replace(dir.as_ref(), "<workspace>"))
            .collect();
        let broken = pytest::uncollected_files(&o.stdout);
        let problem = (code != 0).then(|| {
            format!(
                "listing the tests at the {what} exited {code}{}",
                with(error_lines(&o.stdout, &copy.dir))
            )
        });
        if code != 0 && broken.is_empty() {
            // nonzero without naming a file it could not collect: not a
            // listing this reader understands
            return Err(problem.unwrap_or_default());
        }
        Ok(Listing {
            ids,
            broken,
            problem,
        })
    }

    /// Run exactly `ids` in `copy` and say what each did. `tag` keeps the
    /// report files of different runs in one copy apart.
    pub fn observe(&self, copy: &Copy, ids: &[String], tag: &str) -> BTreeMap<String, Observed> {
        self.observe_explained(copy, ids, tag).0
    }

    /// [`Session::observe`] for a run that only confirms failures: whether
    /// each fails is all it is asked, so Hypothesis is not made to shrink
    /// again what it shrank the first time.
    pub fn observe_confirming(
        &self,
        copy: &Copy,
        ids: &[String],
        tag: &str,
    ) -> BTreeMap<String, Observed> {
        let mut env = self.runner.env.to_vec();
        env.push((emi_kernel::pins::CONFIRM_ENV.to_owned(), "1".to_owned()));
        let runner = Runner {
            env: &env,
            ..self.runner
        };
        self.observe_with(&runner, copy, ids, tag).seen
    }

    /// [`Session::observe`], with what each failing test said (its
    /// counterexample), the copy's path taken out.
    pub fn observe_explained(
        &self,
        copy: &Copy,
        ids: &[String],
        tag: &str,
    ) -> (BTreeMap<String, Observed>, BTreeMap<String, String>) {
        let o = self.observe_with(&self.runner, copy, ids, tag);
        (o.seen, o.said)
    }

    /// [`Session::observe_explained`], with the inputs each test that
    /// draws them ran on.
    pub fn observe_all(&self, copy: &Copy, ids: &[String], tag: &str) -> Observation {
        self.observe_with(&self.runner, copy, ids, tag)
    }

    fn observe_with(&self, runner: &Runner, copy: &Copy, ids: &[String], tag: &str) -> Observation {
        let mut seen = BTreeMap::new();
        let mut said = BTreeMap::new();
        let mut examples = BTreeMap::new();
        let dir = copy.dir.to_string_lossy().into_owned();
        for (n, batch) in pytest::batches(ids).into_iter().enumerate() {
            let rel = format!("{WORK}/junit-{tag}-{n}.xml");
            // a file committed there would be read as this run's if the
            // run died before writing its own
            let _ = std::fs::remove_file(copy.dir.join(&rel));
            let o = self.run_with(runner, copy, &self.command.run_argv(batch, &rel));
            let mut exit = exit_of(&o, runner.timeout);
            // pytest cannot select every test by its id (parameters with
            // escapes in them, say) and then runs nothing. The file run
            // whole reports the same tests, the named ones among them.
            if matches!(exit, RunExit::Code(4)) {
                let _ = std::fs::remove_file(copy.dir.join(&rel));
                let file = [pytest::file_of(&batch[0]).to_owned()];
                let whole = self.run_with(runner, copy, &self.command.run_argv(&file, &rel));
                exit = exit_of(&whole, runner.timeout);
            }
            let report = read_report(&copy.dir, &rel);
            let _ = std::fs::remove_file(copy.dir.join(&rel));
            let _ = std::fs::remove_dir(copy.dir.join(WORK));
            if let Ok(r) = &report {
                for id in batch {
                    if let Some(m) = r.message(id) {
                        said.insert(id.clone(), m.replace(dir.as_str(), "<tree>"));
                    }
                    if let Some(e) = r.examples(id) {
                        examples.insert(id.clone(), e);
                    }
                }
            }
            seen.extend(junit::observe(&exit, report, batch));
        }
        Observation {
            seen,
            said,
            examples,
        }
    }

    /// Run exactly `ids` under coverage.py in `copy` and place its report
    /// on the files the copy holds.
    pub fn coverage(
        &self,
        copy: &Copy,
        ids: &[String],
        sources: &[String],
    ) -> Result<Coverage, String> {
        let rcfile = format!("{WORK}/coverage.ini");
        // Python 3.14 defaults to sys.monitoring, which records a line only
        // once and cannot switch contexts reliably between tests.
        let rc = format!(
            "[run]\ncore = ctrace\nsource = {}\nrelative_files = True\n\n[json]\nshow_contexts = True\n",
            sources.join(",")
        );
        std::fs::create_dir(copy.dir.join(WORK))
            .map_err(|e| format!("cannot create reserved coverage directory: {e}"))?;
        write_new(&copy.dir.join(&rcfile), rc.as_bytes())?;
        write_new(
            &copy.dir.join("emi_proving_contexts.py"),
            CONTEXTS.as_bytes(),
        )?;
        let output = format!("{WORK}/coverage.json");
        let run = CoverageRun {
            commands: self.command.coverage_argv(ids, &rcfile, &output),
            output,
        };
        let files: BTreeSet<String> = copy.files.iter().cloned().collect();
        let (resolved, record) = covrun::measure_in(&self.runner, copy, &run, &files);
        let steps = record["steps"].as_array().map_or(&[][..], Vec::as_slice);
        for step in steps {
            if let Some(isolation) = step["isolation"].as_str()
                && let Ok(mut seen) = self.isolations.lock()
            {
                seen.insert(isolation.to_owned());
            }
        }
        // the step that failed is the last one recorded; its last error
        // line says why (the rest of its output holds timings)
        let resolved = resolved.map_err(|why| {
            let last = steps
                .last()
                .and_then(|s| s["stderr_tail"].as_str())
                .and_then(|err| err.lines().rev().find(|l| !l.trim().is_empty()))
                .map(|l| {
                    l.trim()
                        .replace(copy.dir.to_string_lossy().as_ref(), "<tree>")
                });
            format!("{why}{}", with(last.unwrap_or_default()))
        })?;
        let text =
            std::fs::read_to_string(copy.dir.join(&run.output)).map_err(|e| e.to_string())?;
        Coverage::read(resolved, &text)
    }
}

#[cfg(test)]
mod tests;
