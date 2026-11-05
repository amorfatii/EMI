//! Change evidence: whether the behaviour a task asked for exists in the
//! candidate, shown by the tests the candidate adds.
//!
//! An added test is a **proving test** when it fails or errors against the
//! baseline's product code, passes on the candidate, and executes changed
//! product code in its own context. An old test the candidate edited is a
//! proving test by the same rule, in its edited form. A **redundant test**
//! is an added test that already passes at the baseline or executes no
//! changed body, unless another case of the same parametrized test is a
//! proving test: it is then that test's **companion**. An added test that
//! is skipped at both revisions is listed as skipped. A test file only the
//! candidate cannot collect is listed with the failing tests. A function the candidate
//! changed is **exercised** when a proving test executes a line the change
//! wrote in it, **unexercised** when none does. Whatever could not be decided
//! is `unknown` with its reason, never a guess and never an empty list.
//!
//! [`measure`] runs the measurement and returns the receipt's `change`
//! object ([`ChangeEvidence::to_json`]). Python with pytest is measured;
//! any other language is an explicit `unknown`.
//!
//! The pure parts (reading JUnit XML, classifying a test, judging a
//! function, the status rule) are separate from the sandboxed runs, so each
//! has its own truth table.
//!
//! What the result does not say:
//!
//! - Tests are run as chosen node ids, one file per run, so a test that
//!   depends on another running first is judged without it.
//! - A test file that fails to import at the baseline (it names a function
//!   the candidate added) reports one error for the whole file, so every
//!   added test in it is `errored` at the baseline, including ones that do
//!   not use the new name. Only tests whose own execution reaches changed
//!   product code receive proving credit.
//! - A test file moved without git seeing a rename (too little in common)
//!   looks like new tests, which pass at the baseline and so look redundant.
//! - The stricter rule (a proving test must kill a mutant no other test
//!   kills, `Request::strict`) is not measured: asking for it adds a
//!   `not_measured` entry and changes nothing else.
//! - The test report is written by a process the candidate controls. It is
//!   cross-checked against the run's exit code and the listed ids, which
//!   catches a report that disagrees with them, not one that lies
//!   consistently.

pub mod classify;
mod contexts;
pub mod evidence;
mod exec;
pub mod junit;
mod measure;
mod overlay;
mod proof;
pub mod pytest;
mod rerun;

use std::path::PathBuf;
use std::time::Duration;

pub use classify::BaselineResult;
pub use evidence::{
    ChangeEvidence, FailingTest, FunctionEvidence, NOT_COLLECTED, ProvingTest, RedundantTest,
    Status, UnknownTest,
};
pub use measure::{measure, measure_in};
pub use proof::FnStatus;
pub use rerun::passing;

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
            // the default pins; a caller that configures others sets them
            // after these, and the last setting of a variable wins
            env: emi_kernel::pins::Pins::default().env(),
            network: false,
            timeout: Duration::from_secs(600),
            strict: false,
        }
    }
}
