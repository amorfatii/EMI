//! JavaScript/TypeScript scope assembly, following `js_probe.collect`.

use std::fs;
use std::path::Path;

use std::collections::{BTreeMap, BTreeSet};

use emi_ir::clones::Grouper;
use emi_ir::{Burden, Component, FileError, Measurement, pytext};
use emi_lang_js::FileMeasure;
use rayon::prelude::*;

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
    "coverage",
];

const CODE_EXTENSIONS: [&str; 8] = [".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"];

fn is_code(name: &str) -> bool {
    let lower = name.to_lowercase();
    CODE_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

enum Outcome {
    Symlink,
    Unreadable(&'static str, String),
    Parsed { sha: String, measure: FileMeasure },
    Unparsed,
}

fn measure_file(f: &ScopeFile) -> Outcome {
    if is_symlink(&f.path) {
        return Outcome::Symlink;
    }
    let raw = match fs::read(&f.path) {
        Ok(raw) => raw,
        Err(e) => return Outcome::Unreadable("OSError", truncate(&e.to_string())),
    };
    let text = match String::from_utf8(raw) {
        Ok(text) => text,
        Err(e) => return Outcome::Unreadable("UnicodeDecodeError", truncate(&e.to_string())),
    };
    let source = pytext::universal_newlines(&text);
    let sha = pytext::sha256_hex(source.as_bytes());
    match emi_lang_js::measure(&f.rel, &source) {
        Some(measure) => Outcome::Parsed { sha, measure },
        None => Outcome::Unparsed,
    }
}

fn file_error(file: &str, error: &str, message: String, lines: Option<Vec<u32>>) -> FileError {
    FileError {
        file: file.to_owned(),
        stage: None,
        error: error.to_owned(),
        message: Some(message),
        error_lines: lines,
    }
}

fn select(root: &Path, options: &Options, out: &mut Measurement) -> Vec<ScopeFile> {
    let excludes = options
        .exclude_dirs
        .clone()
        .unwrap_or_else(|| DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect());
    let (candidates, _pruned, walk_errors) = walk::walk(root, &excludes, is_code);
    out.errors.extend(
        walk_errors
            .into_iter()
            .map(|e| file_error(&e.rel, "ScanError", truncate(&e.message), None)),
    );
    candidates
        .into_iter()
        .filter(|f| fs::metadata(&f.path).is_ok_and(|m| m.is_file()))
        .filter(|f| walk::globs_keep(&f.rel, &options.exclude_globs, &options.include_globs))
        .collect()
}

/// Scope-wide accumulation across files.
#[derive(Default)]
struct Redundancy {
    exact: Grouper,
    normalized: Grouper,
    rule_lines: BTreeMap<String, BTreeSet<u32>>,
}

/// Add a parsed file's records. Syntax errors abort in raise mode and are
/// recorded (with the affected lines) otherwise.
fn add_parsed(
    out: &mut Measurement,
    red: &mut Redundancy,
    rel: &str,
    measure: FileMeasure,
    raise: bool,
) -> Result<(), ScanError> {
    if !measure.error_lines.is_empty() {
        let rows: Vec<u32> = measure.error_lines.iter().copied().collect();
        if raise {
            let message = format!("rows {rows:?}");
            return Err(ScanError::Syntax {
                file: rel.to_owned(),
                message,
            });
        }
        let message = "best-effort measurement; regions on error lines are unknown".to_owned();
        out.errors
            .push(file_error(rel, "SyntaxError", message, Some(rows)));
    }
    for c in &measure.clones {
        red.exact.add(c.exact, rel, &c.body_lines);
        red.normalized.add(c.normalized, rel, &c.body_lines);
    }
    out.verbosity_findings.extend(measure.findings);
    red.rule_lines.insert(rel.to_owned(), measure.rule_lines);
    if measure.sloc > 0 {
        out.components.push(Component {
            id: rel.to_owned(),
            sloc: measure.sloc,
            burden: Burden {
                erosion: measure.erosion,
                redundancy: None,
            },
        });
        out.functions.extend(measure.functions);
        out.regions.extend(measure.regions);
    }
    Ok(())
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    let mut out = Measurement {
        measurement_profile: emi_lang_js::PROFILE.to_owned(),
        ..Measurement::default()
    };
    let selected = select(root, options, &mut out);
    if selected.is_empty() {
        return Err(ScanError::NoSources(
            "no JavaScript/TypeScript source files",
        ));
    }
    let raise = options.on_syntax_error == OnSyntaxError::Raise;
    let outcomes: Vec<Outcome> = selected.par_iter().map(measure_file).collect();
    let (mut fallbacks, mut measured) = (Vec::new(), 0usize);
    let mut red = Redundancy::default();
    for (f, outcome) in selected.iter().zip(outcomes) {
        let rel = f.rel.clone();
        match outcome {
            Outcome::Symlink if raise => return Err(ScanError::Symlink(rel)),
            Outcome::Unreadable(_, message) if raise => {
                return Err(ScanError::Read { file: rel, message });
            }
            Outcome::Symlink => {
                let message = "symlinked source is unsupported".to_owned();
                out.errors
                    .push(file_error(&rel, "SymlinkError", message, None));
            }
            Outcome::Unreadable(name, message) => {
                out.errors.push(file_error(&rel, name, message, None))
            }
            Outcome::Unparsed => {
                return Err(ScanError::Syntax {
                    file: rel,
                    message: "parser unavailable".into(),
                });
            }
            Outcome::Parsed { sha, measure } => {
                out.source_sha256.insert(rel.clone(), sha);
                if measure.fallback {
                    fallbacks.push(rel.clone());
                }
                measured += 1;
                add_parsed(&mut out, &mut red, &rel, measure, raise)?;
            }
        }
    }
    if measured == 0 && out.errors.is_empty() {
        return Err(ScanError::NoSources(
            "no measurable JavaScript/TypeScript source files",
        ));
    }
    out.errors
        .sort_by(|a, b| (&a.file, &a.error, &a.message).cmp(&(&b.file, &b.error, &b.message)));
    fallbacks.sort();
    out.fallbacks = Some(fallbacks);
    let mut clone_lines = BTreeMap::new();
    out.clone_groups = red.exact.finish(&mut clone_lines);
    out.clone_groups_normalized = red.normalized.finish(&mut clone_lines);
    let empty = BTreeSet::new();
    for c in &mut out.components {
        let clones = clone_lines.get(&c.id).unwrap_or(&empty);
        let rules = red.rule_lines.get(&c.id).unwrap_or(&empty);
        // Unlike the Python adapter, JS redundancy is not capped at size.
        c.burden.redundancy = Some(u32::try_from(clones.union(rules).count()).unwrap_or(u32::MAX));
    }
    Ok(out)
}
