//! Running `emi done` as a subprocess and reading its answer.
//!
//! The hook knows `emi done` only through its command line and its receipt
//! (docs/plan.md, interfaces 2 and 3). `EMI_DONE_COMMAND` replaces the
//! command prefix (default: this executable followed by `done`) so tests can
//! stand in a script that prints receipts.

use std::path::Path;
use std::time::Duration;

use emi_exec::{Spec, Status};

use crate::decision::DoneResult;
use crate::receipt;

/// Environment variable naming the command that stands for `emi done`.
pub const COMMAND_VARIABLE: &str = "EMI_DONE_COMMAND";
/// Environment variable overriding how long the check may run, in seconds.
pub const TIMEOUT_VARIABLE: &str = "EMI_HOOK_DONE_TIMEOUT_SECS";

/// Below the Claude Code hook timeout that `emi hook install` registers
/// (see `claude::STOP_TIMEOUT_SECS`), so the executor stops the check and
/// reports it before the harness cancels the hook and discards the report.
pub const DEFAULT_TIMEOUT_SECS: u64 = 840;

/// Variables `emi done` may need from the user's environment. Nothing else
/// is inherited: the executor starts from an explicit allowlist.
const INHERITED: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL",
];

/// How to start the done check.
#[derive(Clone, Debug)]
pub struct Command {
    /// The program and the arguments that come before `--repo`.
    pub prefix: Vec<String>,
    pub timeout: Duration,
}

impl Command {
    /// The command from the process environment: `EMI_DONE_COMMAND` when
    /// set, otherwise this executable's own `done` subcommand.
    pub fn from_environment() -> Result<Command, String> {
        let prefix = match std::env::var(COMMAND_VARIABLE) {
            Ok(text) => shlex::split(&text)
                .filter(|argv| !argv.is_empty())
                .ok_or_else(|| {
                    format!("{COMMAND_VARIABLE} is empty or not a valid command line")
                })?,
            Err(_) => {
                let exe = std::env::current_exe().map_err(|e| {
                    format!("cannot find the emi executable to run `emi done`: {e}")
                })?;
                vec![exe.to_string_lossy().into_owned(), "done".to_owned()]
            }
        };
        let timeout = match std::env::var(TIMEOUT_VARIABLE) {
            Ok(text) => text.parse::<u64>().ok().filter(|s| *s > 0).ok_or_else(|| {
                format!("{TIMEOUT_VARIABLE} must be a positive number of seconds")
            })?,
            Err(_) => DEFAULT_TIMEOUT_SECS,
        };
        Ok(Command {
            prefix,
            timeout: Duration::from_secs(timeout),
        })
    }
}

/// The arguments that fix which change is checked against what. The
/// baseline is passed only when the session baseline decides it; on any
/// other branch `emi done` resolves the merge base itself.
pub fn scope_args(repo: &Path, session_baseline: Option<&str>) -> Vec<String> {
    let mut args = vec!["--repo".to_owned(), repo.to_string_lossy().into_owned()];
    if let Some(sha) = session_baseline {
        args.extend(["--baseline".to_owned(), sha.to_owned()]);
        args.extend(["--baseline-source".to_owned(), "session".to_owned()]);
    }
    args
}

/// What a person types to run the same check by hand and add an approval.
pub fn manual_command(args: &[String]) -> String {
    let quoted = args
        .iter()
        .map(|a| shlex::try_quote(a).map_or_else(|_| a.clone(), |q| q.into_owned()));
    std::iter::once("emi done".to_owned())
        .chain(quoted)
        .collect::<Vec<_>>()
        .join(" ")
}

fn inherited_environment() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| {
            INHERITED.contains(&k.as_str()) || k.starts_with("XDG_") || k.starts_with("EMI_")
        })
        .collect()
}

/// Run the check once and interpret the answer. Every way it can fail to
/// answer is an `incomplete` result.
pub fn run(command: &Command, repo: &Path, scope: &[String]) -> DoneResult {
    let mut argv: Vec<&str> = command.prefix.iter().map(String::as_str).collect();
    argv.extend(scope.iter().map(String::as_str));
    argv.extend(["--format", "json"]);
    let mut spec = Spec::new(&argv, repo);
    spec.env = inherited_environment();
    spec.timeout = command.timeout;
    let outcome = emi_exec::run(&spec);
    match outcome.status {
        Status::NotLaunched => DoneResult::incomplete(format!(
            "emi done could not be started: {}",
            outcome.launch_error.unwrap_or_default()
        )),
        Status::TimedOut => DoneResult::incomplete(format!(
            "emi done did not finish within {} seconds and was stopped",
            command.timeout.as_secs()
        )),
        Status::Exited if outcome.stdout_bytes > outcome.stdout.len() as u64 => {
            DoneResult::incomplete(
                "emi done printed more than the hook keeps, so its receipt is cut off",
            )
        }
        Status::Exited => receipt::interpret(outcome.exit_code, &outcome.stdout, &outcome.stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_names_the_session_baseline_only_when_given() {
        let repo = Path::new("/r");
        assert_eq!(scope_args(repo, None), ["--repo", "/r"]);
        assert_eq!(
            scope_args(repo, Some("abc")),
            [
                "--repo",
                "/r",
                "--baseline",
                "abc",
                "--baseline-source",
                "session"
            ]
        );
    }

    #[test]
    fn the_manual_command_quotes_what_needs_it() {
        let args = scope_args(Path::new("/my repo"), None);
        assert_eq!(manual_command(&args), "emi done --repo '/my repo'");
    }
}
