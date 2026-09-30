//! `emi drift`: for the Python functions a change modifies, the inputs the
//! base commit's tests pass them, replayed against the old and the new
//! code. A drift is an input where the two answer differently (another
//! return value, another exception type, or a return where there was a
//! raise). Passing tests do not show this; the report does, input by input
//! (docs/drift.md).
//!
//! Every target, input and call that could not be followed through is
//! counted by reason, and a run that did not happen, or only partly, is
//! `unverified`, never "no drift".

mod compare;
mod python;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use emi_diff::functions::{ChangeKind, ChangedFn, changed_functions};
use emi_diff::snapshot::resolve_ref;
use emi_panel::Lang;
use emi_profile::Execution;
use emi_profile::sandbox::{Copy, Runner};
use serde_json::{Value, json};

use self::compare::Tally;
use self::python::{Harvest, Recorded, Replay, ReplaySettings, Target};

const NOTE: &str = "inputs come from the test run on the base commit; drift on inputs the tests never pass is not seen";
const LIMITS: &str = "Python only; module-level functions, static methods and class methods are replayed; instance methods, nested and async functions are counted, not replayed; pinned is not measured yet";

#[derive(Args)]
pub struct DriftArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// The revision before the change; its tests supply the inputs.
    #[arg(long)]
    base: String,
    /// The revision after the change.
    #[arg(long, default_value = "HEAD")]
    head: String,
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
    /// Ignored dependency directory linked into each copy (repeatable).
    #[arg(long)]
    link: Vec<String>,
    /// KEY=VALUE for the environment of the test run and the replays
    /// (repeatable).
    #[arg(long = "env")]
    env: Vec<String>,
    /// Upper bound for the test run and for each side's replay, in seconds.
    #[arg(long, default_value_t = 600)]
    timeout_s: u64,
    /// Upper bound for one replayed call, in seconds.
    #[arg(long, default_value_t = 2.0)]
    call_timeout_s: f64,
    /// Let the test run and the replays use the network.
    #[arg(long)]
    network: bool,
    /// Write here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Test command (argv, no shell) run on the base commit, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

/// What to compare and how the commands run.
pub struct Options {
    pub repo: PathBuf,
    pub base: String,
    pub head: String,
    pub command: Vec<String>,
    pub execution: Execution,
    pub call_timeout_s: f64,
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
            "not_replayed": self.not_replayed,
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
fn modified(report: &mut Report, repo: &std::path::Path) -> anyhow::Result<Vec<ChangedFn>> {
    let changed =
        changed_functions(repo, &report.base, &report.head).map_err(anyhow::Error::msg)?;
    for file in changed
        .unparsed
        .iter()
        .filter(|f| Lang::of_path(f) == Some(Lang::Python))
    {
        report.reasons.push(format!(
            "{file} did not parse on one side; its functions are unknown"
        ));
    }
    let python = changed
        .functions
        .into_iter()
        .filter(|f| f.lang == Lang::Python);
    let (modified, other): (Vec<_>, Vec<_>) = python.partition(|f| f.kind == ChangeKind::Modified);
    report.counts.added_or_removed = other.len() as u64;
    report.counts.targets = modified.len() as u64;
    Ok(modified)
}

/// The replayable targets; the rest are counted by kind.
fn targets(
    report: &mut Report,
    runner: &Runner,
    copy: &Copy,
    modified: &[ChangedFn],
) -> Result<Vec<Target>, String> {
    let asked: Vec<(String, String, u32)> = modified
        .iter()
        .map(|f| (f.file.clone(), f.name.clone(), f.old.map_or(0, |s| s.0)))
        .collect();
    let kinds = python::classify(runner, copy, &asked)?;
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

pub fn measure(o: &Options) -> anyhow::Result<Value> {
    let base = resolve_ref(&o.repo, &o.base).map_err(anyhow::Error::msg)?;
    let head = resolve_ref(&o.repo, &o.head).map_err(anyhow::Error::msg)?;
    let mut report = Report {
        base,
        head,
        reasons: Vec::new(),
        functions: BTreeMap::new(),
        skipped: Vec::new(),
        counts: Counts::default(),
    };
    let modified = modified(&mut report, &o.repo)?;
    if modified.is_empty() {
        return Ok(report.finish());
    }
    let (base, head) = (report.base.clone(), report.head.clone());
    let runner = |rev| Runner {
        repo: &o.repo,
        rev,
        link: &o.execution.link,
        env: &o.execution.env,
        timeout: o.execution.timeout,
        network: o.execution.network,
    };
    let sides = (runner(&base), runner(&head));
    let copy = sides.0.copy("drift-harvest").map_err(anyhow::Error::msg)?;
    let found = targets(&mut report, &sides.0, &copy, &modified);
    let targets = match found {
        Ok(t) if t.is_empty() => return Ok(report.finish()),
        Ok(t) => t,
        Err(e) => {
            report.reasons.push(e);
            return Ok(report.finish());
        }
    };
    match python::harvest(&sides.0, &copy, &o.command, &targets) {
        Ok(h) => replay_all(&mut report, o, &sides, &copy, &targets, &h)?,
        Err(e) => report.reasons.push(e),
    }
    Ok(report.finish())
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

type Sides<'a> = (Runner<'a>, Runner<'a>);

fn replay_all(
    report: &mut Report,
    o: &Options,
    sides: &Sides,
    harvested: &Copy,
    targets: &[Target],
    harvest: &Harvest,
) -> anyhow::Result<()> {
    if let Some(why) = &harvest.incomplete {
        report
            .reasons
            .push(format!("{why}; the recorded inputs may be incomplete"));
    }
    let items = replayable(report, targets, harvest);
    if items.is_empty() {
        return Ok(());
    }
    // fresh copies: the test run may have changed files in its own
    let old = sides.0.copy("drift-base").map_err(anyhow::Error::msg)?;
    let new = sides.1.copy("drift-head").map_err(anyhow::Error::msg)?;
    let pythons = [&old, &new].map(|c| python::interpreter(harvest, harvested, c));
    let settings = |python, repeat| ReplaySettings {
        python,
        sys_path: &harvest.sys_path,
        call_timeout_s: o.call_timeout_s,
        repeat,
    };
    let base = python::replay(&sides.0, &old, &items, &settings(&pythons[0], 2));
    let head = python::replay(&sides.1, &new, &items, &settings(&pythons[1], 1));
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

fn options(a: &DriftArgs) -> anyhow::Result<Options> {
    if !(a.call_timeout_s.is_finite() && a.call_timeout_s > 0.0) {
        anyhow::bail!("--call-timeout-s must be a positive number of seconds");
    }
    Ok(Options {
        repo: a.repo.clone(),
        base: a.base.clone(),
        head: a.head.clone(),
        command: a.command.clone(),
        execution: Execution {
            link: a.link.clone(),
            env: crate::profilecli::env_pairs(&a.env)?,
            timeout: Duration::from_secs(a.timeout_s),
            network: a.network,
        },
        call_timeout_s: a.call_timeout_s,
    })
}

fn short(sha: &Value) -> &str {
    sha.as_str().map_or("?", |s| &s[..s.len().min(12)])
}

fn line(v: &Value) -> String {
    emi_ledger::pyfmt::dumps_line(v)
}

/// The report for a terminal: drifts first, then what was not compared.
pub fn render(r: &Value) -> String {
    let c = &r["counts"];
    let mut out = format!(
        "drift {}..{}: {}\n{} targets, {} compared, {} with drift\n",
        short(&r["base"]),
        short(&r["head"]),
        r["state"].as_str().unwrap_or("?"),
        c["targets"],
        c["compared"],
        c["with_drift"]
    );
    for f in r["functions"].as_array().into_iter().flatten() {
        out += &format!(
            "\n{} {} [{}]: {} inputs, {} compared\n",
            f["file"].as_str().unwrap_or("?"),
            f["name"].as_str().unwrap_or("?"),
            f["status"].as_str().unwrap_or("?"),
            f["inputs"],
            f["compared"]
        );
        for d in f["drifts"].as_array().into_iter().flatten() {
            out += &format!(
                "  input {}\n    old {}\n    new {}\n",
                line(&d["input"]),
                line(&d["old"]),
                line(&d["new"])
            );
        }
        if f["more_drifts"].as_u64().is_some_and(|n| n > 0) {
            out += &format!("  ... and {} more drifts\n", f["more_drifts"]);
        }
    }
    for s in r["skipped"].as_array().into_iter().flatten() {
        out += &format!(
            "skipped {} {}: {}\n",
            s["file"].as_str().unwrap_or("?"),
            s["name"].as_str().unwrap_or("?"),
            s["reason"].as_str().unwrap_or("?")
        );
    }
    for why in r["reasons"].as_array().into_iter().flatten() {
        out += &format!("unverified: {}\n", why.as_str().unwrap_or("?"));
    }
    out += &format!("counts: {}\n", line(c));
    out += &format!("{}\n", NOTE);
    out
}

pub fn run(a: &DriftArgs) -> anyhow::Result<u8> {
    let report = measure(&options(a)?)?;
    let text = if a.format == "json" {
        emi_ledger::pyfmt::dumps_pretty(&report) + "\n"
    } else {
        render(&report)
    };
    match &a.output {
        Some(p) => std::fs::write(p, text).with_context(|| format!("writing {}", p.display()))?,
        None => write!(std::io::stdout().lock(), "{text}")?,
    }
    Ok(0)
}
