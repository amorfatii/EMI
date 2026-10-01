//! What the done check keeps between runs: the authority directory for its
//! derived contracts and the approvals. It lives under the user's state
//! directory, keyed by the repository, and never inside the repository: the
//! proposer works in the repository and must not be able to rewrite what
//! the controller decides from.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;

const APPROVAL_SCHEMA: &str = "emi-done-approval-0.1";

/// `$XDG_STATE_HOME/emi`, or `~/.local/state/emi`. A relative
/// `XDG_STATE_HOME` is ignored, as the specification says.
pub fn root(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, Error> {
    let base = match xdg_state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => {
            let home = home
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or_else(|| {
                    Error::Usage(
                        "cannot find a state directory: set XDG_STATE_HOME or HOME".to_owned(),
                    )
                })?;
            home.join(".local").join("state")
        }
    };
    Ok(base.join("emi"))
}

/// [`root`] from the process environment.
pub fn root_from_env() -> Result<PathBuf, Error> {
    root(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// The directory for one repository: its name for people, a digest of its
/// canonical path for identity.
pub fn repo_dir(state_root: &Path, repo: &Path) -> PathBuf {
    let name: String = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let id = emi_ir::pytext::sha256_hex(repo.to_string_lossy().as_bytes());
    state_root
        .join("repos")
        .join(format!("{name}-{}", &id[..16]))
}

/// The authority directory the controller keeps contracts and evidence in.
pub fn authority(repo_dir: &Path) -> PathBuf {
    repo_dir.join("authority")
}

/// One recorded decision to allow a changed old test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub item: String,
    pub baseline: String,
    /// The candidate's version of the file (its blob id), or `deleted`.
    pub content: String,
}

impl Approval {
    fn to_value(&self) -> Value {
        json!({"schema": APPROVAL_SCHEMA, "item": self.item,
               "baseline": self.baseline, "content": self.content,
               "at": emi_diff::trajectory::utc_now_iso()})
    }

    fn from_value(v: &Value) -> Option<Approval> {
        if v["schema"] != APPROVAL_SCHEMA {
            return None;
        }
        let text = |k: &str| v[k].as_str().map(str::to_owned);
        Some(Approval {
            item: text("item")?,
            baseline: text("baseline")?,
            content: text("content")?,
        })
    }
}

fn approvals_file(repo_dir: &Path) -> PathBuf {
    repo_dir.join("approvals.jsonl")
}

/// Every recorded approval. A line that is not an approval means the
/// record is damaged: an error, never a shorter list.
pub fn load(repo_dir: &Path) -> Result<Vec<Approval>, Error> {
    let path = approvals_file(repo_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Usage(format!("{}: {e}", path.display()))),
    };
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| Approval::from_value(&v))
                .ok_or_else(|| {
                    Error::Usage(format!(
                        "{} line {} is not an approval; the record is damaged",
                        path.display(),
                        n + 1
                    ))
                })
        })
        .collect()
}

/// Append an approval unless an identical one is already recorded.
pub fn record(repo_dir: &Path, approval: &Approval) -> Result<(), Error> {
    if load(repo_dir)?.contains(approval) {
        return Ok(());
    }
    std::fs::create_dir_all(repo_dir)
        .map_err(|e| Error::Usage(format!("{}: {e}", repo_dir.display())))?;
    let path = approvals_file(repo_dir);
    let mut line = approval.to_value().to_string();
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| Error::Usage(format!("{}: {e}", path.display())))
}

/// Whether the person approved exactly this item, as it stands now.
pub fn approved(approvals: &[Approval], wanted: &Approval) -> bool {
    approvals.contains(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_follows_the_xdg_rule() {
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            root(os("/x/state"), os("/home/u")).ok(),
            Some(PathBuf::from("/x/state/emi"))
        );
        assert_eq!(
            root(os("relative"), os("/home/u")).ok(),
            Some(PathBuf::from("/home/u/.local/state/emi"))
        );
        assert_eq!(
            root(None, os("/home/u")).ok(),
            Some(PathBuf::from("/home/u/.local/state/emi"))
        );
        assert!(root(None, None).is_err(), "no guessing a directory");
        assert!(root(None, os("home")).is_err());
    }

    #[test]
    fn a_repository_gets_its_own_directory() {
        let a = repo_dir(Path::new("/s"), Path::new("/work/app"));
        let b = repo_dir(Path::new("/s"), Path::new("/other/app"));
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("/repos/app-"));
    }

    #[test]
    fn approvals_are_exact_and_a_damaged_record_is_an_error() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load(dir.path()).ok(), Some(Vec::new()));
        let a = Approval {
            item: "old-test:tests/t.py".into(),
            baseline: "b".repeat(40),
            content: "c".repeat(40),
        };
        record(dir.path(), &a).unwrap_or_else(|e| panic!("{e}"));
        record(dir.path(), &a).unwrap_or_else(|e| panic!("{e}"));
        let all = load(dir.path()).unwrap_or_default();
        assert_eq!(all, vec![a.clone()], "recorded once");
        assert!(approved(&all, &a));
        let edited = Approval {
            content: "d".repeat(40),
            ..a.clone()
        };
        assert!(
            !approved(&all, &edited),
            "a further edit needs a new approval"
        );
        let rebased = Approval {
            baseline: "e".repeat(40),
            ..a
        };
        assert!(!approved(&all, &rebased));
        std::fs::write(approvals_file(dir.path()), "not json\n").unwrap_or_default();
        assert!(load(dir.path()).is_err());
    }
}
