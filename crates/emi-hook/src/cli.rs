//! The `emi hook` subcommand.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};

use crate::install::{self, Dirs, Harness, Scope, Target};
use crate::{Env, claude, opencode};

#[derive(Subcommand)]
pub enum Command {
    /// Claude Code adapter: reads the hook event as JSON on stdin and answers
    /// in Claude Code's hook protocol. Registered by `emi hook install`.
    ClaudeCode {
        #[arg(value_enum)]
        event: claude::Event,
    },
    /// opencode adapter: reads an event from the EMI plugin as JSON on stdin
    /// and prints the plugin's instruction as JSON.
    Opencode {
        #[arg(value_enum)]
        event: opencode::Event,
    },
    /// Register the hook with a harness, keeping its existing settings.
    Install(InstallArgs),
    /// Remove what `install` registered.
    Uninstall(InstallArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    #[arg(long, value_enum)]
    harness: Harness,
    /// The repository (project scope writes inside it).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// `project` writes inside the repository, `user` into your own harness
    /// configuration (the hook stays silent in repositories without
    /// `.emi/config.json`).
    #[arg(long, value_enum, default_value_t = Scope::Project)]
    scope: Scope,
    /// How the harness starts emi; default is this executable's absolute
    /// path. Pass `emi` to rely on PATH, for a file you share.
    #[arg(long)]
    emi: Option<String>,
}

fn read_stdin() -> anyhow::Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the event from stdin")?;
    Ok(text)
}

fn setup(args: &InstallArgs) -> anyhow::Result<(PathBuf, Dirs, String)> {
    let repo = std::fs::canonicalize(&args.repo)
        .with_context(|| format!("cannot resolve --repo {}", args.repo.display()))?;
    let dirs = match args.scope {
        Scope::User => Dirs::from_process().map_err(anyhow::Error::msg)?,
        // not consulted for the project scope
        Scope::Project => Dirs {
            home: repo.clone(),
            config_home: repo.clone(),
        },
    };
    let emi = match &args.emi {
        Some(emi) => emi.clone(),
        None => std::env::current_exe()
            .context("cannot find this executable's path; pass --emi")?
            .to_string_lossy()
            .into_owned(),
    };
    Ok((repo, dirs, emi))
}

fn change(args: &InstallArgs, installing: bool) -> anyhow::Result<u8> {
    let (repo, dirs, emi) = setup(args)?;
    let target = Target {
        harness: args.harness,
        scope: args.scope,
        repo: &repo,
        dirs: &dirs,
        emi: &emi,
    };
    let lines = if installing {
        install::install(&target)
    } else {
        install::uninstall(&target)
    }
    .map_err(anyhow::Error::msg)?;
    let mut out = std::io::stdout().lock();
    for line in lines {
        writeln!(out, "{line}")?;
    }
    if installing && !repo.join(".emi/config.json").is_file() {
        writeln!(
            out,
            "note: {} has no .emi/config.json, so the hook stays silent there until `emi init` creates one",
            repo.display()
        )?;
    }
    Ok(0)
}

pub fn run(command: &Command) -> anyhow::Result<u8> {
    let output = match command {
        Command::ClaudeCode { event } => {
            claude::handle(*event, &read_stdin()?, &Env::from_process())
        }
        Command::Opencode { event } => {
            opencode::handle(*event, &read_stdin()?, &Env::from_process())
        }
        Command::Install(args) => return change(args, true),
        Command::Uninstall(args) => return change(args, false),
    };
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(0)
}
