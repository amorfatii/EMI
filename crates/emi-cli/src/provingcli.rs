//! `emi proving`: the receipt's `change` object for one candidate, printed
//! as JSON (crates/emi-proving). A diagnostic for the measurement; `emi
//! done` calls the same function.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use emi_proving::{Candidate, Language, Request};

use crate::profilecli::env_pairs;

#[derive(Args)]
pub struct ProvingArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// The baseline: where the task started.
    #[arg(long)]
    baseline: String,
    /// The candidate revision (default: the working tree as it is on disk).
    #[arg(long)]
    candidate: Option<String>,
    /// Language of the product code; only `python` (pytest) is measured.
    #[arg(long, default_value = "python")]
    language: String,
    /// Product directory (coverage is also measured where changes are).
    #[arg(long, default_value = "")]
    source: String,
    /// Glob of the test files (repeatable; default `tests/*`).
    #[arg(long = "test-glob")]
    test_globs: Vec<String>,
    /// Ignored dependency directory linked read-only into each copy (repeatable).
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
    /// Also require that a proving test kills a mutant no other test kills
    /// (not measured yet; the result says so).
    #[arg(long)]
    strict: bool,
    /// Write here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// The test command (argv, no shell), after `--`: pytest with options.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

pub fn run(a: &ProvingArgs) -> anyhow::Result<u8> {
    let mut req = Request::new(&a.repo, &a.baseline, a.command.clone());
    req.language = Language::from_name(&a.language);
    req.candidate = a
        .candidate
        .clone()
        .map_or(Candidate::Worktree, Candidate::Ref);
    req.source.clone_from(&a.source);
    if !a.test_globs.is_empty() {
        req.test_globs.clone_from(&a.test_globs);
    }
    req.link.clone_from(&a.link);
    req.env = env_pairs(&a.env)?;
    req.network = a.network;
    req.timeout = Duration::from_secs(a.timeout_s);
    req.strict = a.strict;
    let change = emi_proving::measure(&req).map_err(anyhow::Error::msg)?;
    let text = emi_ledger::pyfmt::dumps_pretty(&change.to_json()) + "\n";
    match &a.output {
        Some(path) => std::fs::write(path, text)?,
        None => write!(std::io::stdout().lock(), "{text}")?,
    }
    // a report: unknown, unproven and redundant are findings, not errors
    Ok(0)
}
