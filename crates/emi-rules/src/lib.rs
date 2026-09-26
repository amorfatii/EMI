//! EMI rules (plan Phase 5): declarative tree-sitter query rules for
//! Python and JavaScript/TypeScript, a first-party catalog, inline
//! suppressions with reasons and expiry, baselines, and self-testing rule
//! examples. Rule hits are diagnostics: they never enter burden masses.

pub mod engine;
pub mod model;
pub mod suppress;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

pub use engine::{Grammar, Hit};
pub use model::{Rule, RuleFile, Severity};

use crate::engine::Compiled;
use crate::model::Tests;

/// The built-in catalog, one YAML file per family.
pub const BUILTIN: [(&str, &str); 3] = [
    ("builtin/common.yml", include_str!("builtin/common.yml")),
    ("builtin/js.yml", include_str!("builtin/js.yml")),
    ("builtin/python.yml", include_str!("builtin/python.yml")),
];

/// Parse rule YAML.
pub fn parse_rules(label: &str, text: &str) -> Result<Vec<Rule>, String> {
    let file: RuleFile = yaml_serde::from_str(text).map_err(|e| format!("{label}: {e}"))?;
    Ok(file.rules)
}

/// Load the built-in catalog (unless disabled) plus rule files and
/// directories of `*.yml`/`*.yaml`; duplicate ids are an error.
pub fn load(paths: &[std::path::PathBuf], builtin: bool) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    if builtin {
        for (label, text) in BUILTIN {
            rules.extend(parse_rules(label, text)?);
        }
    }
    for path in paths {
        let files: Vec<std::path::PathBuf> = if path.is_dir() {
            let mut found: Vec<_> = std::fs::read_dir(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
                .collect();
            found.sort();
            found
        } else {
            vec![path.clone()]
        };
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            rules.extend(parse_rules(&f.display().to_string(), &text)?);
        }
    }
    let mut seen = BTreeSet::new();
    for r in &rules {
        if !seen.insert(r.id.clone()) {
            return Err(format!("duplicate rule id {}", r.id));
        }
    }
    Ok(rules)
}

/// Test-infrastructure files (Python and JS conventions).
pub fn is_test_file(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let name = parts.last().copied().unwrap_or("");
    let dirs = &parts[..parts.len().saturating_sub(1)];
    if dirs
        .iter()
        .any(|d| matches!(*d, "test" | "tests" | "__tests__" | "spec" | "specs"))
    {
        return true;
    }
    if name == "conftest.py"
        || (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("_test.py")
    {
        return true;
    }
    let stem_parts: Vec<&str> = name.split('.').collect();
    stem_parts.len() > 2
        && stem_parts[1..stem_parts.len() - 1]
            .iter()
            .any(|p| matches!(*p, "test" | "spec"))
}

/// A finding after suppressions and baseline.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    #[serde(flatten)]
    pub hit: Hit,
    pub fingerprint: String,
    /// `new`, `baseline` (present in the baseline file), or `suppressed`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppression: Option<String>,
}

pub struct Check<'a> {
    pub rules: &'a [Rule],
    /// Rule ids to run (empty: all).
    pub only: &'a [String],
    pub exclude_globs: &'a [String],
    /// `YYYY-MM-DD` for suppression expiry.
    pub today: String,
    pub baseline: Option<&'a BTreeSet<String>>,
}

pub const SUPPRESSION_RULE: &str = "suppression";

fn fingerprint(hit: &Hit, index: usize) -> String {
    emi_ir::pytext::sha256_hex(
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{index}",
            hit.rule, hit.file, hit.text
        )
        .as_bytes(),
    )
}

/// Check one file's source.
pub fn check_source(
    compiled: &[Compiled],
    rel: &str,
    source: &str,
    today: &str,
) -> Vec<(Hit, suppress::Verdict)> {
    let Some(grammar) = Grammar::of(rel) else {
        return Vec::new();
    };
    let test = is_test_file(rel);
    let applicable: Vec<&Compiled> = compiled
        .iter()
        .filter(|c| c.applies_to(grammar))
        .filter(|c| match c.rule.tests {
            Tests::Any => true,
            Tests::Only => test,
            Tests::Exclude => !test,
        })
        .filter(|c| {
            !c.rule
                .exclude
                .iter()
                .any(|g| emi_scan::walk::fnmatch(rel, g))
        })
        .collect();
    let markers = if source.contains("emi-ignore[") {
        suppress::parse(source)
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    if !applicable.is_empty()
        && let Some(tree) = engine::parse(grammar, source)
    {
        for hit in engine::run(&applicable, grammar, rel, source, &tree) {
            let v = suppress::verdict(&markers, &hit.rule, hit.line, today);
            out.push((hit, v));
        }
    }
    for (line, message) in suppress::problems(&markers, today) {
        let hit = Hit {
            rule: SUPPRESSION_RULE.into(),
            severity: "warning",
            file: rel.to_owned(),
            line,
            column: 1,
            end_line: line,
            text: message.clone(),
            message,
        };
        out.push((hit, suppress::Verdict::Active));
    }
    out
}

/// Compile the selected rules.
pub fn compile(check: &Check) -> Result<Vec<Compiled>, String> {
    check
        .rules
        .iter()
        .filter(|r| check.only.is_empty() || check.only.contains(&r.id))
        .map(|r| engine::compile(r.clone()))
        .collect()
}

/// Directories never descended into (both adapters' defaults).
const EXCLUDE_DIRS: [&str; 15] = [
    "__pycache__",
    ".git",
    ".hg",
    ".svn",
    ".tox",
    ".venv",
    "venv",
    "node_modules",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "dist",
    "build",
    ".eggs",
    "coverage",
];

const CODE_SUFFIXES: [&str; 9] = [
    ".py", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts",
];

/// Check every Python/JS/TS file under `root`.
pub fn check_tree(root: &Path, check: &Check) -> Result<Vec<Finding>, String> {
    let compiled = compile(check)?;
    let excludes: Vec<String> = EXCLUDE_DIRS.iter().map(|s| (*s).to_owned()).collect();
    let (files, _, _) = emi_scan::walk::walk(root, &excludes, |n| {
        let lower = n.to_lowercase();
        CODE_SUFFIXES.iter().any(|s| lower.ends_with(s))
    });
    let files: Vec<_> = files
        .into_iter()
        .filter(|f| {
            !check
                .exclude_globs
                .iter()
                .any(|g| emi_scan::walk::fnmatch(&f.rel, g))
        })
        .collect();
    let results: Vec<Option<Vec<_>>> = files
        .par_iter()
        .map(|f| {
            let source = std::fs::read_to_string(&f.path).ok()?;
            Some(check_source(&compiled, &f.rel, &source, &check.today))
        })
        .collect();
    let mut findings = Vec::new();
    for (hit, verdict) in results.into_iter().flatten().flatten() {
        findings.push(Finding {
            hit,
            fingerprint: String::new(),
            state: "new",
            suppression: match verdict {
                suppress::Verdict::Active => None,
                suppress::Verdict::Suppressed(r) => Some(r),
            },
        });
    }
    findings.sort_by(|a, b| {
        (&a.hit.file, a.hit.line, a.hit.column, &a.hit.rule).cmp(&(
            &b.hit.file,
            b.hit.line,
            b.hit.column,
            &b.hit.rule,
        ))
    });
    let mut occurrences: HashMap<(String, String, String), usize> = HashMap::new();
    for f in &mut findings {
        let key = (f.hit.rule.clone(), f.hit.file.clone(), f.hit.text.clone());
        let index = occurrences.entry(key).or_default();
        f.fingerprint = fingerprint(&f.hit, *index);
        *index += 1;
        f.state = if f.suppression.is_some() {
            "suppressed"
        } else if check.baseline.is_some_and(|b| b.contains(&f.fingerprint)) {
            "baseline"
        } else {
            "new"
        };
    }
    Ok(findings)
}

/// Baseline file contents for the current findings (suppressed excluded).
pub fn baseline(findings: &[Finding]) -> serde_json::Value {
    let prints: BTreeSet<&str> = findings
        .iter()
        .filter(|f| f.suppression.is_none())
        .map(|f| f.fingerprint.as_str())
        .collect();
    serde_json::json!({"version": "emi-baseline-1", "fingerprints": prints})
}

/// Read a baseline file's fingerprints.
pub fn read_baseline(path: &Path) -> Result<BTreeSet<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(v["fingerprints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect())
}

/// Message placeholders must name captures of the rule's query.
fn placeholder_problems(rule: &Rule) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = rule.message.as_str();
    while let Some(i) = rest.find("{{") {
        let Some(j) = rest[i..].find("}}") else { break };
        let name = &rest[i + 2..i + j];
        if !rule.query.contains(&format!("@{name}")) {
            out.push(format!(
                "{}: message placeholder {{{{{name}}}}} names no capture",
                rule.id
            ));
        }
        rest = &rest[i + j + 2..];
    }
    out
}

/// Run every rule's `examples`: each `match` snippet must hit, each
/// `no_match` snippet must not. Returns the failures.
pub fn self_test(rules: &[Rule]) -> Vec<String> {
    let mut failures = Vec::new();
    for rule in rules {
        failures.extend(placeholder_problems(rule));
        let compiled = match engine::compile(rule.clone()) {
            Ok(c) => c,
            Err(e) => {
                failures.push(e);
                continue;
            }
        };
        let file = |lang: model::Language| match lang {
            model::Language::Python => {
                if rule.tests == Tests::Only {
                    "tests/test_x.py"
                } else {
                    "x.py"
                }
            }
            model::Language::Js => {
                if rule.tests == Tests::Only {
                    "x.test.js"
                } else {
                    "x.js"
                }
            }
            model::Language::Ts => {
                if rule.tests == Tests::Only {
                    "x.test.ts"
                } else {
                    "x.ts"
                }
            }
        };
        for lang in &rule.languages {
            let mut exercised = 0;
            for (expect, snippets) in [
                (true, &rule.examples.matching),
                (false, &rule.examples.no_match),
            ] {
                for snippet in snippets {
                    // An example speaks for the languages it parses in.
                    let parses = Grammar::of(file(*lang))
                        .and_then(|g| engine::parse(g, snippet))
                        .is_some_and(|t| !t.root_node().has_error());
                    if !parses {
                        continue;
                    }
                    exercised += usize::from(expect);
                    let hits = check_source(
                        std::slice::from_ref(&compiled),
                        file(*lang),
                        snippet,
                        "2000-01-01",
                    );
                    let hit = hits.iter().any(|(h, _)| h.rule == rule.id);
                    if hit != expect
                        && Grammar::of(file(*lang)).is_some_and(|g| compiled.applies_to(g))
                    {
                        let what = if expect {
                            "should match"
                        } else {
                            "should not match"
                        };
                        failures.push(format!("{} ({lang:?}) {what}: {snippet}", rule.id));
                    }
                }
            }
            if exercised == 0 {
                failures.push(format!(
                    "{} ({lang:?}) has no matching example that parses in it",
                    rule.id
                ));
            }
        }
    }
    failures
}
