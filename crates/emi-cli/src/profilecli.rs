//! `emi profile`: per function, structure, change history, what the tests
//! run and how much of it they check, and how far a change there could be
//! verified (docs/profile.md).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use emi_profile::covrun::CoverageRun;
use emi_profile::history::Window;
use emi_profile::{CoverageSource, Execution, MutationOptions, Options};

use crate::Language;

#[derive(Args)]
pub struct ProfileArgs {
    /// Git repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Product scope subdirectory.
    #[arg(long, default_value = "src")]
    source: String,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    /// Revision to profile.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// History window in days before the head's commit time (0: all history).
    #[arg(long, default_value_t = 365)]
    days: u32,
    /// Coverage report produced at the head: lcov, Cobertura XML,
    /// coverage.py JSON or a Go cover profile.
    #[arg(long, conflicts_with = "coverage_command")]
    coverage: Option<PathBuf>,
    /// Measure coverage here instead: a command run in a sandboxed copy of
    /// the head, quoted like a shell word list but never run by a shell
    /// (repeatable; the steps run in order in the same copy).
    #[arg(long, requires = "coverage_output")]
    coverage_command: Vec<String>,
    /// Where the coverage commands write their report, relative to the
    /// repository root.
    #[arg(long, requires = "coverage_command")]
    coverage_output: Option<String>,
    /// Functions listed (0: all; the summary always counts all).
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// With a test command: the most exposed functions that get mutants.
    #[arg(long, default_value_t = 10)]
    mutate_functions: usize,
    /// With a test command: mutants per function, spread over its lines.
    #[arg(long, default_value_t = 5)]
    mutants_per_function: usize,
    /// Ignored dependency directory linked into each copy (repeatable).
    #[arg(long)]
    link: Vec<String>,
    /// KEY=VALUE for the environment of coverage and test runs (repeatable).
    #[arg(long = "env")]
    env: Vec<String>,
    /// Upper bound for one coverage step or test run, in seconds.
    #[arg(long, default_value_t = 600)]
    timeout_s: u64,
    /// Let coverage and test runs use the network.
    #[arg(long)]
    network: bool,
    /// Record unparseable files instead of aborting the measurement.
    #[arg(long)]
    tolerate_syntax_errors: bool,
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
    /// Write here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Test command (argv, no shell) run against sampled mutants, after `--`.
    #[arg(last = true)]
    command: Vec<String>,
}

fn env_pairs(env: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    env.iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .ok_or_else(|| anyhow::anyhow!("--env needs KEY=VALUE, got {kv:?}"))
        })
        .collect()
}

fn coverage(a: &ProfileArgs) -> anyhow::Result<Option<CoverageSource>> {
    if let Some(path) = &a.coverage {
        return Ok(Some(CoverageSource::File(path.clone())));
    }
    let Some(output) = &a.coverage_output else {
        return Ok(None);
    };
    let commands = a
        .coverage_command
        .iter()
        .map(|c| {
            shlex::split(c)
                .filter(|argv| !argv.is_empty())
                .ok_or_else(|| anyhow::anyhow!("cannot split --coverage-command {c:?}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(CoverageSource::Run(CoverageRun {
        commands,
        output: output.clone(),
    })))
}

fn options(a: &ProfileArgs) -> anyhow::Result<Options> {
    let mutation = (!a.command.is_empty()).then(|| MutationOptions {
        command: a.command.clone(),
        functions: a.mutate_functions,
        per_function: a.mutants_per_function,
    });
    Ok(Options {
        repo: a.repo.clone(),
        source: a.source.clone(),
        probe: a.probe.into(),
        head: a.head.clone(),
        window: Window {
            days: (a.days > 0).then_some(a.days),
            ..Window::default()
        },
        coverage: coverage(a)?,
        mutation,
        execution: Execution {
            link: a.link.clone(),
            env: env_pairs(&a.env)?,
            timeout: Duration::from_secs(a.timeout_s),
            network: a.network,
        },
        top: a.top,
        tolerate_syntax_errors: a.tolerate_syntax_errors,
    })
}

pub fn run(a: &ProfileArgs) -> anyhow::Result<u8> {
    let profile = emi_profile::build(&options(a)?).map_err(anyhow::Error::msg)?;
    let text = if a.format == "json" {
        emi_ledger::pyfmt::dumps_pretty(&profile) + "\n"
    } else {
        emi_profile::text::render(&profile)
    };
    match &a.output {
        Some(p) => std::fs::write(p, text).with_context(|| format!("writing {}", p.display()))?,
        None => write!(std::io::stdout().lock(), "{text}")?,
    }
    Ok(0)
}
