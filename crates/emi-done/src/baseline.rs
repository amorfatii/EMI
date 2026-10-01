//! Where a task started: the baseline a candidate is compared against.

use std::path::Path;

use emi_diff::snapshot;

use crate::Error;

/// How the caller came by an explicit baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Given {
    /// The person named it (`--baseline REF`).
    Explicit,
    /// The hook recorded it when the session began.
    Session,
}

impl Given {
    fn source(self) -> &'static str {
        match self {
            Given::Explicit => "explicit",
            Given::Session => "session",
        }
    }
}

/// A resolved baseline: a full commit id and where it came from
/// (`explicit`, `session` or `merge-base:<branch>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Baseline {
    pub sha: String,
    pub source: String,
}

/// The rule when no baseline was given, on already resolved commits: the
/// merge base of HEAD and the default branch, unless that is HEAD itself.
/// There the branch has not split off (HEAD is on the default branch, or
/// behind it), no earlier commit says where the task began, and guessing
/// one would measure the wrong change.
pub fn from_merge_base(head: &str, merge_base: &str, branch: &str) -> Result<Baseline, Error> {
    if merge_base == head {
        return Err(Error::Usage(format!(
            "HEAD is on the default branch ({branch}), so there is no point where this work \
             split off; pass --baseline REF (the hook passes the session baseline)"
        )));
    }
    Ok(Baseline {
        sha: merge_base.to_owned(),
        source: format!("merge-base:{branch}"),
    })
}

/// The baseline for `repo`: the given one, or the merge base with
/// `default_branch`.
pub fn resolve(
    repo: &Path,
    given: Option<(&str, Given)>,
    default_branch: &str,
) -> Result<Baseline, Error> {
    if let Some((reference, how)) = given {
        let sha = snapshot::resolve_ref(repo, reference)
            .map_err(|e| Error::Usage(format!("baseline {reference}: {e}")))?;
        return Ok(Baseline {
            sha,
            source: how.source().to_owned(),
        });
    }
    let head = snapshot::resolve_ref(repo, "HEAD")
        .map_err(|e| Error::Usage(format!("HEAD does not resolve: {e}")))?;
    let branch = snapshot::resolve_ref(repo, default_branch).map_err(|e| {
        Error::Usage(format!(
            "the default branch {default_branch} does not resolve ({e}); \
             pass --baseline REF or fix default_branch in .emi/config.json"
        ))
    })?;
    let base = snapshot::merge_base(repo, &head, &branch).map_err(|e| {
        Error::Usage(format!(
            "no common history between HEAD and {default_branch} ({e}); pass --baseline REF"
        ))
    })?;
    from_merge_base(&head, &base, default_branch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit_file, git, git_out, repo};

    #[test]
    fn the_merge_base_is_the_baseline_unless_it_is_head() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let got = from_merge_base(&a, &b, "main").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (got.sha, got.source),
            (b.clone(), "merge-base:main".to_owned())
        );
        let err = from_merge_base(&a, &a, "main").err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("pass --baseline")));
    }

    #[test]
    fn a_feature_branch_resolves_to_where_it_split_off() {
        let dir = repo();
        let split = git_out(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        commit_file(dir.path(), "a.txt", "one", "work");
        let got = resolve(dir.path(), None, "main").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            got,
            Baseline {
                sha: split,
                source: "merge-base:main".into()
            }
        );
    }

    #[test]
    fn on_the_default_branch_a_baseline_must_be_given() {
        let dir = repo();
        commit_file(dir.path(), "a.txt", "one", "more");
        let err = resolve(dir.path(), None, "main")
            .err()
            .map(|e| e.to_string());
        assert!(
            err.is_some_and(|m| m.contains("default branch")),
            "no guessing"
        );
    }

    #[test]
    fn a_given_baseline_wins_and_names_its_source() {
        let dir = repo();
        let first = git_out(dir.path(), &["rev-parse", "HEAD"]);
        commit_file(dir.path(), "a.txt", "one", "more");
        for (how, source) in [(Given::Explicit, "explicit"), (Given::Session, "session")] {
            let got = resolve(dir.path(), Some(("HEAD~1", how)), "main")
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                (got.sha.as_str(), got.source.as_str()),
                (first.as_str(), source)
            );
        }
    }

    #[test]
    fn what_does_not_resolve_is_an_error_not_a_guess() {
        let dir = repo();
        let missing = resolve(dir.path(), Some(("no-such-ref", Given::Explicit)), "main");
        assert!(missing.is_err());
        let no_branch = resolve(dir.path(), None, "develop")
            .err()
            .map(|e| e.to_string());
        assert!(no_branch.is_some_and(|m| m.contains("develop")));
    }
}
