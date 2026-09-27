//! The guardrail checks. Each takes the change and returns raw findings
//! `(id, path, line, evidence)`; the engine assigns tiers from the policy.

use std::collections::BTreeMap;
use std::path::Path;

use emi_diff::hunks::{FileChange, Status, file_at};
use emi_scan::classify::{PathClass, classify, is_test};
use emi_scan::walk::fnmatch;

use crate::patterns::{SKIP, SUPPRESS, count_assertions, count_tests, first_match, lang_of};
use crate::policy::Policy;

/// A guardrail finding before its tier is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub id: &'static str,
    pub path: String,
    pub line: Option<u32>,
    pub evidence: String,
}

fn finding(
    id: &'static str,
    path: &str,
    line: Option<u32>,
    evidence: impl Into<String>,
) -> Finding {
    Finding {
        id,
        path: path.to_owned(),
        line,
        evidence: evidence.into(),
    }
}

fn any_match(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|g| fnmatch(path, g))
}

/// Paths and change kinds a file change amounts to (a rename is a
/// delete of the old path plus an add of the new one).
fn touches(fc: &FileChange) -> Vec<(&str, &'static str)> {
    match (fc.status, fc.old_path.as_deref()) {
        (Status::Renamed, Some(old)) => vec![(old, "delete"), (fc.path.as_str(), "add")],
        (status, _) => vec![(fc.path.as_str(), status.as_str())],
    }
}

/// `scope.*`: the task envelope (protected, excluded, kind, permitted).
pub fn scope(files: &[FileChange], policy: &Policy) -> Vec<Finding> {
    let mut out = Vec::new();
    for fc in files {
        for (path, kind) in touches(fc) {
            let hit = if any_match(path, &policy.protected) {
                Some((
                    "scope.protected",
                    "protected path; needs a controller revision".to_owned(),
                ))
            } else if any_match(path, &policy.excluded) {
                Some((
                    "scope.excluded",
                    "explicitly excluded from this task".to_owned(),
                ))
            } else if !policy
                .change_kinds
                .iter()
                .any(|k| k == kind || (k == "rename" && fc.status == Status::Renamed))
            {
                Some(("scope.kind", format!("change kind {kind} not permitted")))
            } else if !any_match(path, &policy.permitted) {
                Some((
                    "scope.outside",
                    "outside the permitted task scope".to_owned(),
                ))
            } else {
                None
            };
            if let Some((id, why)) = hit {
                out.push(finding(id, path, None, format!("{kind}: {why}")));
            }
        }
    }
    out
}

/// `prohibited.*`: changes to lockfiles, CI, secret-like or vendored files
/// unless the policy allows the class.
pub fn prohibited(files: &[FileChange], policy: &Policy) -> Vec<Finding> {
    let mut out = Vec::new();
    for fc in files {
        for (path, kind) in touches(fc) {
            let class = classify(path);
            let id = match class {
                PathClass::Lockfile => "prohibited.lockfile",
                PathClass::Ci => "prohibited.ci",
                PathClass::SecretLike => "prohibited.secret",
                PathClass::Vendored => "prohibited.vendored",
                _ => continue,
            };
            if !policy.allow_classes.contains(&class) {
                out.push(finding(
                    id,
                    path,
                    None,
                    format!("{kind} of a {} file", class.as_str()),
                ));
            }
        }
    }
    out
}

/// `suppress.added`: a suppression comment or attribute on an added line.
pub fn suppress(files: &[FileChange]) -> Vec<Finding> {
    let mut out = Vec::new();
    for fc in files.iter().filter(|f| !f.binary) {
        for (line, text) in &fc.added {
            if let Some(kind) = first_match(&SUPPRESS, text) {
                out.push(finding(
                    "suppress.added",
                    &fc.path,
                    Some(*line),
                    format!("{kind}: {}", text.trim()),
                ));
            }
        }
    }
    out
}

/// Sections of shared config files that hold test or coverage settings.
const TEST_SECTIONS: &[&str] = &[
    "[tool.pytest",
    "[tool.coverage",
    "[coverage:",
    "[tool:pytest]",
    "[pytest]",
    "[profile.test]",
];

/// Names of tox environments or nox sessions that are not tests.
const NON_TEST_WORDS: &[&str] = &[
    "lint",
    "format",
    "fmt",
    "pep8",
    "flake8",
    "black",
    "ruff",
    "mypy",
    "typing",
    "pyright",
    "docs",
    "style",
    "zimports",
    "isort",
    "pre-commit",
    "precommit",
];

fn non_test_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    NON_TEST_WORDS.iter().any(|w| lower.contains(w))
}

/// The last line of `text` up to 1-based `line` satisfying `pick`.
fn enclosing(text: &str, line: u32, pick: impl Fn(&str) -> bool) -> Option<&str> {
    text.lines()
        .take(line as usize)
        .filter(|l| pick(l))
        .last()
        .map(str::trim)
}

/// nox: the session a line belongs to decides; a decorator line belongs to
/// the function after it, any other line to the function before it.
fn nox_line_is_test(text: &str, line: u32) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    let at = (line as usize).saturating_sub(1);
    let current = lines.get(at).map_or("", |l| l.trim_start());
    if current.starts_with('@') {
        let next_def = lines
            .iter()
            .skip(at)
            .find(|l| l.trim_start().starts_with("def "));
        return !non_test_name(current) && next_def.is_none_or(|d| !non_test_name(d));
    }
    enclosing(text, line, |l| l.trim_start().starts_with("def ")).is_none_or(|d| !non_test_name(d))
}

/// Does changed `line` of `text` configure tests in file `name`?
fn configures_tests(name: &str, text: &str, line: u32) -> bool {
    match name {
        // tox: every environment runs tests unless it is a lint/docs one
        "tox.ini" => enclosing(text, line, |l| l.trim_start().starts_with('['))
            .is_none_or(|s| !non_test_name(s)),
        "noxfile.py" => nox_line_is_test(text, line),
        _ => enclosing(text, line, |l| l.trim_start().starts_with('['))
            .is_some_and(|s| TEST_SECTIONS.iter().any(|t| s.starts_with(t))),
    }
}

/// A changed line of a shared or multipurpose config file that configures
/// tests (pyproject/setup.cfg/Cargo sections; tox environments and nox
/// sessions other than lint, format or docs ones).
fn config_section_changed(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    fc: &FileChange,
) -> Option<(u32, String)> {
    let name = fc.path.rsplit('/').next().unwrap_or(&fc.path);
    if !matches!(
        name,
        "pyproject.toml" | "setup.cfg" | "tox.ini" | "Cargo.toml" | "noxfile.py"
    ) {
        return None;
    }
    let new = file_at(repo, head, &fc.path).unwrap_or_default();
    let old =
        file_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path)).unwrap_or_default();
    fc.added
        .iter()
        .find(|(l, _)| configures_tests(name, &new, *l))
        .or_else(|| {
            fc.removed
                .iter()
                .find(|(l, _)| configures_tests(name, &old, *l))
        })
        .map(|(l, t)| (*l, t.trim().to_owned()))
}

/// Multipurpose runners judged by content (see `config_section_changed`).
fn multipurpose(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "tox.ini" | "noxfile.py"
    )
}

/// `tests.deleted` for one file change, if it deletes a test file or moves
/// one out of the test layout.
fn deleted(fc: &FileChange) -> Option<Finding> {
    let old_path = fc.old_path.as_deref().unwrap_or(&fc.path);
    match fc.status {
        Status::Deleted if is_test(&fc.path) => Some(finding(
            "tests.deleted",
            &fc.path,
            None,
            "test file deleted",
        )),
        Status::Renamed if is_test(old_path) && !is_test(&fc.path) => Some(finding(
            "tests.deleted",
            old_path,
            None,
            format!("test file moved out of the test layout to {}", fc.path),
        )),
        _ => None,
    }
}

/// `tests.config_changed` for one file change.
fn config_changed(repo: &Path, base: &str, head: Option<&str>, fc: &FileChange) -> Option<Finding> {
    if classify(&fc.path) == PathClass::TestConfig
        && fc.status != Status::Added
        && !multipurpose(&fc.path)
    {
        return Some(finding(
            "tests.config_changed",
            &fc.path,
            None,
            "test or coverage configuration changed",
        ));
    }
    config_section_changed(repo, base, head, fc).map(|(line, text)| {
        finding(
            "tests.config_changed",
            &fc.path,
            Some(line),
            format!("test configuration: {text}"),
        )
    })
}

/// `tests.*`: deleted test files, skip markers added, changed test
/// configuration, and fewer tests or assertions across the change.
pub fn tests(repo: &Path, base: &str, head: Option<&str>, files: &[FileChange]) -> Vec<Finding> {
    let mut out = Vec::new();
    for fc in files.iter().filter(|f| !f.binary) {
        if let Some(f) = deleted(fc) {
            out.push(f);
            continue;
        }
        out.extend(config_changed(repo, base, head, fc));
        for (line, text) in &fc.added {
            if let Some(kind) = first_match(&SKIP, text) {
                out.push(finding(
                    "tests.skip_added",
                    &fc.path,
                    Some(*line),
                    format!("{kind}: {}", text.trim()),
                ));
            }
        }
    }
    out.extend(counts(repo, base, head, files));
    out
}

/// (test cases, assertions) on one side of the change.
type Side = (usize, usize);
/// Per language: old side, new side, and each file's net change.
type Totals = std::collections::BTreeMap<String, (Side, Side, Vec<(String, i64)>)>;
/// Test cases and assertions on each side, per language, summed over the
/// change's test files that are not deleted (deletions are `tests.deleted`).
/// Summing across files keeps a split or move from reading as removal.
fn counts(repo: &Path, base: &str, head: Option<&str>, files: &[FileChange]) -> Vec<Finding> {
    let mut totals: Totals = BTreeMap::new();
    for fc in files
        .iter()
        .filter(|f| !f.binary && f.status != Status::Deleted && is_test(&f.path))
    {
        let Some(lang) = lang_of(&fc.path) else {
            continue;
        };
        let new = file_at(repo, head, &fc.path).unwrap_or_default();
        let old = if fc.status == Status::Added {
            String::new()
        } else {
            file_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path))
                .unwrap_or_default()
        };
        let entry = totals.entry(format!("{lang:?}")).or_default();
        let (t0, a0) = (count_tests(lang, &old), count_assertions(lang, &old));
        let (t1, a1) = (count_tests(lang, &new), count_assertions(lang, &new));
        entry.0 = (entry.0.0 + t0, entry.0.1 + a0);
        entry.1 = (entry.1.0 + t1, entry.1.1 + a1);
        entry
            .2
            .push((fc.path.clone(), (t1 + a1) as i64 - (t0 + a0) as i64));
    }
    let mut out = Vec::new();
    for ((t0, a0), (t1, a1), per_file) in totals.into_values() {
        let files = per_file.len();
        // report at the file that lost the most
        let at = per_file
            .iter()
            .min_by_key(|(_, d)| *d)
            .map_or(".", |(p, _)| p.as_str());
        if t1 < t0 {
            out.push(finding(
                "tests.fewer",
                at,
                None,
                format!("test cases {t0} -> {t1} across {files} changed test file(s)"),
            ));
        }
        if a1 < a0 {
            out.push(finding(
                "tests.fewer_assertions",
                at,
                None,
                format!("assertions {a0} -> {a1} across {files} changed test file(s)"),
            ));
        }
    }
    out
}

/// `growth.*`: net added lines or touched files beyond the policy budget.
pub fn growth(files: &[FileChange], policy: &Policy) -> Vec<Finding> {
    let mut out = Vec::new();
    let net: i64 = files
        .iter()
        .filter(|f| !f.binary)
        .map(FileChange::net)
        .sum();
    if let Some(max) = policy.max_net_sloc
        && net > max
    {
        out.push(finding(
            "growth.net_sloc",
            ".",
            None,
            format!("net lines {net} > budget {max}"),
        ));
    }
    if let Some(max) = policy.max_files
        && files.len() > max
    {
        out.push(finding(
            "growth.files",
            ".",
            None,
            format!("files touched {} > budget {max}", files.len()),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOX: &str = "import nox\n\n@nox.session(python=['3.12'])\ndef tests(session):\n    session.run('pytest')\n\n@nox.session(name=\"mypy\")\ndef typing(session):\n    session.run('mypy')\n";

    #[test]
    fn nox_lines_belong_to_their_session() {
        assert!(nox_line_is_test(NOX, 3), "decorator of the tests session");
        assert!(nox_line_is_test(NOX, 5), "body of the tests session");
        assert!(!nox_line_is_test(NOX, 7), "decorator of the mypy session");
        assert!(!nox_line_is_test(NOX, 9), "body of the typing session");
    }

    #[test]
    fn tox_environments() {
        let tox = "[testenv]\ncommands = pytest\n\n[testenv:lint]\ndeps = flake8\n";
        assert!(configures_tests("tox.ini", tox, 2));
        assert!(!configures_tests("tox.ini", tox, 5));
    }
}
