//! Files that decide how tests run rather than what they test: a
//! `conftest.py` can rewrite every test's outcome, a `pytest.ini` can load
//! a plugin or deselect tests, `sitecustomize.py` runs at interpreter
//! start. Task specs trust and protect them; a candidate that adds one
//! needs review, since restoring trusted files only undoes changes to
//! files the baseline had.

/// File names (fnmatch patterns on the last path component).
pub const NAMES: &[&str] = &[
    "conftest.py",
    "pytest.ini",
    "pyproject.toml",
    "setup.cfg",
    "tox.ini",
    "noxfile.py",
    "sitecustomize.py",
    "usercustomize.py",
    "package.json",
    "jest.config.*",
    "vitest.config.*",
    "vite.config.*",
    ".mocharc*",
    "karma.conf.*",
];

/// Whether `path` is test-runner configuration, wherever it lives.
pub fn matches(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    NAMES.iter().any(|n| emi_scan::walk::fnmatch(name, n))
}

/// Path globs matching the files at the root and in any directory.
pub fn globs() -> Vec<String> {
    NAMES
        .iter()
        .flat_map(|n| [(*n).to_owned(), format!("*/{n}")])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_anywhere_and_only_by_name() {
        assert!(matches("tests/sub/conftest.py"));
        assert!(matches("web/jest.config.ts"));
        assert!(matches("pyproject.toml"));
        assert!(!matches("src/conftest_helpers.py"));
        assert!(globs().contains(&"*/conftest.py".to_owned()));
    }
}
