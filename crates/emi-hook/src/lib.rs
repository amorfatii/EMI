//! `emi-hook`: run the done check when a coding agent tries to stop.
//!
//! The harness adapters ([`claude`], [`opencode`]) only translate: they read
//! an event, call [`stop`] or [`user_message`], and write the answer in the
//! shape their harness documents. The loop itself lives in [`decision`], the
//! per-session memory in [`state`], and the subprocess that measures in
//! [`done`]. The hook never decides whether a change is good; it only
//! decides whether to send the agent back, with what `emi done` said.

use std::path::{Path, PathBuf};

pub mod claude;
pub mod cli;
pub mod decision;
pub mod done;
pub mod install;
pub mod opencode;
pub mod receipt;
pub mod repo;
pub mod state;

use decision::{Decision, DoneResult, State, StopInput, Tone};

/// What a harness should do after an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// Nothing to say: EMI is not set up here, or nothing changed.
    Silent,
    /// Keep the agent working; `reason` is all it is told.
    Block { reason: String },
    /// Let the agent stop and show `message` to the person.
    Notify { message: String, tone: Tone },
}

/// Where the hook keeps state and how it reaches the done check.
pub struct Env {
    pub state_home: PathBuf,
    pub done: done::Command,
}

impl Env {
    pub fn from_process() -> Result<Env, String> {
        Ok(Env {
            state_home: state::state_home()?,
            done: done::Command::from_environment()?,
        })
    }
}

impl From<Decision> for Reaction {
    fn from(decision: Decision) -> Self {
        match decision {
            Decision::Skip(_) => Reaction::Silent,
            Decision::Block { reason } => Reaction::Block { reason },
            Decision::Allow { message, tone } => Reaction::Notify { message, tone },
        }
    }
}

/// A failure of the hook itself. The agent is allowed to stop, because the
/// hook cannot count rounds or measure, but the person is told, and the
/// message is not a pass.
pub(crate) fn failure(error: &str, stopping: bool) -> Reaction {
    let message = if stopping {
        decision::incomplete_notice(error)
    } else {
        format!("EMI hook error: {error}")
    };
    Reaction::Notify {
        message,
        tone: Tone::Attention,
    }
}

/// The agent tries to stop in `cwd`.
pub fn stop(env: &Env, session: &str, cwd: &Path) -> Reaction {
    stop_checked(env, session, cwd).unwrap_or_else(|e| failure(&e, true))
}

/// The person sent a message in `cwd`.
pub fn user_message(env: &Env, session: &str, cwd: &Path) -> Reaction {
    user_message_checked(env, session, cwd).unwrap_or_else(|e| failure(&e, false))
}

fn stop_checked(env: &Env, session: &str, cwd: &Path) -> Result<Reaction, String> {
    let Some(root) = repo::locate(cwd)? else {
        return Ok(Reaction::Silent);
    };
    let config = repo::read_config(&root)?;
    let store = state::Store::open(&env.state_home, &root, session);
    let before = store.load()?;
    let content_id = repo::content_id(&root)?;
    let scope = scope_for(&root, &config, &before)?;
    let done_command =
        done::manual_command(scope.as_ref().unwrap_or(&done::scope_args(&root, None)));
    let input = StopInput {
        content_id: &content_id,
        rounds_limit: config.repair_rounds,
        done_command: &done_command,
    };
    let (decision, after) = decision::on_stop(&before, &input, || match &scope {
        Ok(args) => done::run(&env.done, &root, args),
        Err(why) => DoneResult::incomplete(why.clone()),
    });
    persist(&store, &before, &after)?;
    Ok(decision.into())
}

/// The arguments for `emi done`. When the agent works directly on the
/// default branch the baseline is the commit the session started on, and
/// without one there is nothing to compare against: that is an `Err` the
/// caller turns into an incomplete result, evaluated only when a check is
/// actually needed.
fn scope_for(
    root: &Path,
    config: &repo::Config,
    state: &State,
) -> Result<Result<Vec<String>, String>, String> {
    if !repo::on_default_branch(root, config.default_branch.as_deref())? {
        return Ok(Ok(done::scope_args(root, None)));
    }
    Ok(match &state.baseline {
        Some(sha) => Ok(done::scope_args(root, Some(sha))),
        None => Err(
            "the agent is working on the default branch and EMI never saw this session's first message, so it does not know where the session started"
                .to_owned(),
        ),
    })
}

fn persist(store: &state::Store, before: &State, after: &State) -> Result<(), String> {
    if before == after {
        return Ok(());
    }
    store.save(after)
}

fn user_message_checked(env: &Env, session: &str, cwd: &Path) -> Result<Reaction, String> {
    let Some(root) = repo::locate(cwd)? else {
        return Ok(Reaction::Silent);
    };
    let store = state::Store::open(&env.state_home, &root, session);
    let before = store.load()?;
    let head = repo::head(&root);
    let after = decision::on_user_message(&before, head.as_deref().ok());
    persist(&store, &before, &after)?;
    match head {
        Err(why) if after.baseline.is_none() => Err(format!(
            "could not record the session baseline ({why}); on the default branch the check will be incomplete"
        )),
        _ => Ok(Reaction::Silent),
    }
}
