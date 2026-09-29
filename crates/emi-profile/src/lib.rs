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
pub mod findings;
pub mod functions;
pub mod history;
pub mod mutation;
pub mod readiness;
pub mod text;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_diff::{Request, snapshot};
use serde_json::{Value, json};

use crate::coverage::Resolved;
use crate::functions::{Mutation, Record};
use crate::history::{History, Window};

pub type Result<T> = std::result::Result<T, String>;

pub const SCHEMA: &str = "emi-profile-0.1";

/// What to profile and with which evidence.
pub struct Options {
    pub repo: PathBuf,
    pub source: String,
    pub probe: emi_scan::Probe,
    pub head: String,
    pub window: Window,
    /// A coverage report produced at the head commit.
    pub coverage: Option<PathBuf>,
    pub mutation: Option<MutationOptions>,
    /// Function entries in the output (all are counted in the summary).
    pub top: usize,
    pub tolerate_syntax_errors: bool,
}

/// Sampled mutation: which functions, how many mutants, how to test.
pub struct MutationOptions {
    pub command: Vec<String>,
    pub link: Vec<String>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    pub network: bool,
    /// The most exposed functions that get mutants.
    pub functions: usize,
    pub per_function: usize,
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
    let report = o
        .coverage
        .as_deref()
        .map(|p| load_coverage(p, &o.repo, &tree.files))
        .transpose()?;
    if let Some(r) = &report {
        functions::attach_coverage(&mut records, r, &tree.dir);
    }
    functions::rank(&mut records);
    let sampled = match &o.mutation {
        Some(m) => Some(sample(o, m, &h.sha, &tree, report.as_ref(), &mut records)?),
        None => None,
    };
    Ok(render(
        o,
        &h,
        &scope,
        &records,
        &skipped,
        report.as_ref(),
        sampled,
    ))
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
        repo: &o.repo,
        rev: sha,
        command: &m.command,
        link: &m.link,
        env: &m.env,
        timeout: m.timeout,
        network: m.network,
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

fn coverage_summary(report: &Resolved, records: &[Record]) -> Value {
    let scope_files: BTreeSet<&str> = records.iter().map(|r| r.file.as_str()).collect();
    let in_report = scope_files
        .iter()
        .filter(|f| report.files.contains_key(**f))
        .count();
    let sample = |v: &[String]| v.iter().take(10).cloned().collect::<Vec<_>>();
    json!({"format": report.format, "scope_files": scope_files.len(),
           "scope_files_in_report": in_report, "placed": report.files.len(),
           "unmatched": report.unmatched.len(), "unmatched_sample": sample(&report.unmatched),
           "ambiguous": report.ambiguous.len(), "ambiguous_sample": sample(&report.ambiguous)})
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
    report: Option<&Resolved>,
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
        "coverage": report.map(|r| coverage_summary(r, records)),
        "mutation": sampled,
        "skipped_files": skipped,
        "functions": records.iter().take(top).map(|r| findings::entry(r, &ctx)).collect::<Vec<_>>(),
        "change_coupling": h.coupling.iter().take(10).collect::<Vec<_>>(),
    })
}
