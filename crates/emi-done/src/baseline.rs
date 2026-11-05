//! Where a task started: the baseline a candidate is compared against.

use std::path::Path;

use emi_tree::snapshot;

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

/// The rule when no baseline was given, on already resolved commits. The
/// merge base of HEAD and the default branch is the baseline, even when it
/// is HEAD itself: a branch just cut from the default branch has not split
/// off yet, but it did start there. Two cases have no such point and need a
/// baseline from the caller: HEAD is on the default branch itself, so no
/// earlier commit says where the work began; or HEAD is detached at the
/// merge base, so nothing says what it belongs to. Guessing either would
/// measure the wrong change.
pub fn from_merge_base(
    head: &str,
    merge_base: &str,
    branch: &str,
    current: Option<&str>,
) -> Result<Baseline, Error> {
    let on_default = current == Some(branch);
    let detached_at_base = current.is_none() && merge_base == head;
    if on_default || detached_at_base {
        let place = if on_default {
            format!("HEAD is on the default branch ({branch})")
        } else {
            format!("HEAD is detached at its merge base with {branch}")
        };
        return Err(Error::Usage(format!(
            "{place}, so there is no point where this work began; pass --baseline REF (the hook \
             passes the session baseline)"
        )));
    }
    Ok(Baseline {
        sha: merge_base.to_owned(),
        source: format!("merge-base:{branch}"),
    })
}

/// The branch HEAD is on, `None` when it is detached.
fn current_branch(repo: &Path) -> Result<Option<String>, Error> {
    let out = emi_git::run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map_err(|e| Error::Usage(format!("cannot tell which branch HEAD is on: {e}")))?;
    let name = String::from_utf8_lossy(&out).trim().to_owned();
    Ok((name != "HEAD").then_some(name))
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
    from_merge_base(
        &head,
        &base,
        default_branch,
        current_branch(repo)?.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit_file, git, git_out, repo};

    #[test]
    fn the_merge_base_rule_by_where_head_is() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let cases: &[(&str, &str, Option<&str>, bool)] = &[
            // (head, merge base, current branch, needs a baseline)
            ("a", "b", Some("feature"), false),
            ("a", "a", Some("feature"), false),
            ("a", "b", Some("main"), true),
            ("a", "a", Some("main"), true),
            ("a", "b", None, false),
            ("a", "a", None, true),
        ];
        for (head, base, current, refused) in cases {
            let sha = |s: &str| if s == "a" { a.clone() } else { b.clone() };
            let got = from_merge_base(&sha(head), &sha(base), "main", *current);
            assert_eq!(got.is_err(), *refused, "{head} {base} {current:?}");
            if let Ok(ok) = got {
                assert_eq!(
                    (ok.sha, ok.source),
                    (sha(base), "merge-base:main".to_owned())
                );
            }
        }
        let err = from_merge_base(&a, &a, "main", Some("main"))
            .err()
            .map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("pass --baseline")));
    }

    #[test]
    fn a_branch_cut_from_the_default_branch_uses_where_it_was_cut() {
        let dir = repo();
        let tip = git_out(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        let got = resolve(dir.path(), None, "main").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            got,
            Baseline {
                sha: tip,
                source: "merge-base:main".into()
            }
        );
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
