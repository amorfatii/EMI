//! Python scope assembly, following `python_probe._collect_snapshot`.

use std::fs;
use std::path::Path;

use std::collections::{BTreeMap, BTreeSet};

use emi_ir::clones::Grouper;
use emi_ir::{
    Burden, Component, Evidence, FileError, Finding, InventoryEntry, Measurement, pytext,
};
use emi_lang_python::FileMeasure;
use rayon::prelude::*;

use crate::coupling::apply;
use crate::python_coupling::{Product, couple_all, evidence};
use crate::walk::{self, ScopeFile};
use crate::{OnSyntaxError, Options, ScanError, is_symlink, truncate};

pub const DEFAULT_EXCLUDES: [&str; 14] = [
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
];

/// Test-infrastructure paths never count as product source.
pub fn is_test_file(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let name = parts.last().copied().unwrap_or_default();
    if name == "conftest.py" || name.starts_with("test_") || name.ends_with("_test.py") {
        return true;
    }
    parts[..parts.len().saturating_sub(1)]
        .iter()
        .any(|p| matches!(*p, "tests" | "test"))
}

enum Parsed {
    Product(FileMeasure, String),
    Test(Vec<Finding>),
}

enum Outcome {
    Symlink,
    Unreadable(String, String),
    Failed {
        sha: String,
        stage: &'static str,
        error: &'static str,
        message: String,
    },
    Measured {
        sha: String,
        measure: FileMeasure,
        source: String,
    },
    Test {
        sha: String,
        findings: Vec<Finding>,
    },
}

fn measure_file(f: &ScopeFile, detect_tests: bool) -> Outcome {
    if is_symlink(&f.path) {
        return Outcome::Symlink;
    }
    let raw = match fs::read(&f.path) {
        Ok(raw) => raw,
        Err(e) => {
            return Outcome::Unreadable(
                io_error_name(&e).to_owned(),
                truncate(&crate::os_error(&e, &f.path)),
            );
        }
    };
    let sha = pytext::sha256_hex(&raw);
    let source = match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => {
            let message = truncate(&pytext::utf8_error(e.as_bytes(), &e.utf8_error()));
            return Outcome::Failed {
                sha,
                stage: "decode",
                error: "UnicodeDecodeError",
                message,
            };
        }
    };
    let parse_failed = |sha, e: emi_lang_python::ParseFailure| Outcome::Failed {
        sha,
        stage: "parse",
        error: e.error,
        message: truncate(&e.message),
    };
    if detect_tests && is_test_file(&f.rel) {
        return match emi_lang_python::measure_test(&f.rel, &source) {
            Ok(findings) => Outcome::Test { sha, findings },
            Err(e) => parse_failed(sha, e),
        };
    }
    match emi_lang_python::measure(&f.rel, &source) {
        Ok(measure) => Outcome::Measured {
            sha,
            measure,
            source,
        },
        Err(e) => parse_failed(sha, e),
    }
}

fn io_error_name(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::IsADirectory => "IsADirectoryError",
        _ => "OSError",
    }
}

fn entry(file: &str, state: &str, reason: Option<&str>) -> InventoryEntry {
    InventoryEntry {
        file: file.to_owned(),
        state: state.to_owned(),
        reason: reason.map(str::to_owned),
    }
}

fn error(file: &str, stage: &str, error: &str, message: String) -> FileError {
    FileError {
        file: file.to_owned(),
        stage: Some(stage.to_owned()),
        error: error.to_owned(),
        message: Some(message),
        error_lines: None,
        sha256: None,
    }
}

/// Accumulates records and the file inventory for one scan.
struct Scan {
    out: Measurement,
    inventory: Vec<InventoryEntry>,
    exact_clones: Grouper,
    normalized_clones: Grouper,
    rule_lines: BTreeMap<String, BTreeSet<u32>>,
}

impl Scan {
    fn failed(&mut self, rel: &str, state: &str, err: FileError) {
        self.out.errors.push(err);
        self.inventory.push(entry(rel, state, None));
    }

    /// Record one file's outcome; in raise mode the first failure aborts.
    fn outcome(
        &mut self,
        f: &ScopeFile,
        outcome: Outcome,
        raise: bool,
    ) -> Result<Option<Parsed>, ScanError> {
        let rel = f.rel.clone();
        match outcome {
            Outcome::Symlink if raise => Err(ScanError::Symlink(rel)),
            Outcome::Unreadable(_, message) if raise => Err(ScanError::Read { file: rel, message }),
            Outcome::Failed { message, .. } if raise => {
                Err(ScanError::Syntax { file: rel, message })
            }
            Outcome::Symlink => {
                let message = "symlinked source is unsupported".to_owned();
                self.failed(
                    &rel,
                    "unsupported",
                    error(&rel, "symlink", "SymlinkError", message),
                );
                Ok(None)
            }
            Outcome::Unreadable(name, message) => {
                self.failed(&rel, "unreadable", error(&rel, "read", &name, message));
                Ok(None)
            }
            Outcome::Failed {
                sha,
                stage,
                error: name,
                message,
            } => {
                let mut err = error(&rel, stage, name, message);
                err.sha256 = Some(sha.clone());
                self.out.source_sha256.insert(rel.clone(), sha);
                self.failed(&rel, "failed", err);
                Ok(None)
            }
            Outcome::Measured {
                sha,
                measure,
                source,
            } => {
                self.out.source_sha256.insert(rel, sha);
                Ok(Some(Parsed::Product(measure, source)))
            }
            Outcome::Test { sha, findings } => {
                self.out.source_sha256.insert(rel, sha);
                Ok(Some(Parsed::Test(findings)))
            }
        }
    }

    /// Add a parsed file: tests contribute test-value findings and an
    /// inventory entry; product files wait for the coupling pass.
    fn add(
        &mut self,
        rel: &str,
        parsed: Parsed,
        tests: &mut Vec<(String, Vec<Finding>)>,
        products: &mut Vec<Product>,
    ) {
        match parsed {
            Parsed::Test(findings) => {
                self.inventory.push(entry(rel, "test-only", None));
                tests.push((rel.to_owned(), findings));
            }
            Parsed::Product(measure, source) => {
                let state = if measure.sloc > 0 {
                    "measured"
                } else {
                    "zero-eligible"
                };
                self.inventory.push(entry(rel, state, None));
                products.push(Product {
                    rel: rel.to_owned(),
                    measure,
                    source,
                });
            }
        }
    }

    /// Records of product files with eligible lines, in file order.
    fn component(&mut self, rel: &str, mut m: FileMeasure, coupling: Evidence) {
        self.out.components.push(Component {
            id: rel.to_owned(),
            sloc: m.sloc,
            burden: Burden {
                erosion: Evidence::Exact(m.erosion),
                redundancy: None,
                coupling: Some(coupling),
            },
        });
        for c in &m.clones {
            self.exact_clones.add(c.exact, rel, &c.body_lines);
            self.normalized_clones.add(c.normalized, rel, &c.body_lines);
        }
        self.out.verbosity_findings.append(&mut m.findings);
        self.rule_lines
            .insert(rel.to_owned(), std::mem::take(&mut m.rule_lines));
        self.out.functions.append(&mut m.functions);
        self.out.regions.append(&mut m.regions);
    }

    /// V per component: clone and scored-rule lines, capped at component size.
    fn redundancy(&mut self, clone_lines: &BTreeMap<String, BTreeSet<u32>>) {
        let empty = BTreeSet::new();
        for c in &mut self.out.components {
            let clones = clone_lines.get(&c.id).unwrap_or(&empty);
            let rules = self.rule_lines.get(&c.id).unwrap_or(&empty);
            let flagged = u32::try_from(clones.union(rules).count()).unwrap_or(u32::MAX);
            c.burden.redundancy = Some(flagged.min(c.sloc));
        }
    }

    fn finish_clones(&mut self) -> BTreeMap<String, BTreeSet<u32>> {
        let mut clone_lines = BTreeMap::new();
        let exact = std::mem::take(&mut self.exact_clones);
        let normalized = std::mem::take(&mut self.normalized_clones);
        self.out.clone_groups = exact.finish(&mut clone_lines);
        self.out.clone_groups_normalized = normalized.finish(&mut clone_lines);
        clone_lines
    }
}

/// `_gitignore_effective` patterns: the root `.gitignore`'s nonblank,
/// non-comment lines with outer slashes stripped; none unless enabled.
fn gitignore_patterns(root: &Path, enabled: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    let Ok(raw) = fs::read(root.join(".gitignore")) else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(raw) else {
        return Vec::new();
    };
    pytext::splitlines(&text)
        .into_iter()
        .map(pytext::strip)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_end_matches('/').trim_start_matches('/').to_owned())
        .collect()
}

/// `_ignored_by_gitignore`: the oracle's simple subset (no negations).
fn gitignored(rel: &str, patterns: &[String]) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    patterns.iter().filter(|p| !p.is_empty()).any(|pattern| {
        if pattern.contains('/') {
            walk::fnmatch(rel, pattern) || rel.starts_with(pattern.trim_end_matches('*'))
        } else {
            parts.contains(&pattern.as_str())
                || parts.iter().any(|part| walk::fnmatch(part, pattern))
        }
    })
}

/// `_collect_snapshot` options: the extraction contract.
fn measurement_options(
    options: &Options,
    excludes: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let value = serde_json::json!({
        "adapter": emi_lang_python::PROFILE,
        "engine": crate::ENGINE,
        "detect_tests": options.detect_tests,
        "on_syntax_error": crate::syntax_policy(options),
        "exclude_dirs": crate::sorted(excludes),
        "exclude_globs": crate::sorted(&options.exclude_globs),
        "include_globs": crate::sorted(&options.include_globs),
        "use_gitignore": options.use_gitignore,
    });
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

/// Stamp status, completeness and the inventory summary (`_finish`).
fn finish(out: &mut Measurement, status: &str) {
    let inventory = out.inventory.get_or_insert_with(Vec::new);
    inventory.sort_by(|a, b| a.file.cmp(&b.file));
    let mut summary = serde_json::Map::new();
    for e in inventory.iter() {
        let n = summary
            .get(&e.state)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        summary.insert(e.state.clone(), serde_json::Value::from(n + 1));
    }
    out.inventory_summary = Some(summary);
    out.scope_complete = Some(
        out.errors.is_empty() && matches!(status, "ok" | "empty" | "test-only" | "no-eligible"),
    );
    out.status = Some(status.to_owned());
}

/// Walk the scope and apply glob selection; walk failures and glob
/// exclusions are recorded, never dropped.
fn select(root: &Path, options: &Options, excludes: &[String], scan: &mut Scan) -> Vec<ScopeFile> {
    let gitignore = gitignore_patterns(root, options.use_gitignore);
    let (candidates, _pruned, walk_errors) = walk::walk(root, excludes, |n| n.ends_with(".py"));
    for e in walk_errors {
        scan.failed(
            &e.rel,
            "unreadable",
            error(&e.rel, "read", "OSError", truncate(&e.message)),
        );
    }
    let mut selected = Vec::new();
    for f in candidates {
        if !walk::globs_keep(&f.rel, &options.exclude_globs, &options.include_globs) {
            scan.inventory.push(entry(&f.rel, "excluded", Some("glob")));
        } else if gitignored(&f.rel, &gitignore) {
            scan.inventory
                .push(entry(&f.rel, "excluded", Some("gitignore")));
        } else {
            selected.push(f);
        }
    }
    selected
}

/// An empty measurement carrying the extraction contract and its id.
fn new_measurement(options: &Options, excludes: &[String]) -> Measurement {
    let options_map = measurement_options(options, excludes);
    Measurement {
        measurement_profile: emi_lang_python::PROFILE.to_owned(),
        measurement_contract_id: Some(emi_ledger::pyfmt::digest(
            "measurement-contract",
            &serde_json::Value::Object(options_map.clone()),
        )),
        measurement_options: Some(options_map),
        ..Measurement::default()
    }
}

/// The oracle's `collect()` refusal for a scope of only test files.
fn test_only(inventory: &[InventoryEntry]) -> ScanError {
    let n = inventory.iter().filter(|e| e.state == "test-only").count();
    let hint = if n > 0 {
        format!(" (all {n} files detected as tests)")
    } else {
        String::new()
    };
    ScanError::empty(format!("no measurable product source files{hint}"))
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    if root.symlink_metadata().is_err() {
        let message = format!(
            "no Python source files (scope root not found: {})",
            root.display()
        );
        return Err(ScanError::empty(message));
    }
    let excludes = options
        .exclude_dirs
        .clone()
        .unwrap_or_else(|| DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect());
    let mut scan = Scan {
        out: new_measurement(options, &excludes),
        inventory: Vec::new(),
        exact_clones: Grouper::default(),
        normalized_clones: Grouper::default(),
        rule_lines: BTreeMap::new(),
    };
    let selected = select(root, options, &excludes, &mut scan);
    let outcomes: Vec<Outcome> = selected
        .par_iter()
        .map(|f| measure_file(f, options.detect_tests))
        .collect();
    let raise = options.on_syntax_error == OnSyntaxError::Raise;
    let mut parsed = Vec::new();
    for (f, outcome) in selected.iter().zip(outcomes) {
        if let Some(m) = scan.outcome(f, outcome, raise)? {
            parsed.push((f, m));
        }
    }
    if parsed.is_empty() {
        if scan.out.errors.is_empty() {
            return Err(ScanError::empty("no Python source files"));
        }
        // Every file failed: the snapshot carries the errors, no hashes.
        scan.out.source_sha256.clear();
        scan.out.inventory = Some(scan.inventory);
        finish(&mut scan.out, "all-failed");
        return Ok(scan.out);
    }
    let (mut tests, mut products) = (Vec::new(), Vec::new());
    for (f, m) in parsed {
        scan.add(&f.rel, m, &mut tests, &mut products);
    }
    if products.is_empty() {
        return Err(test_only(&scan.inventory));
    }
    let measured = products.iter().any(|p| p.measure.sloc > 0);
    let coupled = couple_all(&mut products)?;
    for (p, fc) in products.into_iter().zip(&coupled.files) {
        if p.measure.sloc == 0 {
            continue;
        }
        let mut measure = p.measure;
        let in_cycle = coupled
            .cycle_modules
            .contains(&emi_lang_python::coupling::module_name(&p.rel));
        let c = apply(
            &mut measure.functions,
            &mut measure.regions,
            &evidence(fc),
            in_cycle,
        );
        scan.component(&p.rel, measure, c);
    }
    scan.out.static_dependency_cycles = coupled.cycles;
    // The oracle orders test-value findings by file name (string order).
    tests.sort_by(|a, b| a.0.cmp(&b.0));
    scan.out.test_value_findings = Some(tests.into_iter().flat_map(|(_, f)| f).collect());
    let clone_lines = scan.finish_clones();
    scan.redundancy(&clone_lines);
    scan.out.inventory = Some(scan.inventory);
    finish(&mut scan.out, if measured { "ok" } else { "no-eligible" });
    Ok(scan.out)
}
