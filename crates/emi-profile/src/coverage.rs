//! Line coverage reports read into one shape: for each file, the lines the
//! tool considers executable and the lines the tests executed. Supported:
//! lcov (`SF:`/`DA:` records), Cobertura XML, coverage.py JSON
//! (`coverage json`) and Go cover profiles (`go test -coverprofile`).
//!
//! Report paths are the tool's, often absolute or relative to another
//! checkout; [`resolve`] maps them onto the repository's files and reports
//! the entries it could not place instead of dropping them silently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

pub use crate::Result;

/// One file's lines as the report states them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines {
    pub executable: BTreeSet<u32>,
    pub executed: BTreeSet<u32>,
}

impl Lines {
    fn merge(&mut self, other: Lines) {
        self.executable.extend(other.executable);
        self.executed.extend(other.executed);
    }

    fn line(&mut self, line: u32, hits: bool) {
        self.executable.insert(line);
        if hits {
            self.executed.insert(line);
        }
    }
}

/// A parsed report: files under the tool's own paths.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub format: &'static str,
    /// Cobertura `<source>` roots the class file names are relative to.
    pub sources: Vec<String>,
    pub files: BTreeMap<String, Lines>,
}

impl Report {
    fn add(&mut self, path: &str, lines: Lines) {
        self.files.entry(path.to_owned()).or_default().merge(lines);
    }
}

/// Parse a report, recognising its format from the content.
pub fn parse(text: &str) -> Result<Report> {
    let head = text.trim_start();
    let report = if head.starts_with('{') {
        coverage_py(head)?
    } else if head.starts_with('<') {
        cobertura(text)?
    } else if head.starts_with("mode:") {
        go_profile(head)?
    } else if text.lines().any(|l| l.starts_with("SF:")) {
        lcov(text)?
    } else {
        return Err(
            "unrecognised coverage report: expected lcov, Cobertura XML, coverage.py JSON or a Go cover profile"
                .into(),
        );
    };
    if report.files.is_empty() {
        return Err(format!("the {} report lists no files", report.format));
    }
    Ok(report)
}

fn number(field: &str, what: &str) -> Result<u32> {
    field
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("{what}: not a line number: {field:?}"))
}

fn lcov(text: &str) -> Result<Report> {
    let mut report = Report {
        format: "lcov",
        ..Report::default()
    };
    let mut current: Option<(String, Lines)> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if let Some(path) = line.strip_prefix("SF:") {
            current = Some((path.to_owned(), Lines::default()));
        } else if let Some(data) = line.strip_prefix("DA:") {
            let Some((_, lines)) = current.as_mut() else {
                return Err(format!("lcov line {}: DA before SF", n + 1));
            };
            let mut parts = data.split(',');
            let at = number(parts.next().unwrap_or(""), "lcov DA")?;
            let hits = parts.next().unwrap_or("").trim();
            let hits: u64 = hits
                .parse()
                .map_err(|_| format!("lcov line {}: bad hit count {hits:?}", n + 1))?;
            lines.line(at, hits > 0);
        } else if line == "end_of_record"
            && let Some((path, lines)) = current.take()
        {
            report.add(&path, lines);
        }
    }
    if let Some((path, lines)) = current {
        report.add(&path, lines);
    }
    Ok(report)
}

fn cobertura(text: &str) -> Result<Report> {
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("Cobertura XML: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "coverage" {
        return Err("XML report without a <coverage> root is not Cobertura".into());
    }
    let mut report = Report {
        format: "cobertura",
        ..Report::default()
    };
    report.sources = root
        .descendants()
        .filter(|n| n.has_tag_name("source"))
        .filter_map(|n| n.text().map(|t| t.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .collect();
    for class in root.descendants().filter(|n| n.has_tag_name("class")) {
        let path = class
            .attribute("filename")
            .ok_or("Cobertura <class> without a filename")?;
        let mut lines = Lines::default();
        // the class's own <lines>; <methods> repeat some of them
        let direct = class.children().filter(|c| c.has_tag_name("lines"));
        for line in direct.flat_map(|l| l.children().filter(|c| c.has_tag_name("line"))) {
            let at = number(line.attribute("number").unwrap_or(""), "Cobertura line")?;
            let hits = line.attribute("hits").unwrap_or("");
            let hits: u64 = hits
                .parse()
                .map_err(|_| format!("Cobertura {path}:{at}: bad hits {hits:?}"))?;
            lines.line(at, hits > 0);
        }
        report.add(path, lines);
    }
    Ok(report)
}

fn line_list(v: &Value, key: &str, path: &str) -> Result<Vec<u32>> {
    let Some(items) = v.get(key).and_then(Value::as_array) else {
        return Err(format!("coverage.py JSON: {path} has no {key} list"));
    };
    items
        .iter()
        .map(|x| {
            x.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| format!("coverage.py JSON: {path}: bad line {x}"))
        })
        .collect()
}

fn coverage_py(text: &str) -> Result<Report> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("coverage.py JSON: {e}"))?;
    let files = v
        .get("files")
        .and_then(Value::as_object)
        .ok_or("coverage.py JSON without a files object")?;
    let mut report = Report {
        format: "coverage.py",
        ..Report::default()
    };
    for (path, f) in files {
        let mut lines = Lines::default();
        for at in line_list(f, "executed_lines", path)? {
            lines.line(at, true);
        }
        for at in line_list(f, "missing_lines", path)? {
            lines.line(at, false);
        }
        report.add(path, lines);
    }
    Ok(report)
}

/// `file:startLine.col,endLine.col statements count`
fn go_block(line: &str) -> Result<(String, u32, u32, bool)> {
    let bad = || format!("Go cover profile: cannot read {line:?}");
    let (path, rest) = line.rsplit_once(':').ok_or_else(bad)?;
    let mut fields = rest.split_whitespace();
    let span = fields.next().ok_or_else(bad)?;
    let count: u64 = fields.nth(1).and_then(|c| c.parse().ok()).ok_or_else(bad)?;
    let (start, end) = span.split_once(',').ok_or_else(bad)?;
    let first = |pos: &str| pos.split('.').next().unwrap_or("").to_owned();
    let (start, end) = (number(&first(start), "Go")?, number(&first(end), "Go")?);
    Ok((path.to_owned(), start, end, count > 0))
}

fn go_profile(text: &str) -> Result<Report> {
    let mut report = Report {
        format: "go",
        ..Report::default()
    };
    for line in text.lines().skip(1).filter(|l| !l.trim().is_empty()) {
        let (path, start, end, hit) = go_block(line)?;
        let mut lines = Lines::default();
        for at in start..=end.max(start) {
            lines.line(at, hit);
        }
        report.add(&path, lines);
    }
    Ok(report)
}

/// A report placed onto the repository.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub format: &'static str,
    /// Repository-relative path to its lines.
    pub files: BTreeMap<String, Lines>,
    /// Report entries that match no repository file (outside the scope, or
    /// from another tree).
    pub unmatched: Vec<String>,
    /// Report entries whose path could name more than one repository file.
    pub ambiguous: Vec<String>,
}

fn normalize(p: &str) -> String {
    let p = p.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_owned()
}

/// Where one report path lands among `known` repository-relative files.
enum Placement {
    File(String),
    Ambiguous,
    Nowhere,
}

fn place(candidate: &str, root: &str, known: &BTreeSet<String>) -> Placement {
    let rel = candidate
        .strip_prefix(root)
        .map_or(candidate, |r| r.trim_start_matches('/'));
    if known.contains(rel) {
        return Placement::File(rel.to_owned());
    }
    // a relative path from a subdirectory, or an absolute path from another
    // checkout: the repository file it ends with, if exactly one
    let hits: Vec<&String> = known
        .iter()
        .filter(|k| rel.ends_with(&format!("/{k}")) || k.ends_with(&format!("/{rel}")))
        .collect();
    match hits.as_slice() {
        [one] => Placement::File((*one).clone()),
        [] => Placement::Nowhere,
        _ => Placement::Ambiguous,
    }
}

/// Map `report` onto `known` (repository-relative files at the profiled
/// revision); `repo_root` is where absolute paths may point.
pub fn resolve(report: Report, repo_root: &Path, known: &BTreeSet<String>) -> Resolved {
    let root = normalize(&repo_root.to_string_lossy());
    let mut out = Resolved {
        format: report.format,
        ..Resolved::default()
    };
    for (raw, lines) in report.files {
        let path = normalize(&raw);
        let mut candidates: Vec<String> = report
            .sources
            .iter()
            .map(|s| format!("{}/{path}", normalize(s).trim_end_matches('/')))
            .collect();
        candidates.push(path.clone());
        let mut ambiguous = false;
        let placed = candidates
            .iter()
            .find_map(|c| match place(c, &root, known) {
                Placement::File(f) => Some(f),
                Placement::Ambiguous => {
                    ambiguous = true;
                    None
                }
                Placement::Nowhere => None,
            });
        match placed {
            Some(file) => out.files.entry(file).or_default().merge(lines),
            None if ambiguous => out.ambiguous.push(raw),
            None => out.unmatched.push(raw),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(lines: &[u32]) -> BTreeSet<u32> {
        lines.iter().copied().collect()
    }

    #[test]
    fn lcov_records_merge_per_file() {
        let r = parse(
            "TN:\nSF:src/a.py\nDA:1,1\nDA:2,0\nend_of_record\nSF:src/a.py\nDA:3,4\nend_of_record\n",
        )
        .unwrap_or_default();
        let a = &r.files["src/a.py"];
        assert_eq!(a.executable, set(&[1, 2, 3]));
        assert_eq!(a.executed, set(&[1, 3]));
    }

    #[test]
    fn cobertura_joins_sources_and_ignores_method_repeats() {
        let xml = r#"<?xml version="1.0"?><coverage><sources><source>/ci/checkout/src</source></sources>
          <packages><package><classes><class filename="pkg/a.py">
            <methods><method><lines><line number="9" hits="0"/></lines></method></methods>
            <lines><line number="1" hits="2"/><line number="2" hits="0"/></lines>
          </class></classes></package></packages></coverage>"#;
        let r = parse(xml).unwrap_or_default();
        assert_eq!(r.files["pkg/a.py"].executable, set(&[1, 2]));
        let known: BTreeSet<String> = ["src/pkg/a.py".to_owned()].into();
        let placed = resolve(r, Path::new("/home/me/repo"), &known);
        assert_eq!(placed.files["src/pkg/a.py"].executed, set(&[1]));
        assert!(placed.unmatched.is_empty());
    }

    #[test]
    fn coverage_py_json_counts_executed_and_missing() {
        let j = r#"{"meta": {}, "files": {"/home/me/repo/src/a.py": {"executed_lines": [1, 2], "missing_lines": [5], "excluded_lines": [7]}}}"#;
        let r = parse(j).unwrap_or_default();
        let known: BTreeSet<String> = ["src/a.py".to_owned(), "src/b.py".to_owned()].into();
        let placed = resolve(r, Path::new("/home/me/repo"), &known);
        let a = &placed.files["src/a.py"];
        assert_eq!(a.executable, set(&[1, 2, 5]));
        assert_eq!(a.executed, set(&[1, 2]));
    }

    #[test]
    fn go_blocks_cover_their_line_span() {
        let r = parse(
            "mode: set\nexample.com/m/pkg/a.go:3.10,5.2 2 1\nexample.com/m/pkg/a.go:7.2,7.20 1 0\n",
        )
        .unwrap_or_default();
        let known: BTreeSet<String> = ["pkg/a.go".to_owned()].into();
        let placed = resolve(r, Path::new("/x"), &known);
        let a = &placed.files["pkg/a.go"];
        assert_eq!(a.executable, set(&[3, 4, 5, 7]));
        assert_eq!(a.executed, set(&[3, 4, 5]));
    }

    #[test]
    fn unplaceable_entries_are_reported_not_dropped() {
        let r = parse(
            "SF:/elsewhere/vendor/x.py\nDA:1,1\nend_of_record\nSF:util.py\nDA:1,1\nend_of_record\n",
        )
        .unwrap_or_default();
        let known: BTreeSet<String> = ["a/util.py".to_owned(), "b/util.py".to_owned()].into();
        let placed = resolve(r, Path::new("/repo"), &known);
        assert!(placed.files.is_empty());
        assert_eq!(placed.unmatched, vec!["/elsewhere/vendor/x.py"]);
        assert_eq!(placed.ambiguous, vec!["util.py"]);
    }

    #[test]
    fn unreadable_reports_are_errors() {
        for bad in [
            "",
            "hello",
            "SF:a.py\nDA:x,1\n",
            "<coverage/>",
            "{\"files\": {}}",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
