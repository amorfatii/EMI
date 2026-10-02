//! The candidate: the working tree as it is, or one commit. Either way it
//! is read once into a commit inside a clean view of the repository, so
//! every comparison the done check makes reads the same content.

use std::path::Path;

use emi_diff::snapshot;

use crate::Error;

/// What the person asked to check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    /// The working tree, tracked and untracked-but-not-ignored files.
    Worktree,
    /// A commit or ref.
    Ref(String),
}

/// A candidate read into the clean view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// `worktree`, or the commit id.
    pub id: String,
    /// The tree id of the content: equal content, equal id.
    pub content_id: String,
    /// The commit holding the content, readable through the clean view.
    pub rev: String,
}

fn tree_of(view: &Path, rev: &str) -> Result<String, Error> {
    let out = emi_git::run(view, &["rev-parse", &format!("{rev}^{{tree}}")])
        .map_err(|e| Error::Usage(format!("cannot read the candidate's tree: {e}")))?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// Read `spec` from `repo` into `view` (a [`emi_git::CleanView`] of it).
pub fn read(repo: &Path, view: &Path, spec: &Spec) -> Result<Candidate, Error> {
    let (id, rev) = match spec {
        Spec::Ref(reference) => {
            let sha = snapshot::resolve_ref(repo, reference)
                .map_err(|e| Error::Usage(format!("candidate {reference}: {e}")))?;
            (sha.clone(), sha)
        }
        Spec::Worktree => {
            let scratch = snapshot::TempDir::new("emi-done-index-")
                .map_err(|e| Error::Usage(format!("working tree: {e}")))?;
            let rev = snapshot::commit_worktree(repo, view, scratch.path())
                .map_err(|e| Error::Usage(format!("cannot read the working tree: {e}")))?;
            (crate::WORKTREE.to_owned(), rev)
        }
    };
    let content_id = tree_of(view, &rev)?;
    Ok(Candidate {
        id,
        content_id,
        rev,
    })
}

/// The tree id of the candidate as it is now, for comparing with
/// [`Candidate::content_id`] after a long run: a working tree that changed
/// while its tests ran was not what was measured.
pub fn content_id_now(repo: &Path, view: &Path, spec: &Spec) -> Result<String, Error> {
    read(repo, view, spec).map(|c| c.content_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit_file, git_out, repo};

    fn read_now(dir: &Path, spec: &Spec) -> Candidate {
        let view = emi_git::CleanView::of(dir).unwrap_or_else(|e| panic!("{e}"));
        read(dir, view.path(), spec).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn equal_content_has_an_equal_id_whether_committed_or_not() {
        let dir = repo();
        let head = git_out(dir.path(), &["rev-parse", "HEAD"]);
        let clean = read_now(dir.path(), &Spec::Worktree);
        let committed = read_now(dir.path(), &Spec::Ref("HEAD".into()));
        assert_eq!(
            (clean.id.as_str(), committed.id.as_str()),
            (crate::WORKTREE, head.as_str())
        );
        assert_eq!(clean.content_id, committed.content_id);
        std::fs::write(dir.path().join("new.txt"), "x\n").unwrap_or_default();
        let edited = read_now(dir.path(), &Spec::Worktree);
        assert_ne!(
            edited.content_id, clean.content_id,
            "an untracked file is content"
        );
        commit_file(dir.path(), "new.txt", "x\n", "commit it");
        let after = read_now(dir.path(), &Spec::Ref("HEAD".into()));
        assert_eq!(after.content_id, edited.content_id);
    }

    #[test]
    fn a_ref_that_does_not_resolve_is_an_error() {
        let dir = repo();
        let view = emi_git::CleanView::of(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert!(read(dir.path(), view.path(), &Spec::Ref("nope".into())).is_err());
    }
}
