//! Old tests and test-runner configuration the candidate changed. Editing
//! a test file the baseline had, or the files that decide how tests run, is
//! the usual way an agent makes a failing suite pass, so each change is
//! shown to a person for an approval, bound to the exact version the
//! candidate holds. Until approved, the file runs in its baseline form;
//! once approved, the candidate's version runs and counts as evidence.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};

use crate::Error;
use crate::regression::{Regression, Restored, Scope, Status};
use crate::state::Approval;

/// What marks a deleted file in an approval, where a modified or added one
/// has the blob id of its content.
pub const DELETED: &str = "deleted";

/// What sort of file changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    /// A test file the baseline had.
    OldTest,
    /// A file that decides how tests run (`conftest.py`, `pyproject.toml`).
    RunnerConfig,
}

impl What {
    fn as_str(self) -> &'static str {
        match self {
            What::OldTest => "old-test",
            What::RunnerConfig => "runner-config",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Modified,
    Deleted,
    /// Only test-runner configuration can be added: an added test is new.
    Added,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Modified => "modified",
            Kind::Deleted => "deleted",
            Kind::Added => "added",
        }
    }
}

/// A file the candidate changed that a person must decide about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    pub what: What,
    pub kind: Kind,
    /// The candidate's version of the file (its blob id), or [`DELETED`].
    pub content: String,
}

impl Changed {
    /// The name `--approve` takes: `old-test:PATH` or `runner-config:PATH`.
    pub fn item(&self) -> String {
        format!("{}:{}", self.what.as_str(), self.path)
    }

    /// The approval that would allow this change as it stands.
    pub fn approval(&self, baseline: &str) -> Approval {
        Approval {
            item: self.item(),
            baseline: baseline.to_owned(),
            content: self.content.clone(),
        }
    }
}

/// Which sort of file a changed path is, if it needs a decision. A file
/// that is both a test and runner configuration (`tests/conftest.py`) is
/// runner configuration.
fn classify(path: &str, globs: &[String]) -> Option<What> {
    if emi_controller::runner_config::matches(path) {
        Some(What::RunnerConfig)
    } else if globs.iter().any(|g| emi_scan::walk::fnmatch(path, g)) {
        Some(What::OldTest)
    } else {
        None
    }
}

fn kind_of(what: What, change: &str) -> Option<Kind> {
    match (what, change) {
        (_, "delete") => Some(Kind::Deleted),
        (_, "modify") => Some(Kind::Modified),
        (What::RunnerConfig, "add") => Some(Kind::Added),
        _ => None,
    }
}

/// Whether `rev` only added lines to `path`: every line the baseline had is
/// still there, in order. Tests appended to an old test file rewrite
/// nothing, so there is nothing for a person to decide; the file still
/// runs in its baseline form as regression evidence, and the added tests
/// are judged as change evidence. Anything git cannot count (a binary
/// file) is not an addition.
fn only_adds(view: &Path, baseline: &str, rev: &str, path: &str) -> bool {
    let Ok(out) = emi_git::run(view, &["diff", "--numstat", baseline, rev, "--", path]) else {
        return false;
    };
    let text = String::from_utf8_lossy(&out);
    let mut fields = text.split_whitespace();
    let added = fields.next().and_then(|n| n.parse::<u64>().ok());
    let deleted = fields.next().and_then(|n| n.parse::<u64>().ok());
    matches!((added, deleted), (Some(a), Some(0)) if a > 0)
}

/// The files `rev` changed that need a decision, sorted by path: baseline
/// test files (matching `globs`) it modified or deleted, and test-runner
/// configuration it modified, deleted or added. `view` is a clean view of
/// the repository holding `rev`.
pub fn find(
    view: &Path,
    baseline: &str,
    rev: &str,
    globs: &[String],
) -> Result<Vec<Changed>, Error> {
    let changes = emi_controller::verify::changes(view, baseline, rev).map_err(|e| {
        Error::Usage(format!(
            "cannot compare the candidate with the baseline: {e}"
        ))
    })?;
    let mut out = Vec::new();
    for (path, change) in changes {
        let Some(what) = classify(&path, globs) else {
            continue;
        };
        let Some(kind) = kind_of(what, change) else {
            continue;
        };
        if what == What::OldTest && kind == Kind::Modified && only_adds(view, baseline, rev, &path)
        {
            continue;
        }
        let content = if kind == Kind::Deleted {
            DELETED.to_owned()
        } else {
            let blob = emi_git::run(view, &["rev-parse", &format!("{rev}:{path}")])
                .map_err(|e| Error::Usage(format!("cannot read {path} of the candidate: {e}")))?;
            String::from_utf8_lossy(&blob).trim().to_owned()
        };
        out.push(Changed {
            path,
            what,
            kind,
            content,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// A changed file as the receipt lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub changed: Changed,
    /// How its baseline form did on the candidate. Unknown once approved:
    /// the candidate's version ran instead.
    pub restored: Restored,
    pub approved: bool,
}

/// Mark which changes `approvals` allow as they stand. Results are not
/// known yet; see [`settle`].
pub fn mark(changed: Vec<Changed>, approvals: &[Approval], baseline: &str) -> Vec<Entry> {
    changed
        .into_iter()
        .map(|c| Entry {
            approved: crate::state::approved(approvals, &c.approval(baseline)),
            restored: Restored::Unknown,
            changed: c,
        })
        .collect()
}

/// Which files the verification should treat as what.
pub fn scope(entries: &[Entry]) -> Scope {
    let mut scope = Scope::default();
    for e in entries {
        let path = e.changed.path.clone();
        match (e.approved, e.changed.what, e.changed.kind) {
            (true, _, _) => {
                scope.approved.insert(path);
            }
            (false, What::OldTest, _) => {
                scope.restored.insert(path);
            }
            (false, What::RunnerConfig, Kind::Added) => scope.added_runner.push(path),
            (false, What::RunnerConfig, _) => {}
        }
    }
    scope
}

/// The paths the contract must leave out of its trusted set: approved
/// files run as the candidate left them.
pub fn approved_paths(entries: &[Entry]) -> BTreeSet<String> {
    entries
        .iter()
        .filter(|e| e.approved)
        .map(|e| e.changed.path.clone())
        .collect()
}

/// Fill in how each unapproved file's baseline form did, from the run.
pub fn settle(entries: &mut [Entry], regression: &Regression) {
    for e in entries {
        e.restored = match (e.approved, e.changed.what) {
            (true, _) => Restored::Unknown,
            (false, What::OldTest) => regression.restored_result(&e.changed.path),
            (false, What::RunnerConfig) if regression.status == Status::Unknown => {
                Restored::Unknown
            }
            (false, What::RunnerConfig) => regression.clean(),
        };
    }
}

impl Entry {
    pub fn to_value(&self) -> Value {
        json!({"item": self.changed.item(), "type": self.changed.what.as_str(),
               "path": self.changed.path, "kind": self.changed.kind.as_str(),
               "restored_result": self.restored.as_str(), "approved": self.approved})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit_file, git, git_out, repo};

    fn changed(path: &str, what: What, kind: Kind, content: &str) -> Changed {
        Changed {
            path: path.into(),
            what,
            kind,
            content: content.into(),
        }
    }

    #[test]
    fn tests_and_runner_configuration_changes_are_found_and_others_are_not() {
        let dir = repo();
        for (name, body) in [
            ("tests/test_a.py", "a\n"),
            ("tests/test_b.py", "b\n"),
            ("tests/test_c.py", "c\n"),
            ("src/app.py", "x\n"),
            ("pyproject.toml", "[tool]\n"),
            ("setup.cfg", "[x]\n"),
            ("tests/conftest.py", "# c\n"),
        ] {
            commit_file(dir.path(), name, body, name);
        }
        let baseline = git_out(dir.path(), &["rev-parse", "HEAD"]);
        commit_file(dir.path(), "tests/test_a.py", "a2\n", "edit a");
        git(dir.path(), &["rm", "-q", "tests/test_b.py"]);
        git(dir.path(), &["mv", "tests/test_c.py", "tests/test_c2.py"]);
        commit_file(dir.path(), "tests/test_new.py", "n\n", "add a test");
        commit_file(dir.path(), "src/app.py", "y\n", "edit source");
        commit_file(
            dir.path(),
            "pyproject.toml",
            "[tool]\ndep = 1\n",
            "add a dependency",
        );
        git(dir.path(), &["rm", "-q", "setup.cfg"]);
        commit_file(dir.path(), "conftest.py", "# new\n", "add conftest");
        commit_file(
            dir.path(),
            "tests/conftest.py",
            "# edited\n",
            "edit conftest",
        );
        let head = git_out(dir.path(), &["rev-parse", "HEAD"]);
        let view = emi_git::CleanView::of(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        let found = find(view.path(), &baseline, &head, &["tests/*".to_owned()])
            .unwrap_or_else(|e| panic!("{e}"));
        let shown: Vec<(&str, What, Kind)> = found
            .iter()
            .map(|c| (c.path.as_str(), c.what, c.kind))
            .collect();
        assert_eq!(
            shown,
            [
                ("conftest.py", What::RunnerConfig, Kind::Added),
                ("pyproject.toml", What::RunnerConfig, Kind::Modified),
                ("setup.cfg", What::RunnerConfig, Kind::Deleted),
                ("tests/conftest.py", What::RunnerConfig, Kind::Modified),
                ("tests/test_a.py", What::OldTest, Kind::Modified),
                ("tests/test_b.py", What::OldTest, Kind::Deleted),
                ("tests/test_c.py", What::OldTest, Kind::Deleted),
            ],
            "a rename deletes the old test; an added test and a source edit are not listed"
        );
        let blob = git_out(dir.path(), &["rev-parse", "HEAD:pyproject.toml"]);
        assert_eq!(found[1].content, blob);
        assert_eq!(found[2].content, DELETED);
        assert_eq!(found[1].item(), "runner-config:pyproject.toml");
        assert_eq!(found[4].item(), "old-test:tests/test_a.py");
    }

    #[test]
    fn an_approval_matches_only_the_version_it_was_given_for() {
        let change = changed("tests/t.py", What::OldTest, Kind::Modified, "c1");
        let approvals = vec![change.approval("base")];
        let marked = mark(vec![change.clone()], &approvals, "base");
        assert!(marked[0].approved);
        let edited = Changed {
            content: "c2".into(),
            ..change.clone()
        };
        assert!(!mark(vec![edited], &approvals, "base")[0].approved);
        assert!(!mark(vec![change], &approvals, "other")[0].approved);
    }

    #[test]
    fn the_scope_restores_unapproved_tests_and_releases_approved_files() {
        let entries = mark(
            vec![
                changed("tests/a.py", What::OldTest, Kind::Modified, "1"),
                changed("tests/b.py", What::OldTest, Kind::Deleted, DELETED),
                changed("pyproject.toml", What::RunnerConfig, Kind::Modified, "2"),
                changed("conftest.py", What::RunnerConfig, Kind::Added, "3"),
            ],
            &[changed("tests/b.py", What::OldTest, Kind::Deleted, DELETED).approval("base")],
            "base",
        );
        let s = scope(&entries);
        assert_eq!(s.restored.into_iter().collect::<Vec<_>>(), ["tests/a.py"]);
        assert_eq!(s.approved.into_iter().collect::<Vec<_>>(), ["tests/b.py"]);
        assert_eq!(s.added_runner, ["conftest.py"]);
        assert_eq!(approved_paths(&entries).len(), 1);
    }

    #[test]
    fn settling_reads_each_files_result_from_the_run() {
        let mut entries = mark(
            vec![
                changed("tests/a.py", What::OldTest, Kind::Modified, "1"),
                changed("tests/b.py", What::OldTest, Kind::Modified, "2"),
                changed("pyproject.toml", What::RunnerConfig, Kind::Modified, "3"),
            ],
            &[],
            "base",
        );
        let regression = Regression {
            status: Status::Held,
            tests_run: Some(3),
            failed: Vec::new(),
            missing: Vec::new(),
            reasons: Vec::new(),
            complete: true,
            attributed: vec!["tests/a.py::t".into()],
        };
        settle(&mut entries, &regression);
        let got: Vec<Restored> = entries.iter().map(|e| e.restored).collect();
        assert_eq!(got, [Restored::Fails, Restored::Passes, Restored::Unknown]);
    }
}
