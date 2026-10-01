//! `emi trend`: raw burden totals over a stretch of first-parent history,
//! one row per measured commit, and a summary that splits the burden change
//! into "more code" and "denser code". Burden grows with size, so density is
//! only ever shown inside that split, next to the totals.

mod summary;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;
use emi_diff::Request;
use emi_diff::snapshot::resolve_ref;
use emi_scan::Probe;
use rayon::prelude::*;
use serde_json::{Map, Value, json};

use crate::{Language, ProfileArg};

#[derive(Args)]
pub struct TrendArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Product scope subdirectory.
    #[arg(long, default_value = "src")]
    source: String,
    /// Start of the walk, excluded: a ref, or a date (YYYY-MM-DD, UTC
    /// midnight) after which commits are measured.
    #[arg(long, value_name = "REF|YYYY-MM-DD")]
    since: String,
    /// End of the walk, included.
    #[arg(long, default_value = "HEAD", value_name = "REF")]
    until: String,
    /// Measure every Nth commit (the first and last are always measured).
    #[arg(long, default_value_t = 1, value_name = "N")]
    step: usize,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    #[arg(long, value_enum, default_value_t = ProfileArg::Parity)]
    profile: ProfileArg,
    /// Record unparseable files under a row's unmeasured list instead of
    /// leaving the commit unverified.
    #[arg(long)]
    tolerate_syntax_errors: bool,
    /// JSON lines (one per commit, then the summary) or a Markdown table.
    #[arg(long, default_value = "jsonl", value_parser = ["jsonl", "md"])]
    format: String,
    /// Write the output here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

/// One first-parent commit of the walk.
pub struct Commit {
    pub sha: String,
    pub date: String,
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// A commit date with a numeric UTC offset, as every other timestamp the
/// tool writes has it. Git's strict ISO format writes `Z` for a zero
/// offset in current versions and `+00:00` in older ones; the same instant
/// must not serialise two ways depending on the installed git.
fn numeric_offset(date: &str) -> String {
    match date.strip_suffix('Z') {
        Some(stem) => format!("{stem}+00:00"),
        None => date.to_owned(),
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The first parent of `sha`, or `None` for a root commit.
fn first_parent(repo: &Path, sha: &str) -> Result<Option<String>, String> {
    let out = emi_git::run(repo, &["rev-list", "--parents", "-n", "1", sha])?;
    Ok(lines(&out)
        .first()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_owned))
}

/// The commits after `since` up to `until`, oldest first, and the commit
/// the first of them is compared against (the resolved `since` ref, or the
/// first commit's parent when `since` is a date).
pub fn walk(
    repo: &Path,
    since: &str,
    until: &str,
) -> Result<(Option<String>, Vec<Commit>), String> {
    let until = resolve_ref(repo, until)?;
    let (base, since_option, range) = if is_date(since) {
        let option = format!("--since={since}T00:00:00+00:00");
        (None, Some(option), until.clone())
    } else {
        let base = resolve_ref(repo, since)?;
        let range = format!("{base}..{until}");
        (Some(base), None, range)
    };
    let mut args = vec!["log", "--first-parent", "--reverse", "--format=%H %cI"];
    args.extend(since_option.as_deref());
    args.extend(["--end-of-options", range.as_str()]);
    let commits: Vec<Commit> = lines(&emi_git::run(repo, &args)?)
        .iter()
        .filter_map(|l| l.split_once(' '))
        .map(|(sha, date)| Commit {
            sha: sha.to_owned(),
            date: numeric_offset(date),
        })
        .collect();
    let Some(first) = commits.first() else {
        return Err(format!(
            "no first-parent commits after {since} up to {until}"
        ));
    };
    let base = match base {
        Some(b) => Some(b),
        None => first_parent(repo, &first.sha)?,
    };
    Ok((base, commits))
}

/// Indices of every `step`th commit, always including the first and last.
pub fn select(n: usize, step: usize) -> Vec<usize> {
    let mut picked: Vec<usize> = (0..n).step_by(step.max(1)).collect();
    if n > 0 && picked.last() != Some(&(n - 1)) {
        picked.push(n - 1);
    }
    picked
}

/// Scope totals of one measurement. Burden is the measured mass (the known
/// part of every E, V and C interval); what it leaves out is listed in
/// `unmeasured`, never folded in as a guess.
struct Totals {
    sloc: u64,
    burden: f64,
    files: usize,
    clones: usize,
    unmeasured: Vec<Value>,
}

fn items<'a>(m: &'a Value, key: &str) -> &'a [Value] {
    m.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn totals(measurement: Option<&Value>) -> Totals {
    let empty = json!({});
    let m = measurement.unwrap_or(&empty);
    let components = items(m, "components");
    let mut masses = Vec::new();
    let mut unmeasured = Vec::new();
    for c in components {
        for dim in ["E", "V", "C"] {
            let [lo, hi] = crate::report::bounds(c, dim);
            masses.push(lo);
            if hi > lo {
                unmeasured.push(json!({"kind": "burden", "file": c["id"], "dimension": dim,
                                       "unknown_sloc": (hi - lo).round() as u64}));
            }
        }
    }
    for e in items(m, "errors") {
        unmeasured.push(json!({"kind": "file", "file": e["file"], "error": e["error"]}));
    }
    Totals {
        sloc: components.iter().filter_map(|c| c["sloc"].as_u64()).sum(),
        burden: emi_ledger::pyfmt::fsum(masses),
        files: components.len(),
        clones: items(m, "clone_groups").len(),
        unmeasured,
    }
}

/// Guardrail warnings the step from `base` to `head` introduced, counted by
/// family (`tests`, `suppress`, ...), with the default policy. A check the
/// guard could not run (`*.unverified`) is not a count: it goes to
/// `unmeasured`, as does a step with no base or a guard that failed.
fn guard_step(repo: &Path, base: Option<&str>, head: &str) -> (Value, Vec<Value>) {
    let Some(base) = base else {
        return (
            Value::Null,
            vec![json!({"kind": "guard", "error": "root commit: no parent to compare against"})],
        );
    };
    let report = match emi_guard::guard(repo, base, Some(head), &emi_guard::Policy::default()) {
        Ok(r) => r,
        Err(e) => return (Value::Null, vec![json!({"kind": "guard", "error": e})]),
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut unmeasured = Vec::new();
    for v in report
        .violations
        .iter()
        .filter(|v| v.tier != emi_guard::Tier::Off)
    {
        if v.id.ends_with(".unverified") {
            unmeasured
                .push(json!({"kind": "guard", "id": v.id, "file": v.path, "error": v.evidence}));
        } else {
            let family = v.id.split('.').next().unwrap_or(v.id);
            *counts.entry(family).or_default() += 1;
        }
    }
    (json!(counts), unmeasured)
}

/// One row: the commit's totals, or `state: unverified` with the error when
/// it could not be measured. The guard step is reported either way.
fn row(req: &Request, base: Option<&str>, commit: &Commit) -> Value {
    let (guard, mut unmeasured) = guard_step(&req.repo, base, &commit.sha);
    let mut row = Map::new();
    row.insert("sha".into(), json!(commit.sha));
    row.insert("date".into(), json!(commit.date));
    match emi_diff::change::measure_ref(req, &commit.sha) {
        Ok((_, measurement)) => {
            let t = totals(measurement.as_ref());
            let mut all = t.unmeasured;
            all.append(&mut unmeasured);
            for (k, v) in [
                ("state", json!("measured")),
                ("sloc", json!(t.sloc)),
                ("burden", json!(t.burden)),
                ("files", json!(t.files)),
                ("guard_warnings", guard),
                ("clones", json!(t.clones)),
                ("unmeasured", json!(all)),
            ] {
                row.insert(k.into(), v);
            }
        }
        Err(e) => {
            for (k, v) in [
                ("state", json!("unverified")),
                ("error", json!(e)),
                ("guard_warnings", guard),
                ("unmeasured", json!(unmeasured)),
            ] {
                row.insert(k.into(), v);
            }
        }
    }
    Value::Object(row)
}

/// Measure the selected commits (in parallel; each has its own scratch
/// snapshot), each guard step running from the previous selected commit.
pub fn rows(req: &Request, base: Option<String>, commits: &[Commit], step: usize) -> Vec<Value> {
    let picked: Vec<&Commit> = select(commits.len(), step)
        .into_iter()
        .map(|i| &commits[i])
        .collect();
    let bases: Vec<Option<String>> = std::iter::once(base)
        .chain(picked.iter().map(|c| Some(c.sha.clone())))
        .collect();
    picked
        .par_iter()
        .zip(bases.par_iter())
        .map(|(commit, base)| row(req, base.as_deref(), commit))
        .collect()
}

fn request(args: &TrendArgs) -> Request {
    let probe: Probe = args.probe.into();
    let mut req = Request::new(&args.repo, &args.source, "HEAD", "HEAD", probe);
    req.tolerate_syntax_errors = args.tolerate_syntax_errors;
    req.profile = args.profile.into();
    req
}

pub fn run(args: &TrendArgs) -> anyhow::Result<u8> {
    if args.step == 0 {
        anyhow::bail!("--step must be at least 1");
    }
    let (base, commits) = walk(&args.repo, &args.since, &args.until).map_err(|e| {
        anyhow::anyhow!(
            "emi trend --repo {} --since {} --until {}: {e}",
            args.repo.display(),
            args.since,
            args.until
        )
    })?;
    let req = request(args);
    let rows = rows(&req, base, &commits, args.step);
    let profile = emi_scan::Profile::from(args.profile).name();
    let summary = summary::summary(&rows, profile);
    let text = if args.format == "md" {
        let source =
            emi_diff::snapshot::normalize_source(&args.source).map_err(anyhow::Error::msg)?;
        summary::markdown(&rows, &summary, &source)
    } else {
        let mut out: Vec<String> = rows.iter().map(emi_ledger::pyfmt::dumps_line).collect();
        out.push(emi_ledger::pyfmt::dumps_line(&json!({"summary": summary})));
        out.join("\n") + "\n"
    };
    match &args.output {
        Some(path) => std::fs::write(path, text)?,
        None => write!(std::io::stdout().lock(), "{text}")?,
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_keeps_first_and_last() {
        assert_eq!(select(5, 1), [0, 1, 2, 3, 4]);
        assert_eq!(select(5, 2), [0, 2, 4]);
        assert_eq!(select(6, 2), [0, 2, 4, 5]);
        assert_eq!(select(6, 10), [0, 5]);
        assert_eq!(select(1, 3), [0]);
        assert!(select(0, 1).is_empty());
    }

    #[test]
    fn a_commit_date_always_carries_a_numeric_offset() {
        assert_eq!(
            numeric_offset("2021-01-01T12:00:00Z"),
            "2021-01-01T12:00:00+00:00"
        );
        for kept in ["2021-01-01T12:00:00+00:00", "2021-01-01T12:00:00-05:30"] {
            assert_eq!(numeric_offset(kept), kept);
        }
    }

    #[test]
    fn dates_are_recognised_strictly() {
        assert!(is_date("2026-01-31"));
        for s in ["main", "2026-1-31", "2026-01-31x", "abcd-ef-gh", "HEAD~3"] {
            assert!(!is_date(s), "{s}");
        }
    }

    #[test]
    fn partial_evidence_is_listed_not_counted() {
        let m = json!({"components": [
            {"id": "a.py", "sloc": 10, "burden": {"E": 2.0, "V": 1, "C": {"known_mass": 0.5, "unknown_sloc": 3}}},
            {"id": "b.py", "sloc": 4, "burden": {"E": 1.0, "V": 0, "C": null}}],
            "clone_groups": [[{"file": "a.py", "lines": [1]}, {"file": "b.py", "lines": [1]}]],
            "errors": [{"file": "c.py", "error": "SyntaxError"}]});
        let t = totals(Some(&m));
        assert_eq!((t.sloc, t.files, t.clones), (14, 2, 1));
        assert!((t.burden - 4.5).abs() < 1e-12, "{}", t.burden);
        let kinds: Vec<String> = t
            .unmeasured
            .iter()
            .map(|u| format!("{}:{}", u["file"], u["dimension"]))
            .collect();
        assert_eq!(kinds, [r#""a.py":"C""#, r#""b.py":"C""#, r#""c.py":null"#]);
        assert_eq!(t.unmeasured[1]["unknown_sloc"], json!(4));
    }
}
