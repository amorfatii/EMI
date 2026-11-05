//! Per-test outcomes read from pytest's JUnit XML (`--junitxml`).
//!
//! The file is written by a process the candidate controls (its tests and
//! its `conftest.py` run inside pytest), so it is evidence to be checked,
//! not a verdict: the report must agree with itself (the totals it states
//! equal what its test cases show), with the exit code of the run that
//! wrote it, and with the test ids that were asked for. Anything that does
//! not line up is `Observed::Unknown` with the reason, never a guess.

use std::collections::{BTreeMap, BTreeSet};

/// What one run showed about one test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observed {
    Passed,
    Failed,
    /// Setup, teardown or collection raised; the test body did not decide.
    Errored,
    /// Skipped, or an expected failure: neither a pass nor a failure.
    Skipped,
    Unknown(String),
}

/// How the run that wrote a report ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunExit {
    Code(i32),
    /// Timed out, was killed, could not start, or left processes behind.
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Passed,
    Failed,
    Errored,
    Skipped,
}

#[derive(Debug, Default)]
pub struct Report {
    /// (classname, name) as pytest writes them, to every case with that key.
    cases: BTreeMap<(String, String), Vec<Kind>>,
}

/// The `(classname, name)` pytest writes for a node id: the path without
/// `.py` and with dots, then the class names; the last part (with its
/// parameters) is the name.
pub fn address(id: &str) -> (String, String) {
    let (path, bracket, params) = match id.find('[') {
        Some(at) => (&id[..at], "[", &id[at + 1..]),
        None => (id, "", ""),
    };
    let mut parts: Vec<String> = path.split("::").map(str::to_owned).collect();
    if let Some(first) = parts.first_mut() {
        *first = first.replace('/', ".");
        if let Some(stem) = first.strip_suffix(".py") {
            *first = stem.to_owned();
        }
    }
    let name = parts.pop().unwrap_or_default();
    (parts.join("."), format!("{name}{bracket}{params}"))
}

/// The key pytest gives a file that failed to collect: no class, the
/// dotted path as the name.
fn collection_address(id: &str) -> (String, String) {
    let file = id.split("::").next().unwrap_or(id);
    (String::new(), address(file).1)
}

fn count(attr: Option<&str>, what: &str) -> Result<u64, String> {
    attr.and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("the report's {what} total is missing or not a number"))
}

fn kind_of(case: roxmltree::Node) -> Kind {
    let has = |tag: &str| case.children().any(|c| c.has_tag_name(tag));
    if has("failure") {
        Kind::Failed
    } else if has("error") {
        Kind::Errored
    } else if has("skipped") {
        Kind::Skipped
    } else {
        Kind::Passed
    }
}

/// Totals the suites state, in the order tests, failures, errors, skipped.
fn stated(doc: &roxmltree::Document) -> Result<[u64; 4], String> {
    let mut total = [0u64; 4];
    let mut suites = 0;
    for suite in doc.descendants().filter(|n| n.has_tag_name("testsuite")) {
        suites += 1;
        for (slot, name) in total
            .iter_mut()
            .zip(["tests", "failures", "errors", "skipped"])
        {
            *slot += count(suite.attribute(name), name)?;
        }
    }
    if suites == 0 {
        return Err("the report has no <testsuite>".into());
    }
    Ok(total)
}

/// Totals the test cases show.
fn shown(doc: &roxmltree::Document) -> [u64; 4] {
    let tag = |name: &str| {
        doc.descendants()
            .filter(|n| n.has_tag_name("testcase"))
            .flat_map(|c| c.children())
            .filter(|c| c.has_tag_name(name))
            .count() as u64
    };
    let cases = doc
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .count() as u64;
    [cases, tag("failure"), tag("error"), tag("skipped")]
}

/// Read a report. Malformed or truncated XML, a DTD, a missing total, or
/// totals that the cases contradict are errors.
pub fn parse(xml: &str) -> Result<Report, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("the report is not XML: {e}"))?;
    let root = doc.root_element().tag_name().name();
    if root != "testsuites" && root != "testsuite" {
        return Err(format!("the report's root is <{root}>, not a JUnit suite"));
    }
    let (stated, shown) = (stated(&doc)?, shown(&doc));
    if stated != shown {
        return Err(format!(
            "the report contradicts itself: it states {stated:?} (tests, failures, errors, skipped) but its cases show {shown:?}"
        ));
    }
    let mut report = Report::default();
    for case in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        let key = (
            case.attribute("classname").unwrap_or("").to_owned(),
            case.attribute("name")
                .ok_or("a <testcase> in the report has no name")?
                .to_owned(),
        );
        report.cases.entry(key).or_default().push(kind_of(case));
    }
    Ok(report)
}

impl Report {
    fn any_bad(&self) -> bool {
        self.cases
            .values()
            .flatten()
            .any(|k| matches!(k, Kind::Failed | Kind::Errored))
    }

    /// Whether some file is reported as one error under its own name.
    fn has_collection_error(&self) -> bool {
        self.cases
            .iter()
            .any(|((class, _), kinds)| class.is_empty() && kinds.contains(&Kind::Errored))
    }

    fn cases(&self) -> usize {
        self.cases.values().map(Vec::len).sum()
    }

    fn lookup(&self, key: &(String, String)) -> Option<&[Kind]> {
        self.cases.get(key).map(Vec::as_slice)
    }
}

/// Ids whose address another requested id shares: pytest cannot tell them
/// apart in the report.
fn ambiguous(ids: &[String]) -> BTreeSet<&str> {
    let mut by_address: BTreeMap<(String, String), Vec<&str>> = BTreeMap::new();
    for id in ids {
        by_address.entry(address(id)).or_default().push(id);
    }
    by_address
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect()
}

/// Whether the exit code and the report can both be true. Exit 4 is what
/// pytest returns when a test id names a file that failed to import ("found
/// no collectors"): it ran nothing, and the report holds the import error.
fn consistent(exit: i32, report: &Report) -> Result<(), String> {
    match (exit, report.any_bad()) {
        (0, false) | (1, true) => Ok(()),
        (4, true) if report.has_collection_error() => Ok(()),
        (0, true) => Err("pytest exited 0 but the report lists failures or errors".into()),
        (1, false) => Err("pytest exited 1 but the report lists no failure or error".into()),
        (code, _) => Err(format!("pytest exited with code {code}, not 0 or 1")),
    }
}

fn one(kinds: &[Kind]) -> Observed {
    match kinds {
        [Kind::Passed] => Observed::Passed,
        [Kind::Failed] => Observed::Failed,
        [Kind::Errored] => Observed::Errored,
        [Kind::Skipped] => Observed::Skipped,
        _ => Observed::Unknown("the report lists the test more than once".into()),
    }
}

fn observe_one(id: &str, report: &Report) -> Observed {
    if let Some(kinds) = report.lookup(&address(id)) {
        return one(kinds);
    }
    // a file that cannot be imported reports one error under its own name
    // and none of its tests
    match report.lookup(&collection_address(id)) {
        Some([Kind::Errored]) => Observed::Errored,
        _ => Observed::Unknown("the run did not report the test".into()),
    }
}

/// What a run showed about each of `ids`.
pub fn observe(
    exit: &RunExit,
    report: Result<Report, String>,
    ids: &[String],
) -> BTreeMap<String, Observed> {
    let all_unknown = |why: String| {
        ids.iter()
            .map(|id| (id.clone(), Observed::Unknown(why.clone())))
            .collect()
    };
    let code = match exit {
        RunExit::Code(c) => *c,
        RunExit::Failed(why) => return all_unknown(why.clone()),
    };
    let report = match report {
        Ok(r) => r,
        Err(why) => return all_unknown(why),
    };
    if let Err(why) = consistent(code, &report) {
        return all_unknown(why);
    }
    if report.cases() == 0 {
        return all_unknown("the report lists no test".into());
    }
    let shared = ambiguous(ids);
    ids.iter()
        .map(|id| {
            let seen = if shared.contains(id.as_str()) {
                Observed::Unknown("another test has the same name in the report".into())
            } else {
                observe_one(id, &report)
            };
            (id.clone(), seen)
        })
        .collect()
}

#[cfg(test)]
mod tests;
