//! The done check: when a coding agent says it is finished, what was
//! actually proven? [`done`] reads `.emi/config.json`, resolves the
//! baseline, derives a task contract, verifies the candidate through
//! `emi-controller`, finds the old tests the candidate changed, and
//! assembles the receipt and its action (plan.md, interfaces 1 to 3).
//!
//! The change evidence (did the requested behaviour come into being) is
//! measured elsewhere: [`done`] takes a function that returns it. That
//! argument is the seam.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub mod action;
pub mod baseline;
pub mod candidate;
pub mod change;
pub mod config;
pub mod derived;
pub mod detect;
pub mod oldtests;
pub mod outcomes;
pub mod pytest;
pub mod receipt;
pub mod regression;
mod run;
pub mod state;
pub mod text;

#[cfg(test)]
mod testutil;

pub use action::Action;
pub use baseline::{Baseline, Given};
pub use change::ChangeEvidence;
pub use config::Config;
pub use run::proving_request;

/// The candidate id of the working tree.
pub const WORKTREE: &str = "worktree";

/// A failure that ends the run without a receipt: the input was unusable.
/// Anything that goes wrong while measuring is not one of these; it is an
/// explicit unknown inside the receipt.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// `.emi/config.json` is absent, invalid or unsupported.
    Config(String),
    /// A flag, a ref or the repository cannot be used as given.
    Usage(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Config(m) | Error::Usage(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Which kind of verification the receipt speaks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// On the user's machine, where the proposer could alter the
    /// controller's inputs.
    Cooperative,
    /// Where the proposer cannot reach them: the configuration comes from
    /// the baseline revision.
    Enforced,
}

impl Setting {
    pub fn as_str(self) -> &'static str {
        match self {
            Setting::Cooperative => "cooperative",
            Setting::Enforced => "enforced",
        }
    }
}

/// One run of the done check.
pub struct Request<'a> {
    pub repo: &'a Path,
    /// The user's state directory for EMI (see [`state::root_from_env`]).
    pub state_root: &'a Path,
    /// A baseline the caller names; without one it is the merge base with
    /// the default branch.
    pub baseline: Option<(&'a str, Given)>,
    pub candidate: candidate::Spec,
    pub setting: Setting,
    /// Items to approve (`old-test:PATH`), recorded before checking.
    pub approve: &'a [String],
}

/// What a change measurer is told about the run.
pub struct ChangeContext<'a> {
    pub repo: &'a Path,
    pub config: &'a Config,
    pub baseline: &'a Baseline,
    pub candidate: &'a candidate::Candidate,
    /// What was asked for, to read the candidate again if needed.
    pub spec: &'a candidate::Spec,
    /// A clean view of the repository that holds `candidate.rev`.
    pub view: &'a Path,
    pub regression: &'a regression::Regression,
}

/// The receipt and the action it leads to.
#[derive(Debug)]
pub struct Outcome {
    pub receipt: Value,
    pub action: Action,
}

/// The repository root for `path`, which must be the top of a git
/// repository: the check reads and restores files by repository-relative
/// path, and a subdirectory would silently shift all of them.
pub fn repo_root(path: &Path) -> Result<PathBuf, Error> {
    let root = std::fs::canonicalize(path)
        .map_err(|e| Error::Usage(format!("repository {}: {e}", path.display())))?;
    let top = emi_git::run(&root, &["rev-parse", "--show-toplevel"])
        .map_err(|e| Error::Usage(format!("{} is not a git repository: {e}", root.display())))?;
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim());
    if std::fs::canonicalize(&top).ok().as_deref() != Some(root.as_path()) {
        return Err(Error::Usage(format!(
            "{} is inside a repository whose root is {}; pass the root with --repo",
            root.display(),
            top.display()
        )));
    }
    Ok(root)
}

/// The content id of a candidate right now: the tree id of the commit or
/// of the working tree, for deciding whether anything changed since the
/// last check.
pub fn content_id(repo: &Path, spec: &candidate::Spec) -> Result<String, Error> {
    let repo = repo_root(repo)?;
    let view = emi_git::CleanView::of(&repo).map_err(Error::Usage)?;
    candidate::content_id_now(&repo, view.path(), spec)
}

/// Where the configuration comes from, which is what separates the
/// settings: the candidate's own copy, or the baseline's blob.
fn load_config(
    repo: &Path,
    req: &Request,
    cand: &candidate::Candidate,
    baseline: Option<&Baseline>,
) -> Result<Option<Config>, Error> {
    match (req.setting, baseline) {
        (Setting::Enforced, Some(b)) => config::read_at(repo, &b.sha),
        (Setting::Enforced, None) => Err(Error::Usage(
            "enforced verification reads .emi/config.json from the baseline, so it needs \
             --baseline"
                .to_owned(),
        )),
        (Setting::Cooperative, _) => match req.candidate {
            candidate::Spec::Worktree => config::read_file(repo).map(Some),
            candidate::Spec::Ref(_) => {
                config::read_at(repo, &cand.rev)?.map(Some).ok_or_else(|| {
                    Error::Config(format!(
                        "{} does not exist at the candidate; run `emi init`",
                        config::PATH
                    ))
                })
            }
        },
    }
}

/// Resolve the baseline: the caller's, or the merge base with the
/// configured default branch.
fn resolve_baseline(
    repo: &Path,
    req: &Request,
    config: Option<&Config>,
) -> Result<Baseline, Error> {
    let branch = config.map_or("", |c| c.default_branch.as_str());
    baseline::resolve(repo, req.baseline, branch)
}

/// Run the done check. `change` supplies the change evidence.
pub fn done(
    req: &Request,
    change: impl FnOnce(&ChangeContext) -> ChangeEvidence,
) -> Result<Outcome, Error> {
    let repo = repo_root(req.repo)?;
    // what killed runs left behind would fill the temporary directory
    emi_diff::snapshot::sweep_stale_scratch();
    let view = emi_git::CleanView::of(&repo).map_err(Error::Usage)?;
    let cand = candidate::read(&repo, view.path(), &req.candidate)?;
    let (config, baseline) = match req.setting {
        Setting::Enforced => {
            let baseline = resolve_baseline(&repo, req, None)?;
            (load_config(&repo, req, &cand, Some(&baseline))?, baseline)
        }
        Setting::Cooperative => {
            let config = load_config(&repo, req, &cand, None)?;
            let baseline = resolve_baseline(&repo, req, config.as_ref())?;
            (config, baseline)
        }
    };
    let Some(config) = config else {
        return Ok(unconfigured(&repo, req, &baseline, &cand));
    };
    run::check(&repo, view.path(), req, (&config, &baseline, &cand), change)
}

/// Run the baseline's tests ahead of the first check (see `run::warm`).
/// Only cooperative verification of a working tree is warmed: that is
/// what the hook runs. `Err` is for input `done` would refuse too.
pub fn warm(req: &Request) -> Result<(), Error> {
    if req.setting != Setting::Cooperative {
        return Ok(());
    }
    let repo = repo_root(req.repo)?;
    emi_diff::snapshot::sweep_stale_scratch();
    let view = emi_git::CleanView::of(&repo).map_err(Error::Usage)?;
    let config = config::read_file(&repo)?;
    let baseline = resolve_baseline(&repo, req, Some(&config))?;
    run::warm(&repo, view.path(), req, &config, &baseline);
    Ok(())
}

/// Enforced verification of a baseline that has no configuration: the
/// receipt says why nothing was measured.
fn unconfigured(
    repo: &Path,
    req: &Request,
    baseline: &Baseline,
    cand: &candidate::Candidate,
) -> Outcome {
    let why = format!(
        "{} is absent at the baseline {}; enforced verification does not read it from the candidate",
        config::PATH,
        baseline.sha
    );
    let regression = regression::Regression::unknown(vec![why]);
    let change = ChangeEvidence::NotMeasured("nothing was measured".to_owned());
    let limits = receipt::Limits {
        setting: req.setting,
        link: &[],
        pytest: true,
        change: &change,
        config_note: None,
    };
    let parts = receipt::Parts {
        repo,
        baseline,
        candidate: cand,
        setting: req.setting,
        isolation: "unknown",
        regression: &regression,
        old_tests: &[],
        change: &change,
        on_unproven: config::OnUnproven::Report,
        faults: Vec::new(),
        not_verified: receipt::not_verified(&limits),
        this_run: receipt::this_run(&limits),
    };
    let (receipt, action) = receipt::assemble(&parts);
    Outcome { receipt, action }
}
