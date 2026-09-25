//! Measure a source scope with one language adapter: enumerate files under
//! the v0.5 selection rules, measure them in parallel, and assemble the
//! result in the oracle's order.

mod js;
mod python;
pub mod walk;

use std::path::Path;

use emi_ir::Measurement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Python,
    Js,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnSyntaxError {
    /// Abort on the first unreadable or unparseable file.
    Raise,
    /// Record the failure and keep measuring the rest.
    Record,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub probe: Probe,
    pub on_syntax_error: OnSyntaxError,
    /// Directory names pruned during the walk; `None` uses the adapter default.
    pub exclude_dirs: Option<Vec<String>>,
    pub exclude_globs: Vec<String>,
    pub include_globs: Vec<String>,
    /// Python only: keep test files out of product measurement.
    pub detect_tests: bool,
}

impl Options {
    pub fn new(probe: Probe) -> Self {
        Options {
            probe,
            on_syntax_error: OnSyntaxError::Raise,
            exclude_dirs: None,
            exclude_globs: Vec::new(),
            include_globs: Vec::new(),
            detect_tests: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("syntax errors in {file}: {message}")]
    Syntax { file: String, message: String },
    #[error("cannot read {file}: {message}")]
    Read { file: String, message: String },
    #[error("symlinked source is unsupported: {0}")]
    Symlink(String),
    #[error("{0}")]
    NoSources(&'static str),
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    match options.probe {
        Probe::Python => python::scan(root, options),
        Probe::Js => js::scan(root, options),
    }
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

fn truncate(message: &str) -> String {
    message.chars().take(300).collect()
}
