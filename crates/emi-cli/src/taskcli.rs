//! `emi task init|gate`: cumulative task gating (the oracle's legacy,
//! advisory path). `init` pins a baseline commit and the scope/policy;
//! every `gate` reviews baseline -> candidate under that pinned policy, so
//! a retry cannot reset its allowance by committing in pieces.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use emi_diff::{Request, build_change, snapshot};
use emi_ledger::pyfmt::{digest, dumps_pretty};
use serde_json::{Map, Value, json};

use crate::{Language, ProfileArg, gatecli};

const RECORD_VERSION: &str = "emi-task-0.1";

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Pin a task baseline record.
    Init(InitArgs),
    /// Gate baseline -> AFTER under the recorded policy.
    Gate(GateArgs),
}

#[derive(Args)]
pub struct InitArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "src")]
    source: String,
    /// Task baseline ref (resolved to a SHA and pinned).
    #[arg(long)]
    baseline: String,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    #[arg(long, value_enum, default_value_t = ProfileArg::Parity)]
    profile: ProfileArg,
    #[arg(long, default_value_t = 10.0, value_name = "UNITS")]
    allow_growth: f64,
    #[arg(long, value_name = "UNITS")]
    allow_headroom: Option<f64>,
    #[arg(long = "exclude")]
    exclude: Vec<String>,
    #[arg(long = "include")]
    include: Vec<String>,
    #[arg(long)]
    tolerate_syntax_errors: bool,
    #[arg(long)]
    use_gitignore: bool,
    #[arg(long)]
    no_test_detection: bool,
    /// Overwrite an existing task record.
    #[arg(long)]
    force: bool,
    /// Where to write the task record.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub struct GateArgs {
    /// Task record from `emi task init`.
    #[arg(long)]
    task: PathBuf,
    /// Candidate ref.
    #[arg(long)]
    after: String,
    /// Accepted for compatibility; the engine needs no probe cache.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    fail_on_warn: bool,
    #[arg(
        long,
        default_value_t = 5,
        value_name = "N",
        allow_negative_numbers = true
    )]
    max_evidence: i64,
    #[arg(long)]
    compact: bool,
    #[arg(long, value_name = "DIR")]
    log_trajectory: Option<PathBuf>,
    #[arg(long)]
    trajectory_strict: bool,
}

fn check_knob(value: f64, label: &str) -> anyhow::Result<f64> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        anyhow::bail!("{label} is out of range or has an invalid unit")
    }
}

fn init(a: &InitArgs) -> anyhow::Result<u8> {
    if !a.force && a.output.exists() {
        anyhow::bail!(
            "refusing to overwrite existing task record {} (pass --force to replace)",
            a.output.display()
        );
    }
    let sha = snapshot::resolve_ref(&a.repo, &a.baseline).map_err(anyhow::Error::msg)?;
    let allowance = check_knob(a.allow_growth, "allowance")?;
    let headroom = match a.allow_headroom {
        None => allowance,
        Some(h) => check_knob(h, "headroom")?,
    };
    let repo = std::fs::canonicalize(&a.repo)?
        .to_string_lossy()
        .into_owned();
    let probe = if matches!(a.probe, Language::Js) {
        "js"
    } else {
        "python"
    };
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    let mut record = json!({
        "version": RECORD_VERSION, "repo": repo, "source": a.source, "probe": probe,
        "baseline_ref": a.baseline, "baseline_sha": sha, "allowance": allowance, "headroom": headroom,
        "exclude": a.exclude, "include": a.include, "tolerate_syntax_errors": a.tolerate_syntax_errors,
        "use_gitignore": a.use_gitignore, "detect_tests": !a.no_test_detection,
        "measurement_contract_id": digest("measurement-contract", &json!({
            "probe": probe, "exclude": sorted(&a.exclude), "include": sorted(&a.include),
            "tolerate_syntax_errors": a.tolerate_syntax_errors, "use_gitignore": a.use_gitignore,
            "detect_tests": !a.no_test_detection})),
        "gate_policy_id": digest("gate-policy", &json!({
            "profile": emi_ledger::contracts::PROFILE, "allowance": allowance, "headroom": headroom,
            "version": "emi-gate-0.1"})),
    });
    if matches!(a.profile, ProfileArg::Precise) {
        // Parity records carry no profile key (they are the oracle's).
        record["measurement"] = json!("precise");
    }
    record["created"] = json!(emi_diff::trajectory::utc_now_iso());
    std::fs::write(&a.output, dumps_pretty(&record) + "\n")?;
    Ok(0)
}

fn load(path: &std::path::Path) -> anyhow::Result<Map<String, Value>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read task record {}: {e}", path.display()))?;
    let value = emi_ledger::strict::loads(&text)
        .map_err(|e| anyhow::anyhow!("cannot parse task record {} as JSON: {e}", path.display()))?;
    let Some(record) = value
        .as_object()
        .filter(|r| r.get("version").and_then(Value::as_str) == Some(RECORD_VERSION))
    else {
        anyhow::bail!("{} is not an {RECORD_VERSION} task record", path.display());
    };
    for key in [
        "repo",
        "source",
        "probe",
        "baseline_sha",
        "allowance",
        "headroom",
    ] {
        if record.get(key).is_none_or(Value::is_null) {
            anyhow::bail!("task record {} is missing '{key}'", path.display());
        }
    }
    Ok(record.clone())
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

fn gate(a: &GateArgs) -> anyhow::Result<u8> {
    let r = load(&a.task)?;
    let text = |k: &str| r[k].as_str().unwrap_or("").to_owned();
    let flag = |k: &str, default: bool| r.get(k).and_then(Value::as_bool).unwrap_or(default);
    let gate_args = gatecli::GateArgs {
        guardrail: true,
        fail_on_warn: a.fail_on_warn,
        allow_growth: r["allowance"].as_f64().unwrap_or(10.0),
        allow_headroom: r["headroom"].as_f64(),
        max_evidence: a.max_evidence,
        compact: a.compact,
        log_trajectory: a.log_trajectory.clone(),
        trajectory_strict: a.trajectory_strict,
    };
    gate_args.validate()?;
    let repo = PathBuf::from(text("repo"));
    let after = snapshot::resolve_ref(&repo, &a.after).map_err(anyhow::Error::msg)?;
    let probe = if text("probe") == "js" {
        emi_scan::Probe::Js
    } else {
        emi_scan::Probe::Python
    };
    let mut req = Request::new(&repo, &text("source"), &text("baseline_sha"), &after, probe);
    req.exclude = strings(r.get("exclude"));
    req.include = strings(r.get("include"));
    req.tolerate_syntax_errors = flag("tolerate_syntax_errors", false);
    req.use_gitignore = flag("use_gitignore", false);
    req.detect_tests = flag("detect_tests", true);
    if r.get("measurement").and_then(Value::as_str) == Some("precise") {
        req.profile = emi_scan::Profile::Precise;
    }
    let fail = |e: String| anyhow::anyhow!("emi task gate: {e}");
    let change = build_change(&req).map_err(fail)?;
    let report = emi_ledger::review(&change).map_err(fail)?;
    let mut record = Map::new();
    record.insert("repo".into(), json!(text("repo")));
    record.insert("source".into(), json!(text("source")));
    record.insert("before".into(), json!(text("baseline_sha")));
    record.insert("after".into(), json!(after));
    record.insert("before_ref".into(), json!(text("baseline_sha")));
    record.insert("after_ref".into(), json!(a.after));
    record.insert("probe".into(), json!(text("probe")));
    let (out, code) = gatecli::apply(report, &gate_args, record)?;
    let rendered = dumps_pretty(&out);
    match &a.output {
        Some(p) => std::fs::write(p, rendered)?,
        None => println!("{rendered}"),
    }
    Ok(code)
}

pub fn run(cmd: &TaskCommand) -> anyhow::Result<u8> {
    match cmd {
        TaskCommand::Init(a) => init(a),
        TaskCommand::Gate(a) => gate(a),
    }
}
