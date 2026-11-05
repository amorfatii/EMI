//! Which files are tests, and the baseline's tree with the candidate's
//! test files laid over it: the workspace in which an added test meets the
//! old product code.

use std::collections::BTreeSet;
use std::path::Path;

use crate::sandbox::Copy;
use emi_tree::snapshot::assert_contained;

/// The files that are tests: those the configuration names, those that
/// look like tests by convention, and `conftest.py` wherever it is (a new
/// fixture there must travel with the tests that use it, or they would
/// "fail at the baseline" for a reason that has nothing to do with the
/// code under test).
pub struct TestFiles<'a> {
    pub globs: &'a [String],
}

impl TestFiles<'_> {
    pub fn matches(&self, path: &str) -> bool {
        self.globs
            .iter()
            .any(|g| emi_kernel::glob::fnmatch(path, g))
            || emi_kernel::paths::is_test(path)
            || path.rsplit('/').next() == Some("conftest.py")
    }
}

/// A materialized tree and what it holds.
pub struct Tree<'a> {
    pub dir: &'a Path,
    pub files: &'a [String],
    pub symlinks: &'a [String],
}

impl<'a> Tree<'a> {
    pub fn of(copy: &'a Copy) -> Tree<'a> {
        Tree {
            dir: &copy.dir,
            files: &copy.files,
            symlinks: &copy.symlinks,
        }
    }
}

/// Copy the candidate's test files into `to` (a copy of the baseline),
/// replacing the baseline's versions. Returns the candidate's test files
/// that are symbolic links: they are not followed, so tests in them cannot
/// be run at the baseline.
pub fn overlay_tests(
    from: &Tree,
    to: &Path,
    tests: &TestFiles,
) -> Result<BTreeSet<String>, String> {
    for path in from.files.iter().filter(|p| tests.matches(p)) {
        place(from.dir, to, path)?;
    }
    Ok(from
        .symlinks
        .iter()
        .filter(|p| tests.matches(p))
        .cloned()
        .collect())
}

fn place(from: &Path, to: &Path, rel: &str) -> Result<(), String> {
    let (src, dst) = (from.join(rel), to.join(rel));
    if let Some(parent) = dst.parent() {
        // a link the baseline tree holds must not lead the write outside it
        assert_contained(to, parent)?;
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot place {rel}: {e}"))?;
    }
    if dst
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        std::fs::remove_file(&dst).map_err(|e| format!("cannot replace {rel}: {e}"))?;
    }
    std::fs::copy(&src, &dst)
        .map(|_| ())
        .map_err(|e| format!("cannot place {rel}: {e}"))
}

#[cfg(test)]
mod tests;
