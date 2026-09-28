//! The guardrail checks. Each takes the change and returns raw findings
//! `(id, path, line, evidence)`; the engine assigns tiers from the policy.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use emi_diff::hunks::{FileChange, Status, text_at};
use emi_scan::classify::{PathClass, classify, is_test};
use emi_scan::walk::fnmatch;

use crate::patterns::{
    LineKind, SKIP, SUPPRESS, TRIVIAL, count_assertions, count_tests, first_match, lang_of,
    line_kinds,
};
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
            if class == PathClass::SecretLike && public_certificates(fc) {
                continue;
            }
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

/// Markers on removed lines across the change, by kind, so a marker that
/// is only moved, or kept on an edited line, is not reported as new: each
/// removed marker pairs with one added marker of the same kind.
struct Moved(HashMap<&'static str, usize>);

impl Moved {
    fn of(files: &[FileChange], set: &[(&'static str, regex::Regex)]) -> Self {
        let mut m = HashMap::new();
        for (_, text) in files.iter().flat_map(|f| &f.removed) {
            if let Some(kind) = first_match(set, text) {
                *m.entry(kind).or_insert(0) += 1;
            }
        }
        Moved(m)
    }

    /// Whether an added marker of `kind` pairs with a removed one.
    fn take(&mut self, kind: &'static str) -> bool {
        match self.0.get_mut(kind) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }
}

/// Added lines of `fc` matching `set` as `(line, pattern, text)`, leaving
/// out markers paired with a removed one (moved, or kept on an edited
/// line), lines inside multi-line strings (test fixtures, templates) and,
/// when `code_only`, comment lines.
fn added_markers<'a>(
    repo: &Path,
    head: Option<&str>,
    fc: &'a FileChange,
    set: &[(&'static str, regex::Regex)],
    moved: &mut Moved,
    code_only: bool,
) -> Vec<(u32, &'static str, &'a str)> {
    let mut kinds: Option<Vec<LineKind>> = None;
    let mut out = Vec::new();
    for (line, text) in &fc.added {
        let Some(kind) = first_match(set, text) else {
            continue;
        };
        if moved.take(kind) {
            continue;
        }
        let kinds = kinds.get_or_insert_with(|| match lang_of(&fc.path) {
            Some(lang) => text_at(repo, head, &fc.path)
                .map(|t| line_kinds(lang, &t))
                .unwrap_or_default(),
            None => Vec::new(),
        });
        let at = usize::try_from(*line).ok().and_then(|l| l.checked_sub(1));
        match at.and_then(|i| kinds.get(i)) {
            Some(LineKind::InString) => continue,
            Some(LineKind::Comment) if code_only => continue,
            _ => out.push((*line, kind, text.as_str())),
        }
    }
    out
}

/// A `.pem` change that adds certificates and no private key (a CA
/// bundle such as `cacert.pem`): public data, not a secret.
fn public_certificates(fc: &FileChange) -> bool {
    fc.path.ends_with(".pem")
        && fc.status != Status::Deleted
        && fc
            .added
            .iter()
            .any(|(_, t)| t.contains("BEGIN CERTIFICATE"))
        && !fc.added.iter().any(|(_, t)| t.contains("PRIVATE KEY"))
}

/// `suppress.added`: a suppression comment or attribute on an added line.
pub fn suppress(repo: &Path, head: Option<&str>, files: &[FileChange]) -> Vec<Finding> {
    let mut moved = Moved::of(files, &SUPPRESS);
    let mut out = Vec::new();
    // a snapshot records output (a formatter's test data), not code; vendored
    // and generated files are not the project's code
    let skip = |p: &str| {
        p.ends_with(".snap")
            || p.contains("__snapshots__/")
            || matches!(classify(p), PathClass::Vendored | PathClass::Generated)
    };
    for fc in files.iter().filter(|f| !f.binary && !skip(&f.path)) {
        for (line, kind, text) in added_markers(repo, head, fc, &SUPPRESS, &mut moved, false) {
            out.push(finding(
                "suppress.added",
                &fc.path,
                Some(line),
                format!("{kind}: {}", text.trim()),
            ));
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
    match enclosing(text, line, |l| l.trim_start().starts_with("def ")) {
        Some(d) => !non_test_name(d),
        // module level: an import configures nothing by itself
        None => !(current.starts_with("import ") || current.starts_with("from ")),
    }
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
    // a new manifest (a new package) configures only the tests it brings
    if fc.status == Status::Added
        || !matches!(
            name,
            "pyproject.toml" | "setup.cfg" | "tox.ini" | "Cargo.toml" | "noxfile.py"
        )
    {
        return None;
    }
    let new = text_at(repo, head, &fc.path).unwrap_or_default();
    let old =
        text_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path)).unwrap_or_default();
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

/// Whether `path` at `rev` defines test cases: fixtures, helpers and
/// snapshots under a test directory are not tests.
fn defines_tests(repo: &Path, rev: &str, path: &str) -> bool {
    lang_of(path).is_some_and(|lang| {
        text_at(repo, Some(rev), path).is_some_and(|t| count_tests(lang, &t) > 0)
    })
}

/// `tests.deleted` for one file change, if it deletes a file defining
/// tests or moves one out of the test layout.
fn deleted(repo: &Path, base: &str, fc: &FileChange) -> Option<Finding> {
    let old_path = fc.old_path.as_deref().unwrap_or(&fc.path);
    let gone = match fc.status {
        Status::Deleted => is_test(&fc.path),
        Status::Renamed => is_test(old_path) && !is_test(&fc.path),
        _ => false,
    };
    if !gone || !defines_tests(repo, base, old_path) {
        return None;
    }
    match fc.status {
        Status::Deleted => Some(finding(
            "tests.deleted",
            &fc.path,
            None,
            "test file deleted",
        )),
        _ => Some(finding(
            "tests.deleted",
            old_path,
            None,
            format!("test file moved out of the test layout to {}", fc.path),
        )),
    }
}

/// `text` without whitespace, commas and semicolons, with one quote
/// style: equal for two versions that differ only in formatting.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ',' | ';'))
        .map(|c| if c == '\'' { '"' } else { c })
        .collect()
}

/// Lines of a `conftest.py` that configure the test run, as opposed to
/// fixtures and helpers.
static CONFTEST_CONFIG: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"\bpytest_\w+|collect_ignore|pytest_plugins|register_assert_rewrite|autouse|addopts|\bsys\.path\b|\bos\.environ\b",
    )
    .unwrap_or_else(|e| panic!("bad built-in pattern: {e}"))
});

/// Whether a whole-file test configuration change configures anything:
/// not formatting only, and for `conftest.py` not only fixtures/helpers.
fn configures(repo: &Path, base: &str, head: Option<&str>, fc: &FileChange) -> bool {
    if fc.status == Status::Deleted {
        return true;
    }
    if fc.path.rsplit('/').next() == Some("conftest.py") {
        return fc
            .added
            .iter()
            .chain(&fc.removed)
            .any(|(_, t)| CONFTEST_CONFIG.is_match(t));
    }
    let old = text_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path));
    match (old, text_at(repo, head, &fc.path)) {
        (Some(old), Some(new)) => normalized(&old) != normalized(&new),
        _ => true,
    }
}

/// `tests.config_changed` for one file change.
fn config_changed(repo: &Path, base: &str, head: Option<&str>, fc: &FileChange) -> Option<Finding> {
    if classify(&fc.path) == PathClass::Vendored {
        return None; // a dependency's own test setup
    }
    if classify(&fc.path) == PathClass::TestConfig
        && fc.status != Status::Added
        && !multipurpose(&fc.path)
    {
        if !configures(repo, base, head, fc) {
            return None;
        }
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
    let mut moved = Moved::of(files, &SKIP);
    let mut moved_trivial = Moved::of(files, &TRIVIAL);
    let mut out = Vec::new();
    for fc in files.iter().filter(|f| !f.binary) {
        if let Some(f) = deleted(repo, base, fc) {
            out.push(f);
            continue;
        }
        out.extend(config_changed(repo, base, head, fc));
        let skips = added_markers(repo, head, fc, &SKIP, &mut moved, true);
        let (new, old) = if skips.is_empty() {
            (None, None)
        } else {
            let old = (fc.status != Status::Added)
                .then(|| text_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path)))
                .flatten();
            (text_at(repo, head, &fc.path), old)
        };
        for (line, kind, text) in skips {
            // a skip on a test the baseline already had weakens it; one on a
            // new test (a platform-only case, say) is a judgment call
            let existing = new
                .as_deref()
                .is_none_or(|n| skips_existing_test(n, old.as_deref(), line, kind));
            out.push(finding(
                if existing {
                    "tests.skip_added"
                } else {
                    "tests.skip_new_test"
                },
                &fc.path,
                Some(line),
                format!("{kind}: {}", text.trim()),
            ));
        }
        if is_test(&fc.path) {
            for (line, kind, text) in
                added_markers(repo, head, fc, &TRIVIAL, &mut moved_trivial, true)
            {
                out.push(finding(
                    "tests.assertion_trivialized",
                    &fc.path,
                    Some(line),
                    format!("{kind}: {}", text.trim()),
                ));
            }
        }
    }
    out.extend(counts(repo, base, head, files));
    out
}

/// A test (or test class) header and its name: Python `def`/`class`,
/// JS/TS `it`/`test`/`describe` titles, Go `func`, Rust `fn`.
static TEST_HEADER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"^\s*(?:async\s+)?def\s+(\w+)|^\s*class\s+(\w+)|\b(?:it|test|describe)(?:\.\w+)?\s*\(\s*['"`]([^'"`]+)['"`]|^func\s+(\w+)|^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)"#,
    )
    .unwrap_or_else(|e| panic!("bad built-in pattern: {e}"))
});

fn header_name(line: &str) -> Option<String> {
    let c = TEST_HEADER.captures(line)?;
    (1..=5)
        .find_map(|i| c.get(i))
        .map(|m| m.as_str().to_owned())
}

/// Whether the skip at 1-based `line` of `new` applies to a test the old
/// version already had. Markers (decorators, attributes, `it.skip(`) apply
/// to the header at or after them; calls (`pytest.skip()`, `t.Skip()`,
/// `this.skip()`) to the enclosing header. No header found: assume existing.
fn skips_existing_test(new: &str, old: Option<&str>, line: u32, kind: &str) -> bool {
    let Some(old) = old else {
        return false;
    };
    let lines: Vec<&str> = new.lines().collect();
    let at = (line as usize).saturating_sub(1).min(lines.len());
    let call = matches!(
        kind,
        "pytest.skip() call" | "unittest SkipTest" | "mocha this.skip()" | "Go t.Skip"
    );
    let before = || {
        lines[..at]
            .iter()
            .rev()
            .take(200)
            .find_map(|l| header_name(l))
    };
    let name = if call {
        before()
    } else {
        lines.iter().skip(at).take(8).find_map(|l| header_name(l))
    };
    match name {
        Some(n) => old
            .lines()
            .any(|l| header_name(l).as_deref() == Some(n.as_str())),
        None => true,
    }
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
        // a side that cannot be read is unknown, not empty
        let Some(new) = text_at(repo, head, &fc.path) else {
            continue;
        };
        let old = if fc.status == Status::Added {
            String::new()
        } else {
            let Some(old) = text_at(repo, Some(base), fc.old_path.as_deref().unwrap_or(&fc.path))
            else {
                continue;
            };
            old
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
