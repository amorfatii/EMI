//! `emi init` and `emi done`: the check a harness runs when a coding agent
//! says it is finished (docs/done.md).

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::bail;
use clap::{Args, ValueEnum};
use emi_done::{ChangeContext, ChangeEvidence, Given, Request, Setting, candidate, config, detect};

#[derive(Args)]
pub struct InitArgs {
    /// Repository root (default: current directory).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Write the configuration without asking.
    #[arg(long)]
    yes: bool,
    /// Replace an existing `.emi/config.json`.
    #[arg(long)]
    force: bool,
    /// The default branch, when it cannot be detected.
    #[arg(long, value_name = "BRANCH")]
    default_branch: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SettingArg {
    Cooperative,
    Enforced,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Json,
    Text,
}

#[derive(Clone, Copy, ValueEnum)]
enum BaselineSource {
    Explicit,
    Session,
}

#[derive(Args)]
pub struct DoneArgs {
    /// Repository root (default: current directory).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// The revision the task started from (default: where this branch split
    /// off the default branch).
    #[arg(long, value_name = "REF")]
    baseline: Option<String>,
    /// How `--baseline` was chosen; the hook says `session`.
    #[arg(long, value_enum, default_value_t = BaselineSource::Explicit, requires = "baseline")]
    baseline_source: BaselineSource,
    /// Check this commit instead of the working tree.
    #[arg(long, value_name = "REF")]
    candidate: Option<String>,
    /// `enforced` reads the configuration from the baseline, never from
    /// the candidate.
    #[arg(long, value_enum, default_value_t = SettingArg::Cooperative)]
    setting: SettingArg,
    /// Default: text on a terminal, JSON otherwise.
    #[arg(long, value_enum)]
    format: Option<FormatArg>,
    /// In the text form, list every standing limit instead of counting them.
    #[arg(long)]
    full: bool,
    /// Approve a changed old test, as named in the text receipt
    /// (`old-test:PATH`); repeatable.
    #[arg(long, value_name = "ITEM")]
    approve: Vec<String>,
    /// The change must keep the behaviour of the functions it modifies
    /// (under these paths; none for all): each is compared with the
    /// baseline's on the inputs the tests pass it and inputs drawn from its
    /// annotations. Without the flag, the intent `emi intent` recorded on
    /// this machine applies (cooperative setting only).
    #[arg(long, value_name = "PATH", num_args = 0..)]
    preserve: Option<Vec<String>>,
    /// Run the baseline's tests now and keep the result, so that the first
    /// check finds it waiting; prints nothing and checks nothing. The hook
    /// starts this when the person sends a message.
    #[arg(long, conflicts_with_all = ["candidate", "approve"])]
    warm: bool,
}

fn confirmed(path: &std::path::Path) -> anyhow::Result<bool> {
    print!("Write {}? [y/N] ", path.display());
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// `emi init`: propose the configuration, and write it once confirmed.
pub fn init(a: &InitArgs) -> anyhow::Result<u8> {
    let repo = emi_done::repo_root(&a.repo)?;
    let path = repo.join(config::PATH);
    if path.exists() && !a.force {
        bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    let found = detect::detect(&repo, a.default_branch.as_deref())?;
    println!("Detected:");
    for note in &found.notes {
        println!("  {note}");
    }
    for warning in &found.warnings {
        eprintln!("emi: warning: {warning}");
    }
    println!("\n{}", found.config.to_text());
    if !a.yes {
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "emi: not written: nobody to confirm; pass --yes to write {}",
                path.display()
            );
            return Ok(2);
        }
        if !confirmed(&path)? {
            println!("Not written.");
            return Ok(1);
        }
    }
    detect::write(&repo, &found.config, a.force)?;
    if detect::ignored(&repo)? {
        println!(
            "Wrote {}. git ignores it, so `git add` leaves it out: commit it with `git add -f {}`; enforced verification reads it from the baseline.",
            path.display(),
            emi_done::config::PATH
        );
    } else {
        println!(
            "Wrote {}. Commit it: enforced verification reads it from the baseline.",
            path.display()
        );
    }
    Ok(0)
}

/// The languages this build checks (ADR 0009). A project's configuration
/// names one of them.
pub static DRIVERS: &[&dyn emi_driver::Driver] = &[&emi_python::driver::Python, &emi_rust::Rust];

/// Measure the same frozen candidate as the regression check, not a second
/// snapshot of a working tree that the proposer may have edited meanwhile.
fn change_evidence(context: &ChangeContext) -> ChangeEvidence {
    let run = emi_done::run_of(
        (context.repo, context.view),
        context.config,
        (&context.baseline.sha, &context.candidate.rev),
        context.listing_cache.clone(),
    );
    match context.driver.change(&run) {
        Ok(evidence) => ChangeEvidence::Measured(evidence),
        Err(emi_driver::Unmeasured::Unsupported(why)) => ChangeEvidence::NotMeasured(why),
        Err(emi_driver::Unmeasured::Failed(why)) => {
            ChangeEvidence::Failed(format!("change evidence could not be measured: {why}"))
        }
    }
}

/// `emi done`: check the candidate and print the receipt; the exit status
/// is the action (0 stop, 3 repair, 4 ask_person, 5 incomplete).
pub fn done(a: &DoneArgs) -> anyhow::Result<u8> {
    let state_root = emi_done::state::root_from_env()?;
    let given = a.baseline.as_deref().map(|b| {
        let how = match a.baseline_source {
            BaselineSource::Explicit => Given::Explicit,
            BaselineSource::Session => Given::Session,
        };
        (b, how)
    });
    let request = Request {
        drivers: DRIVERS,
        repo: &a.repo,
        state_root: &state_root,
        baseline: given,
        candidate: match &a.candidate {
            Some(reference) => candidate::Spec::Ref(reference.clone()),
            None => candidate::Spec::Worktree,
        },
        setting: match a.setting {
            SettingArg::Cooperative => Setting::Cooperative,
            SettingArg::Enforced => Setting::Enforced,
        },
        approve: &a.approve,
        preserve: a.preserve.clone(),
    };
    if a.warm {
        emi_done::warm(&request)?;
        return Ok(0);
    }
    let outcome = emi_done::done(&request, change_evidence)?;
    let text = match a.format {
        Some(FormatArg::Json) => false,
        Some(FormatArg::Text) => true,
        None => std::io::stdout().is_terminal(),
    };
    let mut out = std::io::stdout().lock();
    if text {
        let detail = if a.full {
            emi_done::text::Detail::Full
        } else {
            emi_done::text::Detail::Short
        };
        write!(
            out,
            "{}",
            emi_done::text::render_with(&outcome.receipt, detail)
        )?;
    } else {
        let mut json = String::new();
        emi_kernel::json::canonical(&outcome.receipt, &mut json);
        writeln!(out, "{json}")?;
    }
    Ok(outcome.action.exit_code())
}

#[derive(Args)]
pub struct ReplayArgs {
    /// A receipt `emi done --format json` printed.
    receipt: PathBuf,
    /// Repository root (default: current directory).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

/// `emi replay`: the receipt computed again from nothing cached. Prints
/// whether the decision is the same and, if not, which parts differ.
pub fn replay(a: &ReplayArgs) -> anyhow::Result<u8> {
    let text = std::fs::read_to_string(&a.receipt)
        .map_err(|e| anyhow::anyhow!("{}: {e}", a.receipt.display()))?;
    let original = emi_kernel::strict::loads(&text)
        .map_err(|e| anyhow::anyhow!("{}: {e}", a.receipt.display()))?;
    let state_root = emi_done::state::root_from_env()?;
    let replayed =
        emi_done::replay::replay((&a.repo, &state_root), &original, DRIVERS, change_evidence)?;
    let mut json = String::new();
    emi_kernel::json::canonical(&replayed.to_value(), &mut json);
    writeln!(std::io::stdout().lock(), "{json}")?;
    Ok(if replayed.same { 0 } else { 1 })
}

#[derive(clap::Subcommand)]
pub enum IntentCommand {
    /// The change must keep the behaviour of the functions it modifies
    /// (under these paths; none for all), until cleared.
    Preserve {
        paths: Vec<String>,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// The change may alter behaviour (the default).
    Clear {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Print the intent recorded for the repository.
    Show {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}

/// `emi intent`: what the person declares about the change, recorded on
/// this machine for the cooperative check (and so for the hook).
pub fn intent(c: &IntentCommand) -> anyhow::Result<u8> {
    let state_root = emi_done::state::root_from_env()?;
    let (repo, wanted) = match c {
        IntentCommand::Preserve { paths, repo } => (
            repo,
            Some(emi_done::preserve::Intent {
                preserve: true,
                paths: paths.clone(),
            }),
        ),
        IntentCommand::Clear { repo } => (repo, Some(emi_done::preserve::Intent::default())),
        IntentCommand::Show { repo } => (repo, None),
    };
    let repo = emi_done::repo_root(repo)?;
    let dir = emi_done::state::repo_dir(&state_root, &repo);
    if let Some(intent) = wanted {
        emi_done::preserve::write(&dir, &intent)?;
    }
    let now = emi_done::preserve::read(&dir)?;
    let mut out = std::io::stdout().lock();
    match (now.preserve, now.paths.is_empty()) {
        (false, _) => writeln!(out, "No intent: the change may alter behaviour.")?,
        (true, true) => writeln!(out, "Preserve: every function the change modifies.")?,
        (true, false) => writeln!(out, "Preserve: functions under {}.", now.paths.join(", "))?,
    }
    Ok(0)
}
