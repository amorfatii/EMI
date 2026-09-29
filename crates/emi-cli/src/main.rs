//! `emi`: structural burden measurement and gating for code changes.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use emi_scan::{OnSyntaxError, Options, Probe};

mod checkcli;
mod diffcli;
mod explaincli;
mod gatecli;
mod guardcli;
mod historycli;
mod markdown;
mod mergecli;
mod profilecli;
mod report;
mod sarif;
mod taskcli;

#[derive(Parser)]
#[command(
    name = "emi",
    version,
    about = "Structural burden measurement and gating for code changes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Measurement contract.
#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    /// Reproduce the v0.5.1 oracle exactly.
    Parity,
    /// Static coupling with value origins (fewer unknowns; new contract).
    Precise,
    /// Precise coupling; function erosion graded by cognitive complexity and
    /// Halstead volume (ranks buggy functions better than parity erosion).
    Graded,
}

impl From<ProfileArg> for emi_scan::Profile {
    fn from(p: ProfileArg) -> Self {
        match p {
            ProfileArg::Parity => emi_scan::Profile::Parity,
            ProfileArg::Precise => emi_scan::Profile::Precise,
            ProfileArg::Graded => emi_scan::Profile::Graded,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Language {
    Python,
    Js,
    Rust,
    Go,
}

impl From<Language> for Probe {
    fn from(l: Language) -> Self {
        match l {
            Language::Python => Probe::Python,
            Language::Js => Probe::Js,
            Language::Rust => Probe::Rust,
            Language::Go => Probe::Go,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Measure a source scope and print the measurement as JSON.
    Scan {
        /// Scope root (a directory).
        source: PathBuf,
        #[arg(long, value_enum, default_value_t = Language::Python)]
        probe: Language,
        /// Record unreadable or unparseable files instead of aborting.
        #[arg(long)]
        tolerate_syntax_errors: bool,
        /// Measure test files as product source (Python).
        #[arg(long)]
        no_test_detection: bool,
        /// Do not prune the default venv/build/cache directories.
        #[arg(long)]
        no_default_excludes: bool,
        /// fnmatch glob against scope-relative paths to exclude (repeatable).
        #[arg(long = "exclude")]
        exclude: Vec<String>,
        /// fnmatch glob restricting measured paths (repeatable).
        #[arg(long = "include")]
        include: Vec<String>,
        /// JS: directory tsconfig.json is read from (default: the scope root).
        #[arg(long)]
        config_root: Option<PathBuf>,
        /// Python: also skip paths matched by the root .gitignore.
        #[arg(long)]
        use_gitignore: bool,
        #[arg(long, value_enum, default_value_t = ProfileArg::Parity)]
        profile: ProfileArg,
        /// Output format: the measurement JSON, or SARIF 2.1.0 results.
        #[arg(long, default_value = "json", value_parser = ["json", "sarif"])]
        format: String,
        /// Measure the scope as of this commit/ref (raw tracked blobs, as
        /// `emi diff` does); SOURCE is then relative to --repo.
        #[arg(long, value_name = "REF")]
        at: Option<String>,
        /// Repository for --at.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Write JSON here instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Review a change JSON (before/after component measurements).
    Review {
        /// Change JSON as produced by `emi diff` or build_change.
        input: PathBuf,
        #[command(flatten)]
        gate: gatecli::GateArgs,
    },
    /// Measure a change between two git refs; with --guardrail, review
    /// and gate it.
    Diff(diffcli::DiffArgs),
    /// Gate consecutive first-parent commit pairs and report the verdict
    /// curve (calibrate allowances before enforcing).
    Backtest(diffcli::BacktestArgs),
    /// Rank files by measured burden × change frequency; report change
    /// coupling, authorship and AI attribution over a history window.
    Hotspots(historycli::HotspotsArgs),
    /// Run declarative rules (built-in catalog and your own YAML) with
    /// inline suppressions and baselines.
    Check(checkcli::CheckArgs),
    /// Merge SARIF logs from several tools into one (runs joined by tool,
    /// rules unioned, duplicate results dropped).
    SarifMerge(mergecli::MergeArgs),
    /// Cumulative task gating: pin a baseline once, gate every retry
    /// against it.
    #[command(subcommand)]
    Task(taskcli::TaskCommand),
    /// Write a self-contained HTML report (burden, hotspots, coupling, findings).
    Report(report::ReportArgs),
    /// Verifiable guardrails on a change: scope, weakened tests, prohibited
    /// files, new suppressions, growth (exit 2 on a blocking violation).
    Guard(guardcli::GuardArgs),
    /// Why each function carries graded erosion, and the small structural
    /// steps that lower it.
    Explain(explaincli::ExplainArgs),
    /// Per function: structure, change history, what the tests run
    /// (--coverage) and how much of it they check (sampled mutants), and how
    /// far a change there could be verified. Advisory; gates nothing.
    Profile(profilecli::ProfileArgs),
    /// Print the classic per-function metrics `graded` is built from (SLOC,
    /// cognitive complexity, Halstead volume), as the lab panel defines them.
    Panel {
        /// Source files; the extension picks the grammar.
        files: Vec<PathBuf>,
    },
    /// Serve EMI's tools to agents over the Model Context Protocol (stdio).
    Mcp,
    /// Language server (stdio): rule diagnostics, erosion hints, burden-delta code lenses.
    Lsp,
}

fn review(input: &std::path::Path, gate: &gatecli::GateArgs) -> anyhow::Result<u8> {
    gate.validate()?;
    let report = fs::read_to_string(input)
        .map_err(|e| e.to_string())
        .and_then(|text| emi_ledger::strict::loads(&text))
        .and_then(|change| emi_ledger::review(&change))
        .map_err(|e| {
            anyhow::anyhow!(
                "cannot review {}: {e}; pass a build_change JSON file with matching measurement_profile_before/after",
                input.display()
            )
        })?;
    let mut record = serde_json::Map::new();
    for (k, v) in [("repo", ""), ("source", "")] {
        record.insert(k.into(), v.into());
    }
    for k in ["before", "after", "before_ref", "after_ref"] {
        record.insert(k.into(), serde_json::Value::Null);
    }
    let profile = report
        .get("measurement_profile")
        .cloned()
        .unwrap_or_default();
    record.insert("probe".into(), profile.clone());
    record.insert("measurement_profile".into(), profile);
    let (out, code) = gatecli::apply(report, gate, record)?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        emi_ledger::pyfmt::dumps_pretty(&out)
    )?;
    Ok(code)
}

/// Measure `source` (repository-relative) at `reference` from raw blobs.
fn scan_at(
    repo: &std::path::Path,
    source: &std::path::Path,
    reference: &str,
    options: &Options,
) -> anyhow::Result<serde_json::Value> {
    let mut req = emi_diff::Request::new(
        repo,
        &source.to_string_lossy(),
        reference,
        reference,
        options.probe,
    );
    req.exclude.clone_from(&options.exclude_globs);
    req.include.clone_from(&options.include_globs);
    req.tolerate_syntax_errors = options.on_syntax_error == OnSyntaxError::Record;
    req.detect_tests = options.detect_tests;
    req.use_gitignore = options.use_gitignore;
    req.profile = options.profile;
    let (sha, measurement) =
        emi_diff::change::measure_ref(&req, reference).map_err(anyhow::Error::msg)?;
    let mut m =
        measurement.ok_or_else(|| anyhow::anyhow!("no measurable source files at {reference}"))?;
    m["commit"] = serde_json::json!(sha);
    Ok(m)
}

/// `{file: [{name, start, end, sloc, cognitive, halstead_volume}]}`.
fn panel(files: &[PathBuf]) -> anyhow::Result<u8> {
    let mut out = serde_json::Map::new();
    for path in files {
        let name = path.to_string_lossy();
        let Some(lang) = emi_panel::Lang::of_path(&name) else {
            continue;
        };
        let src = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let fns: Vec<serde_json::Value> = emi_panel::functions(lang, &src)
            .into_iter()
            .map(|f| {
                serde_json::json!({"name": f.name, "start": f.start, "end": f.end, "sloc": f.sloc,
                                   "cognitive": f.cognitive, "halstead_volume": f.halstead_volume})
            })
            .collect();
        out.insert(name.into_owned(), serde_json::Value::Array(fns));
    }
    println!("{}", serde_json::Value::Object(out));
    Ok(0)
}

fn run(cli: Cli) -> anyhow::Result<u8> {
    match cli.command {
        Command::Scan {
            source,
            probe,
            tolerate_syntax_errors,
            no_test_detection,
            no_default_excludes,
            exclude,
            include,
            config_root,
            use_gitignore,
            profile: measurement_profile,
            format,
            at,
            repo,
            output,
        } => {
            let mut options = Options::new(probe.into());
            if tolerate_syntax_errors {
                options.on_syntax_error = OnSyntaxError::Record;
            }
            options.detect_tests = !no_test_detection;
            if no_default_excludes {
                options.exclude_dirs = Some(Vec::new());
            }
            options.exclude_globs = exclude;
            options.include_globs = include;
            options.config_root = config_root;
            options.use_gitignore = use_gitignore;
            options.profile = measurement_profile.into();
            let (measurement, root) = match at {
                Some(reference) => (
                    scan_at(&repo, &source, &reference, &options)?,
                    repo.join(&source),
                ),
                None => (
                    serde_json::to_value(emi_scan::scan(&source, &options)?)?,
                    source.clone(),
                ),
            };
            let json = if format == "sarif" {
                serde_json::to_string_pretty(&sarif::log(&measurement, &root))?
            } else {
                serde_json::to_string_pretty(&measurement)?
            };
            match output {
                Some(path) => fs::write(&path, json + "\n")
                    .with_context(|| format!("writing {}", path.display()))?,
                None => writeln!(std::io::stdout().lock(), "{json}")?,
            }
            Ok(0)
        }
        Command::Review { input, gate } => review(&input, &gate),
        Command::Diff(args) => diffcli::run(&args),
        Command::Backtest(args) => diffcli::backtest(&args),
        Command::Hotspots(args) => historycli::run(&args),
        Command::Check(args) => checkcli::run(&args),
        Command::SarifMerge(args) => mergecli::run(&args),
        Command::Task(cmd) => taskcli::run(&cmd),
        Command::Report(args) => report::run(&args),
        Command::Guard(args) => guardcli::run(&args),
        Command::Panel { files } => panel(&files),
        Command::Explain(args) => explaincli::run(&args),
        Command::Profile(args) => profilecli::run(&args),
        Command::Lsp => {
            emi_lsp::serve(std::io::stdin().lock(), std::io::stdout().lock())?;
            Ok(0)
        }
        Command::Mcp => {
            emi_mcp::serve(std::io::stdin().lock(), std::io::stdout().lock())?;
            Ok(0)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("emi: error: {e:#}");
            ExitCode::from(2)
        }
    }
}
