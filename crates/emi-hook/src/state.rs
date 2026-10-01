//! Per-session hook state, kept outside the repository.
//!
//! The agent can edit anything inside the repository, including a counter
//! that decides when it is allowed to stop, so the state lives under the
//! user's state directory, keyed by the canonical repository path and the
//! session id. A missing file is a new session; a file that cannot be read
//! back is an error the person sees, never a silent reset to zero rounds.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::decision::State;

const SCHEMA: &str = "emi-hook-state-0.1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema: String,
    repo: String,
    session: String,
    rounds_used: u32,
    last_checked: Option<String>,
    baseline: Option<String>,
}

/// Where the state of one (repository, session) pair lives.
pub struct Store {
    file: PathBuf,
    repo: String,
    session: String,
}

/// The user's state directory: `$XDG_STATE_HOME`, else `~/.local/state`.
/// Relative values are ignored, as the XDG specification says.
pub fn state_home() -> Result<PathBuf, String> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    absolute("XDG_STATE_HOME")
        .or_else(|| absolute("HOME").map(|h| h.join(".local").join("state")))
        .ok_or_else(|| {
            "neither XDG_STATE_HOME nor HOME is an absolute path, so there is nowhere to keep the hook state".to_owned()
        })
}

fn short_hash(text: &str) -> String {
    emi_ir::pytext::sha256_hex(text.as_bytes())[..24].to_owned()
}

impl Store {
    /// `repo` must already be canonical: two spellings of one path must
    /// not get two states.
    pub fn open(state_home: &Path, repo: &Path, session: &str) -> Store {
        let repo = repo.to_string_lossy().into_owned();
        let file = state_home
            .join("emi")
            .join("hook")
            .join(short_hash(&repo))
            .join(format!("{}.json", short_hash(session)));
        Store {
            file,
            repo,
            session: session.to_owned(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.file
    }

    fn corrupt(&self, why: impl std::fmt::Display) -> String {
        format!(
            "hook state {} cannot be read ({why}); EMI will not guess the repair-round count. Delete the file to start the session's count again",
            self.file.display()
        )
    }

    /// The stored state; a new session when nothing was stored yet.
    pub fn load(&self) -> Result<State, String> {
        let text = match std::fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(self.corrupt(e)),
        };
        let stored: Stored = serde_json::from_str(&text).map_err(|e| self.corrupt(e))?;
        let checks = [
            (stored.schema == SCHEMA, "unknown schema"),
            (stored.repo == self.repo, "it belongs to another repository"),
            (
                stored.session == self.session,
                "it belongs to another session",
            ),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(self.corrupt(why));
        }
        Ok(State {
            rounds_used: stored.rounds_used,
            last_checked: stored.last_checked,
            baseline: stored.baseline,
        })
    }

    /// Write via a temporary file in the same directory and rename, so a
    /// reader never sees a torn file.
    pub fn save(&self, state: &State) -> Result<(), String> {
        let stored = Stored {
            schema: SCHEMA.to_owned(),
            repo: self.repo.clone(),
            session: self.session.clone(),
            rounds_used: state.rounds_used,
            last_checked: state.last_checked.clone(),
            baseline: state.baseline.clone(),
        };
        let text = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())? + "\n";
        let fail = |what: &str, e: std::io::Error| {
            format!("cannot {what} hook state {}: {e}", self.file.display())
        };
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail("create the directory for", e))?;
        }
        let tmp = self
            .file
            .with_extension(format!("json.tmp{}", std::process::id()));
        write_synced(&tmp, text.as_bytes()).map_err(|e| fail("write", e))?;
        std::fs::rename(&tmp, &self.file).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            fail("replace", e)
        })
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn store(home: &Path, session: &str) -> Store {
        Store::open(home, Path::new("/work/repo"), session)
    }

    #[test]
    fn a_missing_file_is_a_new_session() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(store(home.path(), "s1").load(), Ok(State::default()));
    }

    #[test]
    fn state_round_trips() {
        let home = tempfile::tempdir().expect("tempdir");
        let s = store(home.path(), "s1");
        let state = State {
            rounds_used: 2,
            last_checked: Some("abc".into()),
            baseline: Some("def".into()),
        };
        s.save(&state).expect("save");
        assert_eq!(s.load(), Ok(state));
    }

    #[test]
    fn sessions_and_repositories_do_not_share_state() {
        let home = tempfile::tempdir().expect("tempdir");
        let a = store(home.path(), "s1");
        let b = store(home.path(), "s2");
        let c = Store::open(home.path(), Path::new("/work/other"), "s1");
        a.save(&State {
            rounds_used: 3,
            ..State::default()
        })
        .expect("save");
        assert_eq!(b.load(), Ok(State::default()));
        assert_eq!(c.load(), Ok(State::default()));
        assert_ne!(a.path(), b.path());
        assert_ne!(a.path(), c.path());
    }

    #[test]
    fn the_state_lives_under_the_given_state_home() {
        let home = tempfile::tempdir().expect("tempdir");
        let s = store(home.path(), "s1");
        assert!(s.path().starts_with(home.path().join("emi").join("hook")));
    }

    #[test]
    fn corrupt_files_are_errors_not_resets() {
        let home = tempfile::tempdir().expect("tempdir");
        let s = store(home.path(), "s1");
        s.save(&State::default()).expect("save");
        for bad in [
            "not json",
            "",
            "{}",
            r#"{"schema":"other","repo":"/work/repo","session":"s1","rounds_used":0,"last_checked":null,"baseline":null}"#,
            r#"{"schema":"emi-hook-state-0.1","repo":"/elsewhere","session":"s1","rounds_used":0,"last_checked":null,"baseline":null}"#,
            r#"{"schema":"emi-hook-state-0.1","repo":"/work/repo","session":"s9","rounds_used":0,"last_checked":null,"baseline":null}"#,
            r#"{"schema":"emi-hook-state-0.1","repo":"/work/repo","session":"s1","rounds_used":-1,"last_checked":null,"baseline":null}"#,
            r#"{"schema":"emi-hook-state-0.1","repo":"/work/repo","session":"s1","rounds_used":0,"last_checked":null,"baseline":null,"extra":1}"#,
        ] {
            std::fs::write(s.path(), bad).expect("write");
            let err = s.load().expect_err(bad);
            assert!(err.contains(&s.path().display().to_string()), "{err}");
        }
    }

    #[test]
    fn saving_leaves_no_temporary_file() {
        let home = tempfile::tempdir().expect("tempdir");
        let s = store(home.path(), "s1");
        s.save(&State::default()).expect("save");
        s.save(&State::default()).expect("save again");
        let dir = s.path().parent().expect("parent");
        let names: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }
}
