//! Behavioural drift: for the Python functions a change modifies, the
//! inputs the base commit's tests pass them, replayed against the old and
//! the new code. A drift is an input where the two answer differently
//! (another return value, another exception type, or a return where there
//! was a raise). Passing tests do not show this; the report does, input by
//! input (docs/drift.md).
//!
//! Every target, input and call that could not be followed through is
//! counted by reason, and a run that did not happen, or only partly, is
//! `unverified`, never "no drift".
//!
//! Where the commands run is the caller's: `emi drift` runs them in copies
//! of the two commits, a supervised task in workspaces prepared like its
//! checks'. Both go through a [`Side`].

mod compare;
mod python;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::functions::{ChangeKind, ChangedFn, changed_functions};
use emi_exec::Outcome;
use serde_json::{Value, json};

use self::compare::Tally;
use self::python::{Harvest, Recorded, Replay, ReplaySettings, Target};
use crate::Result;
use crate::sandbox::{Copy, Runner};

pub const NOTE: &str = "inputs come from the test run on the base commit; drift on inputs the tests never pass is not seen";
const LIMITS: &str = "Python only; module-level functions, static methods and class methods are replayed; instance methods, nested and async functions are counted, not replayed; pinned is not measured yet";

/// A tree the commands of one side run in; removed when dropped.
pub trait Tree {
    fn dir(&self) -> &Path;
}

/// One side of the comparison (the base or the head): fresh trees of it,
/// and how a command runs in one.
pub trait Side {
    type Tree: Tree;
    /// A fresh tree (`tag` keeps concurrent ones apart).
    fn tree(&self, tag: &str) -> Result<Self::Tree>;
    /// Run `argv` in `tree` with exactly `env` (built from [`Side::env`]),
    /// bounded by `timeout`.
    fn run(
        &self,
        tree: &Self::Tree,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Outcome;
    /// The environment every command of this side starts from.
    fn env(&self) -> &[(String, String)];
    /// Upper bound for a test command and for one side's replay.
    fn timeout(&self) -> Duration;
}

impl Tree for Copy {
    fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Side for Runner<'_> {
    type Tree = Copy;

    fn tree(&self, tag: &str) -> Result<Copy> {
        self.copy(tag)
    }

    fn run(
        &self,
        tree: &Copy,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Outcome {
        Runner { env, ..*self }.run(tree, argv, timeout)
    }

    fn env(&self) -> &[(String, String)] {
        self.env
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// What to compare and where each side runs.
pub struct Request<'a, S: Side> {
    /// The repository the changed functions are read from.
    pub repo: &'a Path,
    /// Commit shas.
    pub base: String,
    pub head: String,
    /// Test commands (argv, no shell), run in turn on the base side; their
    /// Python processes supply the inputs.
    pub commands: &'a [Vec<String>],
    pub call_timeout_s: f64,
    pub base_side: &'a S,
    pub head_side: &'a S,
    /// Inputs to generate per function from its annotations, besides the
    /// recorded ones; 0 generates none.
    pub generated_examples: u32,
    /// Directories the project's modules are imported from, repository
    /// relative, for a function the tests never imported.
    pub module_roots: &'a [String],
}

/// A side whose trees are copies of `runner.rev` read from `objects`: a
/// clean view that also holds a frozen working tree.
pub struct InView<'a> {
    pub runner: Runner<'a>,
    pub objects: &'a Path,
}

impl Side for InView<'_> {
    type Tree = Copy;

    fn tree(&self, tag: &str) -> Result<Copy> {
        self.runner.copy_of(tag, self.objects, self.runner.rev)
    }

    fn run(
        &self,
        tree: &Copy,
        argv: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Outcome {
        Runner { env, ..self.runner }.run(tree, argv, timeout)
    }

    fn env(&self) -> &[(String, String)] {
        self.runner.env
    }

    fn timeout(&self) -> Duration {
        self.runner.timeout
    }
}

#[derive(Default)]
struct Counts {
    targets: u64,
    compared: u64,
    with_drift: u64,
    skipped_nested: u64,
    skipped_method: u64,
    skipped_async: u64,
    added_or_removed: u64,
    unresolved: u64,
    no_inputs: u64,
    unrecordable: u64,
    timeouts: u64,
    import_failures: u64,
    unencodable_result: u64,
    unstable: u64,
    not_replayed: u64,
    generated: u64,
}

impl Counts {
    fn to_json(&self) -> Value {
        json!({
            "targets": self.targets, "compared": self.compared, "with_drift": self.with_drift,
            "skipped_nested": self.skipped_nested, "skipped_method": self.skipped_method,
            "skipped_async": self.skipped_async, "added_or_removed": self.added_or_removed,
            "unresolved": self.unresolved, "no_inputs": self.no_inputs,
            "unrecordable": self.unrecordable, "timeouts": self.timeouts,
            "import_failures": self.import_failures,
            "unencodable_result": self.unencodable_result, "unstable": self.unstable,
            "not_replayed": self.not_replayed, "generated": self.generated,
        })
    }
}

/// The report under construction.
struct Report {
    base: String,
    head: String,
    /// Why the comparison is incomplete; any entry makes it `unverified`.
    reasons: Vec<String>,
    /// Keyed by target id, so they list in source order.
    functions: BTreeMap<usize, Value>,
    skipped: Vec<Value>,
    counts: Counts,
}

impl Report {
    fn finish(self) -> Value {
        let state = if !self.reasons.is_empty() {
            "unverified"
        } else if self.counts.compared == 0 {
            "nothing_to_compare"
        } else {
            "measured"
        };
        json!({
            "state": state,
            "reasons": self.reasons,
            "base": self.base,
            "head": self.head,
            "functions": self.functions.into_values().collect::<Vec<_>>(),
            "skipped": self.skipped,
            "counts": self.counts.to_json(),
            "limits": LIMITS,
            "note": NOTE,
        })
    }

    fn skip(&mut self, file: &str, name: &str, reason: &str) {
        self.skipped
            .push(json!({"file": file, "name": name, "reason": reason}));
    }
}

/// Whether a test would notice the drift. Answering it means reverting the
/// function to its base version on the head commit and rerunning the tests;
/// until that exists the answer is unknown, never a guess.
fn pinned(_file: &str, _qualname: &str) -> &'static str {
    "unknown"
}

/// The modified Python functions; added and removed ones are counted, and
/// Python files that did not parse leave the comparison incomplete.
fn modified(report: &mut Report, repo: &Path) -> Result<Vec<ChangedFn>> {
    let changed = changed_functions(repo, &report.base, &report.head)?;
    for file in changed.unparsed.iter().filter(|f| f.ends_with(".py")) {
        report.reasons.push(format!(
            "{file} did not parse on one side; its functions are unknown"
        ));
    }
    let python = changed.functions.into_iter();
    let (modified, other): (Vec<_>, Vec<_>) = python.partition(|f| f.kind == ChangeKind::Modified);
    report.counts.added_or_removed = other.len() as u64;
    report.counts.targets = modified.len() as u64;
    Ok(modified)
}

/// The replayable targets; the rest are counted by kind.
fn targets<S: Side>(
    report: &mut Report,
    side: &S,
    tree: &S::Tree,
    modified: &[ChangedFn],
) -> Result<Vec<Target>> {
    let asked: Vec<(String, String, u32)> = modified
        .iter()
        .map(|f| (f.file.clone(), f.name.clone(), f.old.map_or(0, |s| s.0)))
        .collect();
    let kinds = python::classify(side, tree, &asked)?;
    let mut out = Vec::new();
    for (id, (f, (kind, qualname))) in modified.iter().zip(kinds).enumerate() {
        let name = qualname.as_deref().unwrap_or(&f.name);
        let counter = match kind.as_str() {
            "function" | "static" | "class" => None,
            "instance" => Some(&mut report.counts.skipped_method),
            "nested" => Some(&mut report.counts.skipped_nested),
            "async" => Some(&mut report.counts.skipped_async),
            _ => Some(&mut report.counts.unresolved),
        };
        if let Some(n) = counter {
            *n += 1;
            let reason = if kind == "instance" {
                "instance_method"
            } else {
                &kind
            };
            report.skip(&f.file, name, reason);
            if kind == "not_found" {
                report.reasons.push(format!(
                    "{} {name}: no definition at line {} on the base side",
                    f.file,
                    f.old.map_or(0, |s| s.0)
                ));
            }
            continue;
        }
        out.push(Target {
            id,
            file: f.file.clone(),
            qualname: name.to_owned(),
            kind,
        });
    }
    Ok(out)
}

/// The drift report for `r.base..r.head`. `Err` only when the change or a
/// tree could not be read at all; everything that went wrong on the way
/// is in the report, which is then `unverified`.
pub fn measure<S: Side>(r: &Request<S>) -> Result<Value> {
    let mut report = Report {
        base: r.base.clone(),
        head: r.head.clone(),
        reasons: Vec::new(),
        functions: BTreeMap::new(),
        skipped: Vec::new(),
        counts: Counts::default(),
    };
    let modified = modified(&mut report, r.repo)?;
    if modified.is_empty() {
        return Ok(report.finish());
    }
    let tree = r.base_side.tree("drift-harvest")?;
    let found = targets(&mut report, r.base_side, &tree, &modified);
    let targets = match found {
        Ok(t) if t.is_empty() => return Ok(report.finish()),
        Ok(t) => t,
        Err(e) => {
            report.reasons.push(e);
            return Ok(report.finish());
        }
    };
    match python::harvest(r.base_side, &tree, r.commands, &targets) {
        Ok(mut h) => {
            if r.generated_examples > 0 {
                augment(&mut report, r, (&tree, &targets), &mut h)?;
            }
            replay_all(&mut report, r, &tree, &targets, &h)?;
        }
        Err(e) => report.reasons.push(e),
    }
    Ok(report.finish())
}

/// The module a file is imported as from one of `roots` (repository
/// relative), and that root; `None` when no root holds it.
fn module_of(file: &str, roots: &[String]) -> Option<(String, Option<String>)> {
    roots
        .iter()
        .chain(std::iter::once(&".".to_owned()))
        .find_map(|root| {
            let rest = if root == "." {
                Some(file)
            } else {
                file.strip_prefix(root.as_str())?.strip_prefix('/')
            }?;
            let stem = rest.strip_suffix(".py")?;
            let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
            let module = stem.replace('/', ".");
            let root = (root != ".").then(|| root.clone());
            Some((module, root))
        })
}

/// Add inputs generated from each target's annotations to what the tests
/// recorded: a function the tests never call still meets its declared
/// types. Generation runs on the base side; a function it cannot generate
/// for keeps its recorded inputs alone.
fn augment<S: Side>(
    report: &mut Report,
    r: &Request<S>,
    (harvested, targets): (&S::Tree, &[Target]),
    h: &mut Harvest,
) -> Result<()> {
    let modules: Vec<(usize, String, Option<String>)> = targets
        .iter()
        .filter_map(|t| {
            let known = h.recorded.get(&t.id).and_then(|rec| rec.module.clone());
            known
                .or_else(|| module_of(&t.file, r.module_roots))
                .map(|(m, root)| (t.id, m, root))
        })
        .collect();
    let items: Vec<Replay> = modules
        .iter()
        .filter_map(|(id, module, root)| {
            let target = targets.iter().find(|t| t.id == *id)?;
            Some(Replay {
                target,
                module,
                root: root.as_deref(),
                inputs: &[],
            })
        })
        .collect();
    if items.is_empty() {
        return Ok(());
    }
    let tree = r.base_side.tree("drift-generate")?;
    let python = python::interpreter(h, harvested.dir(), tree.dir());
    let generated = match python::generate(
        r.base_side,
        &tree,
        &items,
        (&python, &h.sys_path, r.generated_examples),
    ) {
        Ok(g) => g,
        Err(e) => {
            report
                .reasons
                .push(format!("inputs could not be generated: {e}"));
            return Ok(());
        }
    };
    for (id, module, root) in modules {
        let Some(inputs) = generated.inputs.get(&id) else {
            continue;
        };
        report.counts.generated += inputs.len() as u64;
        let rec = h.recorded.entry(id).or_default();
        rec.module.get_or_insert((module, root));
        for input in inputs {
            if !rec.inputs.contains(input) {
                rec.inputs.push(input.clone());
            }
        }
    }
    Ok(())
}

/// The targets the tests passed inputs to; the others get their entry now.
fn replayable<'a>(
    report: &mut Report,
    targets: &'a [Target],
    harvest: &'a Harvest,
) -> Vec<Replay<'a>> {
    let mut out = Vec::new();
    for t in targets {
        let rec = harvest.recorded.get(&t.id);
        report.counts.unrecordable += rec.map_or(0, |r| r.unrecordable);
        if let Some(r) = rec.filter(|r| r.missing) {
            report.counts.unresolved += 1;
            report.reasons.push(format!(
                "{} {}: the test run imported {} but the recorder did not find the function in it",
                t.file,
                t.qualname,
                r.module.as_ref().map_or("the module", |m| m.0.as_str())
            ));
            report.skip(&t.file, &t.qualname, "not_found");
            continue;
        }
        match rec {
            Some(Recorded {
                module: Some((module, root)),
                inputs,
                ..
            }) if !inputs.is_empty() => out.push(Replay {
                target: t,
                module,
                root: root.as_deref(),
                inputs,
            }),
            _ => {
                report.counts.no_inputs += 1;
                report
                    .functions
                    .insert(t.id, entry(t, "no_inputs", 0, &Tally::default()));
            }
        }
    }
    out
}

fn entry(t: &Target, status: &str, inputs: usize, tally: &Tally) -> Value {
    json!({
        "file": t.file,
        "name": t.qualname,
        "kind": t.kind,
        "status": status,
        "inputs": inputs,
        "compared": tally.compared,
        "drifts": tally.drifts,
        "more_drifts": tally.more_drifts,
        "unstable": tally.unstable,
        "pinned": pinned(&t.file, &t.qualname),
    })
}

fn replay_all<S: Side>(
    report: &mut Report,
    r: &Request<S>,
    harvested: &S::Tree,
    targets: &[Target],
    harvest: &Harvest,
) -> Result<()> {
    if let Some(why) = &harvest.incomplete {
        report
            .reasons
            .push(format!("{why}; the recorded inputs may be incomplete"));
    }
    let items = replayable(report, targets, harvest);
    if items.is_empty() {
        return Ok(());
    }
    // fresh trees: the test run may have changed files in its own
    let old = r.base_side.tree("drift-base")?;
    let new = r.head_side.tree("drift-head")?;
    let pythons = [old.dir(), new.dir()].map(|c| python::interpreter(harvest, harvested.dir(), c));
    let settings = |python, repeat| ReplaySettings {
        python,
        sys_path: &harvest.sys_path,
        call_timeout_s: r.call_timeout_s,
        repeat,
    };
    let base = python::replay(r.base_side, &old, &items, &settings(&pythons[0], 2));
    let head = python::replay(r.head_side, &new, &items, &settings(&pythons[1], 1));
    match (base, head) {
        (Ok(b), Ok(h)) => {
            for r in &items {
                let id = &r.target.id;
                let at = |m: &BTreeMap<usize, Value>| m.get(id).cloned().unwrap_or(Value::Null);
                record(report, r, &at(&b), &at(&h));
            }
        }
        (b, h) => {
            for (side, e) in [("base", b.err()), ("head", h.err())] {
                report
                    .reasons
                    .extend(e.map(|e| format!("replay on {side}: {e}")));
            }
            for r in &items {
                report.counts.not_replayed += r.inputs.len() as u64;
                let e = entry(r.target, "not_replayed", r.inputs.len(), &Tally::default());
                report.functions.insert(r.target.id, e);
            }
        }
    }
    Ok(())
}

fn outcomes(v: &Value) -> Option<&[Value]> {
    v["outcomes"].as_array().map(Vec::as_slice)
}

/// Fold one target's two replays into the report.
fn record(report: &mut Report, r: &Replay, base: &Value, head: &Value) {
    let t = r.target;
    let failed: Vec<String> = [("base", base), ("head", head)]
        .into_iter()
        .filter_map(|(side, v)| v["import_error"].as_str().map(|e| format!("{side}: {e}")))
        .collect();
    let (Some(b), Some(h), true) = (outcomes(base), outcomes(head), failed.is_empty()) else {
        report.counts.import_failures += failed.len().max(1) as u64;
        report.reasons.push(format!(
            "{} {} could not be imported ({})",
            t.file,
            t.qualname,
            failed.join("; ")
        ));
        let e = entry(t, "import_failed", r.inputs.len(), &Tally::default());
        report.functions.insert(t.id, e);
        return;
    };
    let tally = compare::compare(r.inputs, b, h);
    let c = &mut report.counts;
    c.timeouts += tally.timeouts;
    c.unencodable_result += tally.unencodable_result;
    c.unstable += tally.unstable;
    c.not_replayed += tally.not_replayed;
    c.compared += u64::from(tally.compared > 0);
    c.with_drift += u64::from(tally.drift_count() > 0);
    if tally.not_replayed > 0 {
        report.reasons.push(format!(
            "{} {}: {} inputs were not replayed in time",
            t.file, t.qualname, tally.not_replayed
        ));
    }
    let status = if tally.compared > 0 {
        "compared"
    } else {
        "not_compared"
    };
    report
        .functions
        .insert(t.id, entry(t, status, r.inputs.len(), &tally));
}
