//! Measure a source scope with one language adapter: enumerate files under
//! the v0.5 selection rules, measure them in parallel, and assemble the
//! result in the oracle's order.

pub mod classify;
mod coupling;
pub mod exports;
mod go;
mod graded;
mod js;
mod native;
mod python;
mod python_coupling;
mod rust;
pub mod walk;

use std::path::Path;

use emi_ir::Measurement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Python,
    Js,
    Rust,
    Go,
}

impl Probe {
    /// `python`, `js` or `rust`.
    pub fn from_name(name: &str) -> Option<Probe> {
        match name {
            "python" => Some(Probe::Python),
            "js" => Some(Probe::Js),
            "rust" => Some(Probe::Rust),
            "go" => Some(Probe::Go),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Probe::Python => "python",
            Probe::Js => "js",
            Probe::Rust => "rust",
            Probe::Go => "go",
        }
    }
}

/// Which measurement contract to apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    /// Reproduces the v0.5.1 oracle exactly.
    #[default]
    Parity,
    /// Static coupling with value origins: dispatch through parameters and
    /// fields is attributed to the code that obtained the value.
    Precise,
    /// `precise` coupling, with function erosion graded by cognitive
    /// complexity and Halstead volume (study G2, emi-lab `results/G2`).
    Graded,
}

impl Profile {
    /// `parity`, `precise` or `graded`; anything else is rejected.
    pub fn from_name(name: &str) -> Option<Profile> {
        match name {
            "parity" => Some(Profile::Parity),
            "precise" => Some(Profile::Precise),
            "graded" => Some(Profile::Graded),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Parity => "parity",
            Profile::Precise => "precise",
            Profile::Graded => "graded",
        }
    }

    /// Coupling with value origins (`precise` and `graded`).
    pub fn precise_coupling(self) -> bool {
        matches!(self, Profile::Precise | Profile::Graded)
    }

    /// The adapter's measurement profile string under this profile.
    fn adapter(self, parity: &str, precise: &str) -> String {
        match self {
            Profile::Parity => parity.to_owned(),
            Profile::Precise => precise.to_owned(),
            Profile::Graded => format!("{precise}{}", graded::SUFFIX),
        }
    }
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
    /// JS only: directory tsconfig.json loads from (default: the scope root).
    pub config_root: Option<std::path::PathBuf>,
    /// Python only: also skip paths matched by the root `.gitignore`
    /// (the oracle's simple subset: no negations).
    pub use_gitignore: bool,
    pub profile: Profile,
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
            config_root: None,
            use_gitignore: false,
            profile: Profile::Parity,
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
    /// Nothing measurable. `empty_side` marks the cases a diff treats as a
    /// valid empty revision (no sources, test-only) rather than a failure.
    #[error("{message}")]
    NoSources { message: String, empty_side: bool },
}

impl ScanError {
    fn empty(message: impl Into<String>) -> Self {
        ScanError::NoSources {
            message: message.into(),
            empty_side: true,
        }
    }

    fn unmeasurable(message: impl Into<String>) -> Self {
        ScanError::NoSources {
            message: message.into(),
            empty_side: false,
        }
    }
}

/// Identity of this engine in measurement options (in place of the
/// oracle's interpreter version).
pub const ENGINE: &str = concat!("emi-rs-", env!("CARGO_PKG_VERSION"));

fn syntax_policy(options: &Options) -> &'static str {
    match options.on_syntax_error {
        OnSyntaxError::Raise => "raise",
        OnSyntaxError::Record => "record",
    }
}

fn sorted(items: &[String]) -> Vec<String> {
    let mut v = items.to_vec();
    v.sort();
    v
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    match options.probe {
        Probe::Python => python::scan(root, options),
        Probe::Js => js::scan(root, options),
        Probe::Rust => native::scan(&rust::Rust, root, options),
        Probe::Go => native::scan(&go::Go, root, options),
    }
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// `str(OSError)`: `[Errno N] strerror: 'path'`.
fn os_error(e: &std::io::Error, path: &Path) -> String {
    let text = e.to_string();
    let strerror = text
        .rsplit_once(" (os error ")
        .map_or(text.as_str(), |(s, _)| s);
    match e.raw_os_error() {
        Some(code) => format!(
            "[Errno {code}] {strerror}: {}",
            emi_ir::pyrepr::string(&path.to_string_lossy())
        ),
        None => text.clone(),
    }
}

fn truncate(message: &str) -> String {
    message.chars().take(300).collect()
}
