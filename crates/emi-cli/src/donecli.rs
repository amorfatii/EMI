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
    println!(
        "Wrote {}. Commit it: enforced verification reads it from the baseline.",
        path.display()
    );
    Ok(0)
}

/// Measure the same frozen candidate as the regression check, not a second
/// snapshot of a working tree that the proposer may have edited meanwhile.
fn change_evidence(context: &ChangeContext) -> ChangeEvidence {
    let mut req = emi_proving::Request::new(
        context.repo,
        &context.baseline.sha,
        context.config.tests.command.clone(),
    );
    req.candidate = emi_proving::Candidate::Ref(context.candidate.rev.clone());
    req.language = emi_proving::Language::from_name(&context.config.language);
    req.test_globs.clone_from(&context.config.tests.globs);
    req.source.clone_from(&context.config.source);
    req.link.clone_from(&context.config.link);
    if !context.config.python_path.is_empty() {
        // the same import path as the regression run: the candidate's code
        req.env.push((
            "PYTHONPATH".to_owned(),
            context.config.python_path.join(":"),
        ));
    }
    match emi_proving::measure_in(&req, context.view) {
        Ok(evidence) => ChangeEvidence::Measured(evidence.to_json()),
        Err(why) => ChangeEvidence::Failed(format!("change evidence could not be measured: {why}")),
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
    };
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
        emi_ledger::pyfmt::canonical(&outcome.receipt, &mut json);
        writeln!(out, "{json}")?;
    }
    Ok(outcome.action.exit_code())
}
