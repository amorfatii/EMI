//! `emi diff-mutate`: mutants on the lines a change added, run against
//! copies of its head, to show how much a passing test run says about the
//! change (docs/diff-mutate.md). A report: it gates nothing.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use emi_diff::snapshot::resolve_ref;
use emi_profile::diffmut::diff_mutation;
use emi_profile::mutation::Plan;
use emi_profile::sandbox::Runner;

use crate::profilecli::env_pairs;

#[derive(Args)]
pub struct DiffMutateArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Revision before the change.
    #[arg(long)]
    base: String,
    /// Revision after the change; mutants run against copies of it.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// Most mutants run, spread over the changed functions.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..))]
    max_mutants: u64,
    /// Ignored dependency directory linked into each copy (repeatable).
    #[arg(long)]
    link: Vec<String>,
    /// KEY=VALUE for the environment of test runs (repeatable).
    #[arg(long = "env")]
    env: Vec<String>,
    /// Upper bound for one test run, in seconds.
    #[arg(long, default_value_t = 600)]
    timeout_s: u64,
    /// Let test runs use the network.
    #[arg(long)]
    network: bool,
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
    /// Write here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Test command (argv, no shell) run against each mutant, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

pub fn run(a: &DiffMutateArgs) -> anyhow::Result<u8> {
    let base = resolve_ref(&a.repo, &a.base).map_err(anyhow::Error::msg)?;
    let head = resolve_ref(&a.repo, &a.head).map_err(anyhow::Error::msg)?;
    let changed = emi_diff::functions::changed_functions(&a.repo, &base, &head)
        .map_err(anyhow::Error::msg)?;
    let env = env_pairs(&a.env)?;
    let plan = Plan {
        runner: Runner {
            repo: &a.repo,
            rev: &head,
            link: &a.link,
            env: &env,
            timeout: Duration::from_secs(a.timeout_s),
            network: a.network,
        },
        command: &a.command,
    };
    let max = usize::try_from(a.max_mutants).unwrap_or(usize::MAX);
    let found = diff_mutation(&plan, &base, &changed, max).map_err(anyhow::Error::msg)?;
    let text = if a.format == "json" {
        emi_ledger::pyfmt::dumps_pretty(&found.to_json()) + "\n"
    } else {
        found.to_text()
    };
    match &a.output {
        Some(p) => std::fs::write(p, text).with_context(|| format!("writing {}", p.display()))?,
        None => write!(std::io::stdout().lock(), "{text}")?,
    }
    // survivors and failed baselines are findings of the report, not errors
    Ok(0)
}
