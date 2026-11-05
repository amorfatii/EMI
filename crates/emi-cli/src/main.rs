//! `emi`: when a coding agent says it is done, say what was established.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod donecli;

#[derive(Parser)]
#[command(
    name = "emi",
    version,
    about = "When your agent says done, EMI tells you what was actually established"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write `.emi/config.json`: detect the language, test command and
    /// default branch, show them, and write once confirmed.
    Init(donecli::InitArgs),
    /// When an agent says it is done: say what was established (regression
    /// evidence, laws, changed old tests, the action to take). Exit 0 stop,
    /// 3 repair, 4 ask a person, 5 incomplete, 2 usage or configuration error.
    Done(donecli::DoneArgs),
    /// Run the done check when a coding agent tries to stop (Claude Code,
    /// opencode), and install or remove that hook.
    #[command(subcommand)]
    Hook(emi_hook::cli::Command),
    /// What the hook did in this repository (checks, interventions, time),
    /// and a person's labels of its interventions: useful, noise or wrong.
    #[command(subcommand)]
    Journal(emi_hook::journal::Command),
}

fn run(cli: Cli) -> anyhow::Result<u8> {
    match cli.command {
        Command::Init(args) => donecli::init(&args),
        Command::Done(args) => donecli::done(&args),
        Command::Hook(cmd) => emi_hook::cli::run(&cmd),
        Command::Journal(cmd) => emi_hook::journal::run(&cmd),
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
