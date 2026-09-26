//! `emi diff`: the change between two refs, optionally reviewed and gated.

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use emi_diff::{Request, build_change, snapshot};
use emi_scan::Probe;
use serde_json::{Map, Value};

use crate::{Language, gatecli};

#[derive(Args)]
pub struct DiffArgs {
    /// Git repository to diff.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Product scope subdirectory.
    #[arg(long, default_value = "src")]
    source: String,
    /// Base ref (commit, branch, tag).
    #[arg(long)]
    before: String,
    /// Changed ref (commit, branch, tag).
    #[arg(long)]
    after: String,
    /// Diff AFTER against merge-base(BEFORE, AFTER) instead of BEFORE.
    #[arg(long)]
    merge_base: bool,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    /// fnmatch glob against scope-relative paths to skip (repeatable).
    #[arg(long = "exclude")]
    exclude: Vec<String>,
    /// fnmatch glob to keep; only matching files are measured (repeatable).
    #[arg(long = "include")]
    include: Vec<String>,
    /// Record unparseable files under errors instead of aborting.
    #[arg(long)]
    tolerate_syntax_errors: bool,
    /// Accepted for compatibility; the engine needs no probe cache.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// Python: also respect the scope .gitignore.
    #[arg(long)]
    use_gitignore: bool,
    /// Python: treat test files as product source.
    #[arg(long)]
    no_test_detection: bool,
    /// Write JSON to a file instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[command(flatten)]
    gate: gatecli::GateArgs,
}

fn probe(language: Language) -> Probe {
    match language {
        Language::Python => Probe::Python,
        Language::Js => Probe::Js,
    }
}

fn probe_name(language: Language) -> &'static str {
    match language {
        Language::Python => "python",
        Language::Js => "js",
    }
}

/// Resolve both refs once, measure, and (with --guardrail) gate.
fn payload(args: &DiffArgs) -> Result<(Value, u8), String> {
    if matches!(args.probe, Language::Js) && args.cache_dir.is_some() {
        return Err("--cache-dir is not supported with --probe js (python probe only)".into());
    }
    // Pin the refs before measuring: the same immutable SHAs feed the
    // export and the trajectory row even if a branch moves mid-run.
    let before = snapshot::resolve_ref(&args.repo, &args.before)?;
    let after = snapshot::resolve_ref(&args.repo, &args.after)?;
    let before = if args.merge_base {
        snapshot::merge_base(&args.repo, &before, &after)?
    } else {
        before
    };
    let mut req = Request::new(&args.repo, &args.source, &before, &after, probe(args.probe));
    req.exclude.clone_from(&args.exclude);
    req.include.clone_from(&args.include);
    req.tolerate_syntax_errors = args.tolerate_syntax_errors;
    req.use_gitignore = args.use_gitignore;
    req.detect_tests = !args.no_test_detection;
    let change = build_change(&req)?;
    if !args.gate.guardrail {
        return Ok((change, 0));
    }
    let report = emi_ledger::review(&change)?;
    let mut record = Map::new();
    record.insert(
        "repo".into(),
        Value::from(args.repo.to_string_lossy().into_owned()),
    );
    record.insert("source".into(), Value::from(args.source.clone()));
    record.insert("before".into(), Value::from(before));
    record.insert("after".into(), Value::from(after));
    record.insert("before_ref".into(), Value::from(args.before.clone()));
    record.insert("after_ref".into(), Value::from(args.after.clone()));
    record.insert("probe".into(), Value::from(probe_name(args.probe)));
    gatecli::apply(report, &args.gate, record).map_err(|e| e.to_string())
}

pub fn run(args: &DiffArgs) -> anyhow::Result<u8> {
    args.gate.validate()?;
    let (out, code) = payload(args).map_err(|e| {
        anyhow::anyhow!(
            "emi diff --repo {} --source {} --before {} --after {}: {e}",
            args.repo.display(),
            args.source,
            args.before,
            args.after
        )
    })?;
    let text = emi_ledger::pyfmt::dumps_pretty(&out);
    match &args.output {
        Some(path) => std::fs::write(path, text)?,
        None => writeln!(std::io::stdout().lock(), "{text}")?,
    }
    Ok(code)
}

#[derive(Args)]
pub struct BacktestArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Product scope subdirectory.
    #[arg(long, default_value = "src")]
    source: String,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    /// History window: pairs the newest N scope commits.
    #[arg(long, default_value_t = 40)]
    commits: usize,
    /// Read history back from this ref.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// Proven-growth allowance.
    #[arg(long, default_value_t = 10.0, value_name = "UNITS")]
    allow_growth: f64,
    /// Headroom setting to evaluate (repeatable; default: the allowance).
    #[arg(long = "headroom", value_name = "UNITS")]
    headroom: Vec<f64>,
    /// Record unparseable files under errors instead of aborting.
    #[arg(long)]
    tolerate_syntax_errors: bool,
    /// Human table or machine JSON.
    #[arg(long, default_value = "table", value_parser = ["table", "json"])]
    format: String,
    /// Write the report to a file instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

pub fn backtest(args: &BacktestArgs) -> anyhow::Result<u8> {
    if args.commits < 2 {
        anyhow::bail!("--commits needs at least 2 to form one pair");
    }
    let bt = emi_diff::backtest::Backtest {
        repo: args.repo.clone(),
        source: args.source.clone(),
        probe: probe(args.probe),
        commits: args.commits,
        head: args.head.clone(),
        allowance: args.allow_growth,
        headrooms: if args.headroom.is_empty() {
            vec![args.allow_growth]
        } else {
            args.headroom.clone()
        },
        tolerate_syntax_errors: args.tolerate_syntax_errors,
    };
    let payload = emi_diff::backtest::run(&bt).map_err(anyhow::Error::msg)?;
    let text = if args.format == "json" {
        emi_ledger::pyfmt::dumps_pretty(&payload) + "\n"
    } else {
        emi_diff::backtest::table(&payload)
    };
    match &args.output {
        Some(path) => std::fs::write(path, text)?,
        None => write!(std::io::stdout().lock(), "{text}")?,
    }
    Ok(0)
}
