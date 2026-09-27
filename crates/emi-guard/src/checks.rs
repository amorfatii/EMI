//! The guardrail checks. Each takes the change and returns raw findings
//! `(id, path, line, evidence)`; the engine assigns tiers from the policy.

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

/// Section header (`[...]`) in force at 1-based `line` of `text`.
fn section_at(text: &str, line: u32) -> Option<&str> {
    text.lines()
        .take(line as usize)
        .filter(|l| l.trim_start().starts_with('['))
        .last()
        .map(str::trim)
}

fn config_section_changed(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    fc: &FileChange,
) -> Option<(u32, String)> {
    let name = fc.path.rsplit('/').next().unwrap_or(&fc.path);
    if !matches!(
        name,
        "pyproject.toml" | "setup.cfg" | "tox.ini" | "Cargo.toml"
    ) {
        return None;
    }
    let new = file_at(repo, head, &fc.path).unwrap_or_default();
    let old =
        file_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path)).unwrap_or_default();
    let in_test_section = |text: &str, line: u32| {
        section_at(text, line).is_some_and(|s| TEST_SECTIONS.iter().any(|t| s.starts_with(t)))
    };
    fc.added
        .iter()
        .find(|(l, _)| in_test_section(&new, *l))
        .or_else(|| fc.removed.iter().find(|(l, _)| in_test_section(&old, *l)))
        .map(|(l, t)| (*l, t.trim().to_owned()))
}

/// `tests.*`: deleted test files, fewer tests or assertions in a changed
/// test file, skip markers added, and changed test configuration.
pub fn tests(repo: &Path, base: &str, head: Option<&str>, files: &[FileChange]) -> Vec<Finding> {
    let mut out = Vec::new();
    for fc in files.iter().filter(|f| !f.binary) {
        let old_path = fc.old_path.as_deref().unwrap_or(&fc.path);
        match fc.status {
            Status::Deleted if is_test(&fc.path) => {
                out.push(finding(
                    "tests.deleted",
                    &fc.path,
                    None,
                    "test file deleted",
                ));
                continue;
            }
            Status::Renamed if is_test(old_path) && !is_test(&fc.path) => {
                out.push(finding(
                    "tests.deleted",
                    old_path,
                    None,
                    format!("test file moved out of the test layout to {}", fc.path),
                ));
                continue;
            }
            _ => {}
        }
        if classify(&fc.path) == PathClass::TestConfig && fc.status != Status::Added {
            out.push(finding(
                "tests.config_changed",
                &fc.path,
                None,
                "test or coverage configuration changed",
            ));
        } else if let Some((line, text)) = config_section_changed(repo, base, head, fc) {
            out.push(finding(
                "tests.config_changed",
                &fc.path,
                Some(line),
                format!("test/coverage section: {text}"),
            ));
        }
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
        if fc.status != Status::Added && is_test(&fc.path) {
            out.extend(counts(repo, base, head, fc, old_path));
        }
    }
    out
}

/// Fewer tests or assertions in a test file that exists on both sides.
fn counts(
    repo: &Path,
    base: &str,
    head: Option<&str>,
    fc: &FileChange,
    old_path: &str,
) -> Vec<Finding> {
    let Some(lang) = lang_of(&fc.path) else {
        return Vec::new();
    };
    let (Some(old), Some(new)) = (
        file_at(repo, Some(base), old_path),
        file_at(repo, head, &fc.path),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let (t0, t1) = (count_tests(lang, &old), count_tests(lang, &new));
    if t1 < t0 {
        out.push(finding(
            "tests.fewer",
            &fc.path,
            None,
            format!("test cases {t0} -> {t1}"),
        ));
    }
    let (a0, a1) = (count_assertions(lang, &old), count_assertions(lang, &new));
    if a1 < a0 {
        out.push(finding(
            "tests.fewer_assertions",
            &fc.path,
            None,
            format!("assertions {a0} -> {a1}"),
        ));
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
