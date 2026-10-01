// a test that cannot set itself up must fail loudly, not pass
#![allow(clippy::expect_used)]

use super::*;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap_or(root)).unwrap_or_default();
    std::fs::write(path, text).unwrap_or_default();
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn test_files_are_those_named_by_the_globs_or_by_convention() {
    let globs = names(&["checks/*"]);
    let tests = TestFiles { globs: &globs };
    for yes in [
        "checks/a.py",
        "checks/data/case.json",
        "tests/test_a.py",
        "pkg/test_b.py",
        "pkg/b_test.py",
        "conftest.py",
        "pkg/conftest.py",
    ] {
        assert!(tests.matches(yes), "{yes}");
    }
    for no in [
        "src/a.py",
        "src/contest.py",
        "README.md",
        "pkg/testing_utils.py",
    ] {
        assert!(!tests.matches(no), "{no}");
    }
}

#[test]
fn candidate_test_files_replace_the_baselines_and_product_code_stays() {
    let (from, to) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    write(from.path(), "src/a.py", "candidate product\n");
    write(from.path(), "tests/test_a.py", "candidate test\n");
    write(from.path(), "tests/data/x.json", "{}\n");
    write(from.path(), "conftest.py", "candidate conftest\n");
    write(to.path(), "src/a.py", "baseline product\n");
    write(to.path(), "tests/test_a.py", "baseline test\n");
    write(to.path(), "tests/test_old.py", "baseline only\n");
    let files = names(&[
        "src/a.py",
        "tests/test_a.py",
        "tests/data/x.json",
        "conftest.py",
    ]);
    let tree = Tree {
        dir: from.path(),
        files: &files,
        symlinks: &[],
    };
    let globs = names(&["tests/*"]);
    let links = overlay_tests(&tree, to.path(), &TestFiles { globs: &globs }).expect("overlay");
    assert!(links.is_empty());
    let got = |rel: &str| read(to.path(), rel);
    assert_eq!(got("src/a.py").as_deref(), Some("baseline product\n"));
    assert_eq!(got("tests/test_a.py").as_deref(), Some("candidate test\n"));
    assert_eq!(got("tests/data/x.json").as_deref(), Some("{}\n"));
    assert_eq!(got("conftest.py").as_deref(), Some("candidate conftest\n"));
    // a test the candidate deleted is still there; only added ids are run
    assert_eq!(got("tests/test_old.py").as_deref(), Some("baseline only\n"));
}

#[test]
fn symlinked_test_files_are_reported_not_followed() {
    let (from, to) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    let files: Vec<String> = Vec::new();
    let symlinks = names(&["tests/test_link.py", "src/link.py"]);
    let tree = Tree {
        dir: from.path(),
        files: &files,
        symlinks: &symlinks,
    };
    let globs = names(&["tests/*"]);
    let links = overlay_tests(&tree, to.path(), &TestFiles { globs: &globs }).expect("overlay");
    assert_eq!(
        links.into_iter().collect::<Vec<_>>(),
        vec!["tests/test_link.py"]
    );
    assert!(read(to.path(), "tests/test_link.py").is_none());
}

#[cfg(unix)]
#[test]
fn a_link_in_the_baseline_cannot_lead_the_overlay_outside_the_workspace() {
    let (from, to, outside) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    write(from.path(), "tests/test_a.py", "candidate test\n");
    // the baseline's `tests` is a link to a directory elsewhere
    std::os::unix::fs::symlink(outside.path(), to.path().join("tests")).expect("symlink");
    let files = names(&["tests/test_a.py"]);
    let tree = Tree {
        dir: from.path(),
        files: &files,
        symlinks: &[],
    };
    let globs = names(&["tests/*"]);
    let placed = overlay_tests(&tree, to.path(), &TestFiles { globs: &globs });
    assert!(placed.is_err(), "the write must be refused");
    assert!(read(outside.path(), "test_a.py").is_none());
}

#[cfg(unix)]
#[test]
fn a_file_that_is_a_link_in_the_baseline_is_replaced_not_written_through() {
    let (from, to, outside) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    write(from.path(), "tests/test_a.py", "candidate test\n");
    write(outside.path(), "target.py", "untouched\n");
    std::fs::create_dir_all(to.path().join("tests")).expect("dir");
    std::os::unix::fs::symlink(
        outside.path().join("target.py"),
        to.path().join("tests/test_a.py"),
    )
    .expect("symlink");
    let files = names(&["tests/test_a.py"]);
    let tree = Tree {
        dir: from.path(),
        files: &files,
        symlinks: &[],
    };
    let globs = names(&["tests/*"]);
    overlay_tests(&tree, to.path(), &TestFiles { globs: &globs }).expect("overlay");
    assert_eq!(
        read(to.path(), "tests/test_a.py").as_deref(),
        Some("candidate test\n")
    );
    assert_eq!(
        read(outside.path(), "target.py").as_deref(),
        Some("untouched\n")
    );
}
