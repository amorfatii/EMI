//! `emi drift`: the behavioural drift report (`emi_profile::drift`) for two
//! revisions, each side run in sandboxed copies of its commit
//! (docs/drift.md).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use emi_diff::snapshot::resolve_ref;
use emi_profile::Execution;
use emi_profile::drift::{NOTE, Request};
use emi_profile::sandbox::Runner;
use serde_json::Value;

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

pub fn measure(o: &Options) -> anyhow::Result<Value> {
    let base = resolve_ref(&o.repo, &o.base).map_err(anyhow::Error::msg)?;
    let head = resolve_ref(&o.repo, &o.head).map_err(anyhow::Error::msg)?;
    let runner = |rev| Runner {
        repo: &o.repo,
        rev,
        link: &o.execution.link,
        env: &o.execution.env,
        timeout: o.execution.timeout,
        network: o.execution.network,
    };
    let (base_side, head_side) = (runner(&base), runner(&head));
    let commands = [o.command.clone()];
    emi_profile::drift::measure(&Request {
        repo: &o.repo,
        base: base.clone(),
        head: head.clone(),
        commands: &commands,
        call_timeout_s: o.call_timeout_s,
        base_side: &base_side,
        head_side: &head_side,
    })
    .map_err(anyhow::Error::msg)
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
