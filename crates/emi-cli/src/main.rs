//! `emi`: structural burden measurement and gating for code changes.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use emi_scan::{OnSyntaxError, Options, Probe};

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

#[derive(Clone, Copy, ValueEnum)]
enum Language {
    Python,
    Js,
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
        /// Write JSON here instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn run(cli: Cli) -> anyhow::Result<()> {
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
            output,
        } => {
            let mut options = Options::new(match probe {
                Language::Python => Probe::Python,
                Language::Js => Probe::Js,
            });
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
            let measurement = emi_scan::scan(&source, &options)?;
            let json = serde_json::to_string_pretty(&measurement)?;
            match output {
                Some(path) => fs::write(&path, json + "\n")
                    .with_context(|| format!("writing {}", path.display()))?,
                None => writeln!(std::io::stdout().lock(), "{json}")?,
            }
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("emi: error: {e:#}");
            ExitCode::from(2)
        }
    }
}
