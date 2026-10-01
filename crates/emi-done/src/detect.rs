//! `emi init`: read the repository and propose a configuration. Detection
//! only proposes; nothing is written until the person confirms, and a
//! guess that cannot be made is an error rather than a default.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Error;
use crate::config::{self, Config, DEFAULT_REPAIR_ROUNDS, OnUnproven, Tests};

/// The proposal and what it rests on.
#[derive(Debug)]
pub struct Detected {
    pub config: Config,
    /// One line per thing detected, in plain words.
    pub notes: Vec<String>,
    /// Things that will stop `emi done` from working until fixed.
    pub warnings: Vec<String>,
}

/// Top-level directories that are never the product source.
const NOT_SOURCE: &[&str] = &[
    "tests", "test", "docs", "doc", "examples", "scripts", "build", "dist",
];
const SOURCE_DIRS: &[&str] = &["src", "lib", "app"];
const VENVS: &[&str] = &[".venv", "venv"];

/// Tracked and untracked-but-not-ignored files: what a check would see.
fn files(repo: &Path) -> Result<Vec<String>, Error> {
    let raw = emi_git::run(
        repo,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .map_err(|e| Error::Usage(format!("cannot list the repository's files: {e}")))?;
    Ok(raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !VENVS.iter().any(|v| p.starts_with(&format!("{v}/"))))
        .collect())
}

fn is_python(path: &str) -> bool {
    path.ends_with(".py")
}

/// Python first; any other language is named and refused.
fn language(files: &[String]) -> Result<(), Error> {
    if files.iter().any(|f| is_python(f)) {
        return Ok(());
    }
    let others: Vec<&str> = [
        (".js", "JavaScript"),
        (".ts", "TypeScript"),
        (".go", "Go"),
        (".rs", "Rust"),
    ]
    .iter()
    .filter(|(ext, _)| files.iter().any(|f| f.ends_with(ext)))
    .map(|(_, name)| *name)
    .collect();
    Err(Error::Config(if others.is_empty() {
        "no Python files found; emi supports Python with pytest for now".to_owned()
    } else {
        format!(
            "{} is unsupported for now; emi supports Python with pytest first",
            others.join(" and ")
        )
    }))
}

fn top_dir(path: &str) -> Option<&str> {
    path.split_once('/').map(|(d, _)| d)
}

/// The product source directory: a conventional one that holds Python,
/// else the one top-level package, else the repository root.
fn source(files: &[String]) -> String {
    let holds_python = |dir: &str| {
        files
            .iter()
            .any(|f| is_python(f) && top_dir(f) == Some(dir))
    };
    if let Some(d) = SOURCE_DIRS.iter().find(|d| holds_python(d)) {
        return (*d).to_owned();
    }
    let mut packages: Vec<&str> = files
        .iter()
        .filter(|f| f.ends_with("/__init__.py") && f.matches('/').count() == 1)
        .filter_map(|f| top_dir(f))
        .filter(|d| !NOT_SOURCE.contains(d) && !d.starts_with('.'))
        .collect();
    packages.sort_unstable();
    packages.dedup();
    match packages.as_slice() {
        [one] => (*one).to_owned(),
        _ => ".".to_owned(),
    }
}

/// Globs covering the repository's test files, derived from where they are.
fn test_globs(files: &[String]) -> Result<Vec<String>, Error> {
    let tests: Vec<&String> = files
        .iter()
        .filter(|f| is_python(f) && emi_scan::classify::is_test(f))
        .collect();
    if tests.is_empty() {
        return Err(Error::Config(
            "no pytest tests found (test_*.py, *_test.py or a tests/ directory); \
             without tests there is no regression to check"
                .to_owned(),
        ));
    }
    let mut globs = Vec::new();
    for t in tests {
        let name = t.rsplit('/').next().unwrap_or(t);
        let glob = match (top_dir(t), name) {
            (Some(d @ ("tests" | "test")), _) => format!("{d}/*"),
            (Some(_), _) if t.contains("/tests/") => "*/tests/*".to_owned(),
            (Some(_), _) if t.contains("/test/") => "*/test/*".to_owned(),
            (_, n) if n.starts_with("test_") && t.contains('/') => "*/test_*.py".to_owned(),
            (_, n) if n.starts_with("test_") => "test_*.py".to_owned(),
            (_, n) if n.ends_with("_test.py") => "*_test.py".to_owned(),
            // a lone test-like file inside a package (a test helper such
            // as `pkg/testing/fixtures.py`) names itself: a glob over its
            // top directory would declare the whole package to be tests
            _ => t.clone(),
        };
        globs.push(glob);
    }
    globs.sort();
    globs.dedup();
    Ok(globs)
}

fn branch_exists(repo: &Path, name: &str) -> bool {
    emi_git::run(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok()
}

/// The remote's default branch, else `main`, else `master`.
fn default_branch(repo: &Path) -> Result<String, Error> {
    let remote = emi_git::run(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok();
    if let Some(out) = remote {
        let text = String::from_utf8_lossy(&out).trim().to_owned();
        if let Some(name) = text.strip_prefix("origin/").filter(|n| !n.is_empty()) {
            return Ok(name.to_owned());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|b| branch_exists(repo, b))
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::Config(
                "cannot tell the default branch (no origin/HEAD, no main or master); \
                 pass --default-branch"
                    .to_owned(),
            )
        })
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The python the tests should run with: a virtualenv's when there is one.
fn python(repo: &Path) -> Result<String, Error> {
    if let Some(venv) = VENVS
        .iter()
        .find(|v| repo.join(v).join("bin/python").exists())
    {
        return Ok(format!("{venv}/bin/python"));
    }
    ["python", "python3"]
        .into_iter()
        .find(|n| on_path(n).is_some())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::Config("no python found: no .venv or venv, and no python on PATH".to_owned())
        })
}

/// Whether `python` can import pytest, run from `repo` the way the check
/// would run it.
fn has_pytest(repo: &Path, python: &str) -> bool {
    let mut spec = emi_exec::Spec::new(&[python, "-c", "import pytest"], repo);
    spec.timeout = Duration::from_secs(60);
    let outcome = emi_exec::run(&spec);
    outcome.status == emi_exec::Status::Exited && outcome.exit_code == Some(0)
}

/// Propose a configuration for the repository at `repo`.
pub fn detect(repo: &Path, default_branch_override: Option<&str>) -> Result<Detected, Error> {
    let files = files(repo)?;
    language(&files)?;
    let python = python(repo)?;
    let source = source(&files);
    let globs = test_globs(&files)?;
    let branch = match default_branch_override {
        Some(b) => b.to_owned(),
        None => default_branch(repo)?,
    };
    let link: Vec<String> = VENVS
        .iter()
        .filter(|v| repo.join(v).is_dir())
        .map(|v| (*v).to_owned())
        .collect();
    let command: Vec<String> = [python.as_str(), "-m", "pytest", "-q"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut warnings = Vec::new();
    if !has_pytest(repo, &python) {
        warnings.push(format!(
            "{python} cannot import pytest; install it there before running `emi done`"
        ));
    }
    let notes = vec![
        "language: python".to_owned(),
        format!("source directory: {source}"),
        format!("default branch: {branch}"),
        format!("test command: {}", command.join(" ")),
        format!("test files: {}", globs.join(", ")),
        format!(
            "linked dependency directories: {}",
            if link.is_empty() {
                "none".to_owned()
            } else {
                link.join(", ")
            }
        ),
    ];
    let config = Config {
        language: "python".to_owned(),
        source,
        default_branch: branch,
        tests: Tests { command, globs },
        link,
        repair_rounds: DEFAULT_REPAIR_ROUNDS,
        on_unproven: OnUnproven::Report,
    };
    Ok(Detected {
        config,
        notes,
        warnings,
    })
}

/// Write the configuration into the repository. An existing file is
/// replaced only with `force`.
pub fn write(repo: &Path, config: &Config, force: bool) -> Result<PathBuf, Error> {
    let path = repo.join(config::PATH);
    if path.exists() && !force {
        return Err(Error::Config(format!(
            "{} already exists; pass --force to replace it",
            path.display()
        )));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Usage(format!("{}: {e}", dir.display())))?;
    }
    std::fs::write(&path, config.to_text())
        .map_err(|e| Error::Usage(format!("{}: {e}", path.display())))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{commit_file, git, repo};

    fn fixture() -> tempfile::TempDir {
        let dir = repo();
        commit_file(dir.path(), "src/app/__init__.py", "", "pkg");
        commit_file(
            dir.path(),
            "src/app/core.py",
            "def f():\n    return 1\n",
            "core",
        );
        commit_file(
            dir.path(),
            "tests/test_core.py",
            "def test_f():\n    pass\n",
            "test",
        );
        dir
    }

    #[test]
    fn a_conventional_python_repository_is_read() {
        let dir = fixture();
        std::fs::create_dir_all(dir.path().join(".venv/bin")).unwrap_or_default();
        std::fs::write(dir.path().join(".venv/bin/python"), "").unwrap_or_default();
        let d = detect(dir.path(), None).unwrap_or_else(|e| panic!("{e}"));
        let c = &d.config;
        assert_eq!(c.source, "src");
        assert_eq!(c.default_branch, "main");
        assert_eq!(c.tests.command, [".venv/bin/python", "-m", "pytest", "-q"]);
        assert_eq!(c.tests.globs, ["tests/*"]);
        assert_eq!(c.link, [".venv"]);
        assert_eq!(
            Config::from_value(&c.to_value()).ok().as_ref(),
            Some(c),
            "what it proposes is valid"
        );
        assert!(d.notes.iter().any(|n| n.contains("default branch: main")));
    }

    #[test]
    fn other_languages_and_missing_tests_are_explicit_errors() {
        let js = repo();
        commit_file(js.path(), "index.js", "1\n", "js");
        let err = detect(js.path(), None).err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("JavaScript is unsupported for now")));
        let none = repo();
        commit_file(none.path(), "app.py", "x = 1\n", "py");
        let err = detect(none.path(), None).err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("no pytest tests")));
        let empty = repo();
        let err = detect(empty.path(), None).err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("no Python files")));
    }

    #[test]
    fn a_test_helper_inside_a_package_does_not_make_the_package_tests() {
        let files: Vec<String> = [
            "sphinx/testing/fixtures.py",
            "sphinx/app.py",
            "tests/test_a.py",
        ]
        .map(str::to_owned)
        .to_vec();
        let globs = test_globs(&files).unwrap_or_default();
        assert!(!globs.iter().any(|g| g == "sphinx/*"), "{globs:?}");
        assert!(globs.contains(&"tests/*".to_owned()), "{globs:?}");
    }

    #[test]
    fn test_globs_follow_where_the_tests_are() {
        let files: Vec<String> = [
            "tests/a/test_x.py",
            "src/pkg/tests/test_y.py",
            "test_top.py",
            "src/test_deep.py",
            "src/latest_news.py",
            "src/thing_test.py",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            test_globs(&files).unwrap_or_default(),
            [
                "*/test_*.py",
                "*/tests/*",
                "*_test.py",
                "test_*.py",
                "tests/*"
            ]
        );
        let glob_hits = |path: &str| {
            test_globs(&files)
                .unwrap_or_default()
                .iter()
                .any(|g| emi_scan::walk::fnmatch(path, g))
        };
        assert!(
            !glob_hits("src/latest_news.py"),
            "a name that merely contains test_"
        );
    }

    #[test]
    fn the_default_branch_is_found_or_asked_for() {
        let dir = fixture();
        git(dir.path(), &["branch", "-m", "trunk"]);
        let err = detect(dir.path(), None).err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("--default-branch")));
        let d = detect(dir.path(), Some("trunk")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(d.config.default_branch, "trunk");
    }

    #[test]
    fn an_existing_config_is_not_overwritten_without_force() {
        let dir = fixture();
        let d = detect(dir.path(), None).unwrap_or_else(|e| panic!("{e}"));
        write(dir.path(), &d.config, false).unwrap_or_else(|e| panic!("{e}"));
        assert!(write(dir.path(), &d.config, false).is_err());
        assert!(write(dir.path(), &d.config, true).is_ok());
        let text = std::fs::read_to_string(dir.path().join(config::PATH)).unwrap_or_default();
        assert!(Config::parse(&text).is_ok());
    }
}
