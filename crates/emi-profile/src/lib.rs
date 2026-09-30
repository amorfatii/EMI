//! `emi profile`: per function, how the code is built (panel metrics and
//! refactoring steps), how its file has changed (revisions, authors,
//! change coupling), what the tests run (an imported coverage report) and
//! how much of it they check (sampled mutants), reduced to how far a change
//! there could be verified. Every entry keeps measured facts, inferences
//! and unknowns apart.
//!
//! The profile is advisory: it gates nothing. Whether diff coverage and
//! mutation results should gate `emi task verify` is what emi-lab S9
//! decides.

pub mod coverage;
pub mod covrun;
pub mod diffmut;
pub mod findings;
pub mod functions;
pub mod history;
pub mod mutation;
pub mod readiness;
pub mod sandbox;
pub mod text;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_diff::{Request, snapshot};
use serde_json::{Value, json};

use crate::coverage::Resolved;
use crate::covrun::CoverageRun;
use crate::functions::{Coverage, Mutation, Record};
use crate::history::{History, Window};
use crate::sandbox::Runner;

pub type Result<T> = std::result::Result<T, String>;

pub const SCHEMA: &str = "emi-profile-0.1";

/// What to profile and with which evidence.
pub struct Options {
    pub repo: PathBuf,
    pub source: String,
    pub probe: emi_scan::Probe,
    pub head: String,
    pub window: Window,
    pub coverage: Option<CoverageSource>,
    pub mutation: Option<MutationOptions>,
    /// How coverage and mutant runs execute.
    pub execution: Execution,
    /// Function entries in the output (all are counted in the summary).
    pub top: usize,
    pub tolerate_syntax_errors: bool,
}

/// Where line coverage comes from.
pub enum CoverageSource {
    /// A report produced elsewhere; it must be of the head commit.
    File(PathBuf),
    /// Commands EMI runs on a sandboxed copy of the head commit.
    Run(CoverageRun),
}

/// Sampled mutation: which functions, how many mutants, how to test.
pub struct MutationOptions {
    pub command: Vec<String>,
    /// The most exposed functions that get mutants.
    pub functions: usize,
    pub per_function: usize,
}

/// The sandbox for commands the profile runs.
#[derive(Clone, Debug)]
pub struct Execution {
    /// Ignored dependency directories linked into each copy.
    pub link: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Upper bound for one command.
    pub timeout: Duration,
    pub network: bool,
}

impl Default for Execution {
    fn default() -> Self {
        Execution {
            link: Vec::new(),
            env: Vec::new(),
            timeout: Duration::from_secs(600),
            network: false,
        }
    }
}

impl Execution {
    fn runner<'a>(&'a self, repo: &'a Path, rev: &'a str) -> Runner<'a> {
        Runner {
            repo,
            rev,
            link: &self.link,
            env: &self.env,
            timeout: self.timeout,
            network: self.network,
        }
    }
}

/// The head's files, read from a copy that is removed afterwards.
struct Tree {
    dir: PathBuf,
    files: BTreeSet<String>,
}

impl Tree {
    fn new(repo: &Path, sha: &str) -> Result<Tree> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("emi-profile-{}-{nanos}", std::process::id()));
        let inventory = snapshot::materialize(repo, sha, &dir)?;
        Ok(Tree {
            dir,
            files: inventory.files.into_iter().collect(),
        })
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn load_coverage(path: &Path, repo: &Path, files: &BTreeSet<String>) -> Result<Resolved> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("coverage report {}: {e}", path.display()))?;
    let report = coverage::parse(&text)?;
    let root = std::fs::canonicalize(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    Ok(coverage::resolve(report, &root, files))
}

/// Profile `o.head`.
pub fn build(o: &Options) -> Result<Value> {
    let mut req = Request::new(&o.repo, &o.source, &o.head, &o.head, o.probe);
    req.profile = emi_scan::Profile::Graded;
    req.tolerate_syntax_errors = o.tolerate_syntax_errors;
    let h = history::history(&req, &o.head, &o.window)?;
    let scope = snapshot::normalize_source(&o.source)?;
    let tree = Tree::new(&o.repo, &h.sha)?;
    let (mut records, skipped) = functions::collect(&tree.dir, &scope, &h);
    let (report, provenance) = gather_coverage(o, &h.sha, &tree)?;
    match &report {
        Some(r) => functions::attach_coverage(&mut records, r, &tree.dir),
        None if provenance["error"].is_string() => {
            for r in &mut records {
                r.coverage = Coverage::RunFailed;
            }
        }
        None => {}
    }
    functions::rank(&mut records);
    let sampled = match &o.mutation {
        Some(m) => Some(sample(o, m, &h.sha, &tree, report.as_ref(), &mut records)?),
        None => None,
    };
    let coverage = report.as_ref().map_or(provenance.clone(), |r| {
        coverage_summary(r, &records, provenance)
    });
    Ok(render(o, &h, &scope, &records, &skipped, coverage, sampled))
}

/// The coverage report, if any, and where it came from (`null` when none
/// was asked for). A report file that cannot be read is an error; a
/// coverage run that fails is recorded, and its functions stay unknown.
fn gather_coverage(o: &Options, sha: &str, tree: &Tree) -> Result<(Option<Resolved>, Value)> {
    match &o.coverage {
        None => Ok((None, Value::Null)),
        Some(CoverageSource::File(path)) => {
            let report = load_coverage(path, &o.repo, &tree.files)?;
            Ok((Some(report), json!({"source": "file", "path": path})))
        }
        Some(CoverageSource::Run(run)) => {
            let runner = o.execution.runner(&o.repo, sha);
            let (report, record) = covrun::measure(&runner, run, &tree.files);
            Ok(match report {
                Ok(r) => (Some(r), json!({"source": "run", "run": record})),
                Err(e) => (None, json!({"source": "run", "run": record, "error": e})),
            })
        }
    }
}

/// Choose targets among the most exposed functions with something to
/// mutate, run them, and record each function's outcome.
fn sample(
    o: &Options,
    m: &MutationOptions,
    sha: &str,
    tree: &Tree,
    report: Option<&Resolved>,
    records: &mut [Record],
) -> Result<Value> {
    let files: BTreeSet<&str> = records.iter().map(|r| r.file.as_str()).collect();
    let files: Vec<&str> = files.into_iter().collect();
    let by_file = functions::file_mutants(&tree.dir, &files);
    let mut targets = Vec::new();
    for r in records.iter_mut() {
        let all = by_file.get(&r.file).map_or(&[][..], Vec::as_slice);
        let chosen = functions::choose(r, all, report, m.per_function);
        r.mutation = if chosen.is_empty() {
            Mutation::NoMutants
        } else if targets.len() >= m.functions {
            Mutation::NotSelected
        } else {
            targets.push(mutation::Target {
                key: r.key(),
                mutants: chosen,
            });
            // replaced by its tally below, which exists only if the
            // unmutated tests passed
            Mutation::BaselineFailed
        };
    }
    let plan = mutation::Plan {
        runner: o.execution.runner(&o.repo, sha),
        command: &m.command,
    };
    let results = mutation::run(&plan, &targets)?;
    for r in records.iter_mut() {
        if let Some(t) = results.tallies.get(&r.key()) {
            r.mutation = Mutation::Tally(t.clone());
        }
    }
    let mutants: usize = targets.iter().map(|t| t.mutants.len()).sum();
    Ok(
        json!({"command": m.command, "functions": targets.len(), "mutants": mutants,
              "baseline": results.baseline, "isolation": results.isolation}),
    )
}

fn coverage_summary(report: &Resolved, records: &[Record], mut provenance: Value) -> Value {
    let scope_files: BTreeSet<&str> = records.iter().map(|r| r.file.as_str()).collect();
    let in_report = scope_files
        .iter()
        .filter(|f| report.files.contains_key(**f))
        .count();
    let sample = |v: &[String]| v.iter().take(10).cloned().collect::<Vec<_>>();
    let summary = json!({"format": report.format, "scope_files": scope_files.len(),
           "scope_files_in_report": in_report, "placed": report.files.len(),
           "unmatched": report.unmatched.len(), "unmatched_sample": sample(&report.unmatched),
           "ambiguous": report.ambiguous.len(), "ambiguous_sample": sample(&report.ambiguous)});
    if let (Some(p), Value::Object(s)) = (provenance.as_object_mut(), summary) {
        p.extend(s);
    }
    provenance
}

fn counts(records: &[Record]) -> (Value, Value) {
    let mut levels: BTreeMap<&str, usize> = BTreeMap::new();
    let mut delegation: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records {
        let level = findings::readiness_of(r);
        *levels.entry(level.as_str()).or_default() += 1;
        *delegation.entry(level.delegation()).or_default() += 1;
    }
    (json!(levels), json!(delegation))
}

fn render(
    o: &Options,
    h: &History,
    scope: &str,
    records: &[Record],
    skipped: &[String],
    coverage: Value,
    sampled: Option<Value>,
) -> Value {
    let ctx = findings::Context::of(records);
    let files: BTreeSet<&str> = records.iter().map(|r| r.file.as_str()).collect();
    let (levels, delegation) = counts(records);
    let top = if o.top == 0 { records.len() } else { o.top };
    json!({
        "schema": SCHEMA,
        "head": h.sha,
        "source": scope,
        "measurement_profile": h.measurement["measurement_profile"],
        "window_days": o.window.days,
        "summary": {"files": files.len(), "functions": records.len(), "commits": h.commits.len(),
                    "readiness": levels, "delegation": delegation},
        "coverage": coverage,
        "mutation": sampled,
        "skipped_files": skipped,
        "functions": records.iter().take(top).map(|r| findings::entry(r, &ctx)).collect::<Vec<_>>(),
        "change_coupling": h.coupling.iter().take(10).collect::<Vec<_>>(),
    })
}
