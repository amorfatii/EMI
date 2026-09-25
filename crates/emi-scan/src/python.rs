//! Python scope assembly, following `python_probe._collect_snapshot`.

use std::fs;
use std::path::Path;

use emi_ir::{Burden, Component, FileError, InventoryEntry, Measurement, pytext};
use emi_lang_python::FileMeasure;
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
    },
}

fn measure_file(f: &ScopeFile) -> Outcome {
    if is_symlink(&f.path) {
        return Outcome::Symlink;
    }
    let raw = match fs::read(&f.path) {
        Ok(raw) => raw,
        Err(e) => {
            return Outcome::Unreadable(io_error_name(&e).to_owned(), truncate(&e.to_string()));
        }
    };
    let sha = pytext::sha256_hex(&raw);
    let source = match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => {
            let message = truncate(&e.to_string());
            return Outcome::Failed {
                sha,
                stage: "decode",
                error: "UnicodeDecodeError",
                message,
            };
        }
    };
    match emi_lang_python::measure(&f.rel, &source) {
        Ok(measure) => Outcome::Measured { sha, measure },
        Err(e) => Outcome::Failed {
            sha,
            stage: "parse",
            error: e.error,
            message: truncate(&e.message),
        },
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
    }
}

/// Accumulates records and the file inventory for one scan.
struct Scan {
    out: Measurement,
    inventory: Vec<InventoryEntry>,
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
    ) -> Result<Option<FileMeasure>, ScanError> {
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
                self.out.source_sha256.insert(rel.clone(), sha);
                self.failed(&rel, "failed", error(&rel, stage, name, message));
                Ok(None)
            }
            Outcome::Measured { sha, measure } => {
                self.out.source_sha256.insert(rel, sha);
                Ok(Some(measure))
            }
        }
    }

    /// Add a parsed file: tests go to the inventory only, product files with
    /// eligible lines become components.
    fn product(&mut self, rel: &str, m: FileMeasure, detect_tests: bool) {
        if detect_tests && is_test_file(rel) {
            self.inventory.push(entry(rel, "test-only", None));
            return;
        }
        let state = if m.sloc > 0 {
            "measured"
        } else {
            "zero-eligible"
        };
        self.inventory.push(entry(rel, state, None));
        if m.sloc == 0 {
            return;
        }
        self.out.components.push(Component {
            id: rel.to_owned(),
            sloc: m.sloc,
            burden: Burden {
                erosion: emi_ir::Evidence::Exact(m.erosion),
            },
        });
        self.out.functions.extend(m.functions);
        self.out.regions.extend(m.regions);
    }
}

/// Walk the scope and apply glob selection; walk failures and glob
/// exclusions are recorded, never dropped.
fn select(root: &Path, options: &Options, scan: &mut Scan) -> Vec<ScopeFile> {
    let excludes = options
        .exclude_dirs
        .clone()
        .unwrap_or_else(|| DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect());
    let (candidates, _pruned, walk_errors) = walk::walk(root, &excludes, |n| n.ends_with(".py"));
    for e in walk_errors {
        scan.failed(
            &e.rel,
            "unreadable",
            error(&e.rel, "read", "OSError", truncate(&e.message)),
        );
    }
    let mut selected = Vec::new();
    for f in candidates {
        if walk::globs_keep(&f.rel, &options.exclude_globs, &options.include_globs) {
            selected.push(f);
        } else {
            scan.inventory.push(entry(&f.rel, "excluded", Some("glob")));
        }
    }
    selected
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    let out = Measurement {
        measurement_profile: emi_lang_python::PROFILE.to_owned(),
        ..Measurement::default()
    };
    let mut scan = Scan {
        out,
        inventory: Vec::new(),
    };
    let selected = select(root, options, &mut scan);
    let outcomes: Vec<Outcome> = selected.par_iter().map(measure_file).collect();
    let raise = options.on_syntax_error == OnSyntaxError::Raise;
    let mut parsed = Vec::new();
    for (f, outcome) in selected.iter().zip(outcomes) {
        if let Some(m) = scan.outcome(f, outcome, raise)? {
            parsed.push((f, m));
        }
    }
    if parsed.is_empty() && scan.out.errors.is_empty() {
        return Err(ScanError::NoSources("no parseable Python source files"));
    }
    for (f, m) in parsed {
        scan.product(&f.rel, m, options.detect_tests);
    }
    scan.inventory.sort_by(|a, b| a.file.cmp(&b.file));
    scan.out.inventory = Some(scan.inventory);
    Ok(scan.out)
}
