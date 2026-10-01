//! Old tests the candidate changed. A test file the baseline had and the
//! candidate edited or deleted is the usual way an agent makes a failing
//! suite pass, so each one is shown to a person for an approval, bound to
//! the exact version the candidate holds.

use std::path::Path;

use serde_json::{Value, json};

use crate::Error;
use crate::regression::{Regression, Restored};
use crate::state::Approval;

/// What marks a deleted file in an approval, where a modified one has the
/// blob id of its new content.
pub const DELETED: &str = "deleted";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Modified,
    Deleted,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Modified => "modified",
            Kind::Deleted => "deleted",
        }
    }
}

/// A baseline test file the candidate modified or deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    pub path: String,
    pub kind: Kind,
    /// The candidate's version of the file (its blob id), or [`DELETED`].
    pub content: String,
}

impl Changed {
    /// The name `--approve` takes.
    pub fn item(&self) -> String {
        format!("old-test:{}", self.path)
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

/// The baseline files matching `globs` that `rev` modified or deleted,
/// sorted by path. `view` is a clean view of the repository holding `rev`.
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
    for (path, kind) in changes {
        let is_test = globs.iter().any(|g| emi_scan::walk::fnmatch(&path, g));
        match (kind, is_test) {
            ("delete", true) => out.push(Changed {
                path,
                kind: Kind::Deleted,
                content: DELETED.to_owned(),
            }),
            ("modify", true) => {
                let blob =
                    emi_git::run(view, &["rev-parse", &format!("{rev}:{path}")]).map_err(|e| {
                        Error::Usage(format!("cannot read {path} of the candidate: {e}"))
                    })?;
                out.push(Changed {
                    path,
                    kind: Kind::Modified,
                    content: String::from_utf8_lossy(&blob).trim().to_owned(),
                });
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// A changed old test as the receipt lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub changed: Changed,
    pub restored: Restored,
    pub approved: bool,
}

/// Judge each changed test: how its baseline version fared on the
/// candidate, and whether `approvals` allow the change as it stands.
pub fn entries(
    changed: Vec<Changed>,
    regression: &Regression,
    approvals: &[Approval],
    baseline: &str,
) -> Vec<Entry> {
    changed
        .into_iter()
        .map(|c| Entry {
            restored: regression.restored_result(&c.path),
            approved: crate::state::approved(approvals, &c.approval(baseline)),
            changed: c,
        })
        .collect()
}

impl Entry {
    pub fn to_value(&self) -> Value {
        json!({"item": self.changed.item(), "path": self.changed.path,
               "kind": self.changed.kind.as_str(),
               "restored_result": self.restored.as_str(), "approved": self.approved})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit_file, git, git_out, repo};

    #[test]
    fn modified_deleted_and_renamed_tests_are_found_and_others_are_not() {
        let dir = repo();
        for (name, body) in [
            ("tests/test_a.py", "a\n"),
            ("tests/test_b.py", "b\n"),
            ("tests/test_c.py", "c\n"),
            ("tests/test_d.py", "d\n"),
            ("src/app.py", "x\n"),
        ] {
            commit_file(dir.path(), name, body, name);
        }
        let baseline = git_out(dir.path(), &["rev-parse", "HEAD"]);
        commit_file(dir.path(), "tests/test_a.py", "a2\n", "edit a");
        git(dir.path(), &["rm", "-q", "tests/test_b.py"]);
        git(dir.path(), &["mv", "tests/test_c.py", "tests/test_c2.py"]);
        commit_file(dir.path(), "tests/test_new.py", "n\n", "add a test");
        commit_file(dir.path(), "src/app.py", "y\n", "edit source");
        let head = git_out(dir.path(), &["rev-parse", "HEAD"]);
        let view = emi_git::CleanView::of(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        let found = find(view.path(), &baseline, &head, &["tests/*".to_owned()])
            .unwrap_or_else(|e| panic!("{e}"));
        let shown: Vec<(&str, Kind)> = found.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(
            shown,
            [
                ("tests/test_a.py", Kind::Modified),
                ("tests/test_b.py", Kind::Deleted),
                ("tests/test_c.py", Kind::Deleted),
            ],
            "a rename is a deletion of the old test; additions and source edits are not listed"
        );
        let blob = git_out(dir.path(), &["rev-parse", "HEAD:tests/test_a.py"]);
        assert_eq!(found[0].content, blob);
        assert_eq!(found[1].content, DELETED);
        assert_eq!(found[0].item(), "old-test:tests/test_a.py");
    }

    #[test]
    fn an_approval_matches_only_the_version_it_was_given_for() {
        let change = Changed {
            path: "tests/t.py".into(),
            kind: Kind::Modified,
            content: "c1".into(),
        };
        let regression = Regression::unknown(vec!["x".into()]);
        let approvals = vec![change.approval("base")];
        let judged = entries(vec![change.clone()], &regression, &approvals, "base");
        assert!(judged[0].approved);
        assert_eq!(judged[0].restored, Restored::Unknown);
        let edited = Changed {
            content: "c2".into(),
            ..change
        };
        assert!(!entries(vec![edited], &regression, &approvals, "base")[0].approved);
    }
}
