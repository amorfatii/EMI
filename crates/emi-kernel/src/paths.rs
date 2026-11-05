//! What a repository-relative path is, from its name alone.

/// Directories whose files are tests wherever they sit.
const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "specs",
    "testdata",
    "testing",
];

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `path` (forward slashes) names a test file: under a test
/// directory, or named like one (`test_x.py`, `x_test.py`, `x.test.js`).
pub fn is_test(path: &str) -> bool {
    let name = name(path);
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    if dirs.iter().any(|d| TEST_DIRS.contains(d)) {
        return true;
    }
    if (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
    {
        return true;
    }
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() > 2
        && parts[1..parts.len() - 1]
            .iter()
            .any(|p| matches!(*p, "test" | "spec"))
}

#[cfg(test)]
mod tests {
    use super::is_test;

    #[test]
    fn tests_by_directory_and_by_name() {
        for (path, want) in [
            ("tests/conftest.py", true),
            ("src/test_x.py", true),
            ("src/x_test.py", true),
            ("web/a.test.js", true),
            ("src/app.py", false),
            ("src/contest.py", false),
            ("testing.py", false),
        ] {
            assert_eq!(is_test(path), want, "{path}");
        }
    }
}
