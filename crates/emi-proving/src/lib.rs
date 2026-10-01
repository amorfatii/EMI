//! Change evidence: whether the behaviour a task asked for exists in the
//! candidate, shown by the tests the candidate adds.
//!
//! An added test is a **proving test** when it fails or errors against the
//! baseline's product code and passes on the candidate, and a **redundant
//! test** when it already passes at the baseline. A function the candidate
//! changed is **proven** when a proving test executes a line the change
//! wrote in it, **unproven** when none does. Whatever could not be decided
//! is `unknown` with its reason, never a guess and never an empty list.
//!
//! [`measure`] runs the measurement and returns the receipt's `change`
//! object ([`ChangeEvidence::to_json`]). Python with pytest is measured;
//! any other language is an explicit `unknown`.
//!
//! The pure parts (reading JUnit XML, classifying a test, judging a
//! function, the status rule) are separate from the sandboxed runs, so each
//! has its own truth table.

pub mod classify;
pub mod evidence;
mod exec;
pub mod junit;
mod measure;
mod overlay;
mod proof;
pub mod pytest;

use std::path::PathBuf;
use std::time::Duration;

pub use classify::BaselineResult;
pub use evidence::{ChangeEvidence, FunctionEvidence, ProvingTest, Status, UnknownTest};
pub use measure::measure;
pub use proof::FnStatus;

/// The candidate revision: a ref, or the working tree as it is on disk
/// (frozen into one commit before anything is read from it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    Ref(String),
    Worktree,
}

/// The language of the product code and its tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Language {
    Python,
    /// Not measured; the name is kept to say so.
    Other(String),
}

impl Language {
    /// `python` is measured; anything else is carried by name.
    pub fn from_name(name: &str) -> Language {
        match name.trim().to_ascii_lowercase().as_str() {
            "python" | "py" => Language::Python,
            _ => Language::Other(name.to_owned()),
        }
    }
}

/// What to measure and how to run it.
#[derive(Clone, Debug)]
pub struct Request {
    pub repo: PathBuf,
    pub language: Language,
    /// The baseline: a ref or a commit sha.
    pub baseline: String,
    pub candidate: Candidate,
    /// The test command, argv with no shell: pytest with options (path
    /// selectors in it only decide which tests are listed).
    pub test_command: Vec<String>,
    /// Globs of the test files, repository-relative (`tests/*`).
    pub test_globs: Vec<String>,
    /// The product directory, repository-relative (`src`); empty for the
    /// repository root. Coverage is also measured where changed files are.
    pub source: String,
    /// Ignored dependency directories linked read-only into every copy
    /// (`.venv`).
    pub link: Vec<String>,
    /// Extra `KEY=VALUE` for the test environment.
    pub env: Vec<(String, String)>,
    /// Let the test runs use the network.
    pub network: bool,
    /// Upper bound for one run of the test command.
    pub timeout: Duration,
    /// Also require that a proving test kills a mutant on the changed
    /// lines that no other test kills (opt-in; see `measure`).
    pub strict: bool,
}

impl Request {
    /// A request with the defaults: the working tree is the candidate, no
    /// links, no network, ten minutes per run, the plain rule.
    pub fn new(repo: impl Into<PathBuf>, baseline: &str, test_command: Vec<String>) -> Request {
        Request {
            repo: repo.into(),
            language: Language::Python,
            baseline: baseline.to_owned(),
            candidate: Candidate::Worktree,
            test_command,
            test_globs: vec!["tests/*".to_owned()],
            source: String::new(),
            link: Vec::new(),
            env: Vec::new(),
            network: false,
            timeout: Duration::from_secs(600),
            strict: false,
        }
    }
}
