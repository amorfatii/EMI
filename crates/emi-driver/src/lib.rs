//! The boundary between EMI's checks and a language (ADR 0009).
//!
//! `emi-done` runs the same stages for every language: the regression (the
//! project's tests, through the task engine), change proving, laws,
//! preserve, adequacy and escape-hatch signals. What a stage needs from a
//! language is a method of [`Driver`]; the types here say what those
//! methods take and return, and say nothing about any language.
//!
//! A driver that cannot measure a stage keeps the default, which says so.
//! The orchestrator reports that stage as not measured, with the reason,
//! never as a pass; a driver with no [`Driver::regression`] plan for a
//! command leaves the regression to the command's exit status, which the
//! receipt states.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub mod laws;

pub use laws::{Examples, Law, LawEvidence, LawFiles, Proposal};

/// The file a test id belongs to. Every driver names a test
/// `<file>::<name>`, the file repository-relative, so that a test is found
/// again from the file it lives in whatever the language.
pub fn file_of(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

/// One check of a candidate against a baseline, as every stage sees it.
#[derive(Clone, Debug)]
pub struct Run<'a> {
    /// The live repository: dependency links and hidden paths come from it.
    pub repo: &'a Path,
    /// A clean view of the repository that holds both commits.
    pub view: &'a Path,
    pub baseline: &'a str,
    /// The candidate, frozen as a commit.
    pub candidate: &'a str,
    /// The configured test command, as an argv list.
    pub command: &'a [String],
    /// Globs of the test files, repository-relative.
    pub test_globs: &'a [String],
    /// The configured source directory.
    pub source: &'a str,
    /// Dependency directories linked into every run.
    pub link: &'a [String],
    /// The pins' environment, the same at the baseline and on the
    /// candidate.
    pub pins: Vec<(String, String)>,
    /// Where the project's own code is imported or built from, beside the
    /// repository root.
    pub roots: &'a [String],
    /// Where results of baseline runs may be kept between checks.
    pub cache: Option<ListingCache>,
}

/// Where results of baseline runs are kept between checks, and the
/// environment they were taken in. A result is read only by a driver
/// whose own code is the same ([`Driver::producer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingCache {
    pub dir: PathBuf,
    /// The installed environment's identity (interpreter and packages).
    pub environment: String,
}

impl ListingCache {
    /// A cache under `root` for results taken in `environment` by code
    /// whose identity is `producer`.
    pub fn new(root: &Path, environment: &str, producer: &str) -> Self {
        Self {
            dir: root.join(producer),
            environment: environment.to_owned(),
        }
    }
}

/// How the task engine runs a test command so that each test's outcome
/// can be read from its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegressionPlan {
    /// The run, printing one outcome per test.
    pub run: Vec<String>,
    /// The listing of the tests the command would run.
    pub list: Vec<String>,
    /// The name the engine records for the output's format.
    pub format: &'static str,
}

/// The counts a test runner's closing summary states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub passed: u64,
    pub failed: u64,
    pub errors: u64,
    pub skipped: u64,
    /// Expected failures, and unexpected passes: neither ran to its
    /// written expectation.
    pub xfailed: u64,
    pub xpassed: u64,
}

impl Summary {
    /// The tests that ran: everything but the skipped.
    pub fn ran(&self) -> u64 {
        self.passed + self.failed + self.errors + self.xfailed + self.xpassed
    }
}

/// What preserve compares: the functions the change modifies, under these
/// paths (empty for all).
#[derive(Clone, Debug, Default)]
pub struct PreserveAsk<'a> {
    pub paths: &'a [String],
    pub call_timeout_s: f64,
    pub generated_examples: u32,
}

/// One escape-hatch signal in an added line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    pub line: u32,
    pub kind: &'static str,
    pub means: &'static str,
}

/// Why a stage measured nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unmeasured {
    /// The driver does not measure this stage.
    Unsupported(String),
    /// It tried, and could not.
    Failed(String),
}

impl Unmeasured {
    pub fn reason(&self) -> &str {
        match self {
            Unmeasured::Unsupported(why) | Unmeasured::Failed(why) => why,
        }
    }
}

/// A language, as EMI's checks need it.
pub trait Driver: Sync {
    /// The name in the configuration's `language`.
    fn language(&self) -> &'static str;

    /// The test runner, as messages name it.
    fn runner(&self) -> &'static str;

    /// The identity of what the driver itself puts into the runs (its
    /// version and any scripts), so that what one version produced is not
    /// read by another.
    fn producer(&self) -> String;

    fn unsupported(&self, stage: &str) -> Unmeasured {
        Unmeasured::Unsupported(format!(
            "the {} driver does not measure {stage}",
            self.language()
        ))
    }

    // --- the regression, run by the task engine

    /// How to run `command` so each test's outcome can be read; `None`
    /// when the driver cannot read it per test.
    fn regression(&self, command: &[String]) -> Option<RegressionPlan>;

    /// The environment every run of the project's code gets beside the
    /// pins, for code imported or built from `roots`.
    fn run_env(&self, roots: &[String]) -> Vec<(String, String)>;

    /// Write whatever applies the pins inside the test process into a
    /// run's directory.
    fn install_pins(&self, dir: &Path) -> Result<(), String>;

    /// Whether `path` configures the test runner: restored from the
    /// baseline like the tests, and changed only with an approval.
    fn is_runner_config(&self, path: &str) -> bool;

    /// The globs of the runner configuration files, for the engine's
    /// trusted set.
    fn runner_config_globs(&self) -> Vec<String>;

    /// A command that fails when the tests would not run the code being
    /// checked (an installed copy instead), for the project's files at the
    /// baseline; `None` when the language has no such question or the
    /// command cannot answer it.
    fn import_check(&self, _run: &Run, _files: &[String]) -> Option<Vec<String>> {
        None
    }

    /// The closing summary in a run's output.
    fn summary(&self, output: &str) -> Option<Summary>;

    /// The ids of the tests a run's output names as failed.
    fn failures(&self, output: &str) -> Vec<String>;

    /// The per-test outcome lines a run's output holds, as `(id,
    /// outcome)` with outcomes `passed`, `failed`, `error`, `skipped`,
    /// `xfail` and `xpass`; `None` when it holds none.
    fn outcome_lines(&self, _output: &str) -> Option<Vec<(String, &'static str)>> {
        None
    }

    /// A file named by an output line for a test file that could not be
    /// loaded at all, so none of its tests ran.
    fn uncollected_file<'l>(&self, _line: &'l str) -> Option<&'l str> {
        None
    }

    /// The test ids a listing run printed, when its output is in this
    /// driver's own form; `None` leaves it to the engine's `path::name`
    /// reading.
    fn listed(&self, _output: &str) -> Option<Result<BTreeSet<String>, String>> {
        None
    }

    /// The identity of the installed environment the tests run in
    /// (interpreter or toolchain, and the packages it holds), or why it
    /// could not be taken; `None` when the driver has none to give. Two
    /// checks with the same identity ran against the same installed code.
    fn environment(&self, _repo: &Path, _command: &[String]) -> Option<Result<String, String>> {
        None
    }

    /// Why an exit status of the test command says nothing about the
    /// tests (it found none to run, could not read its command line,
    /// failed inside itself), when the runner gives it that meaning.
    fn exit_says(&self, _code: i64) -> Option<String> {
        None
    }

    // --- the stages beyond the regression

    /// Whether the change's tests show the change: the receipt's
    /// `change` object.
    fn change(&self, _run: &Run) -> Result<Value, Unmeasured> {
        Err(self.unsupported("whether the change's tests show it"))
    }

    /// Of the failed old tests `ids`, those that pass when run by
    /// themselves on the candidate.
    fn passing(&self, _run: &Run, _ids: &[String]) -> Result<BTreeSet<String>, Unmeasured> {
        Err(self.unsupported("tests run by themselves"))
    }

    /// The laws the candidate is held to.
    fn laws(&self, _run: &Run, _files: &LawFiles) -> LawEvidence {
        LawEvidence::fault(self.unsupported("laws").reason())
    }

    /// The behaviour of the changed functions at the baseline and on the
    /// candidate, compared on recorded and generated inputs: a drift
    /// report.
    fn preserve(&self, _run: &Run, _ask: &PreserveAsk) -> Result<Value, Unmeasured> {
        Err(self.unsupported("kept behaviour"))
    }

    /// Mutants of the changed lines run against the tests `ids`, at most
    /// `max`: an adequacy report.
    fn adequacy(&self, _run: &Run, _ids: &[String], _max: usize) -> Result<Value, Unmeasured> {
        Err(self.unsupported("mutants"))
    }

    /// The escape hatches in the lines a change added to the product file
    /// `path`.
    fn signals(&self, _path: &str, _added: &[(u32, String)]) -> Result<Vec<Signal>, Unmeasured> {
        Err(self.unsupported("escape hatches"))
    }

    /// Whether `path` is product code the escape-hatch search reads.
    fn is_product_file(&self, _path: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A driver that measures only what it must: every stage it leaves
    /// alone says so.
    struct Bare;

    impl Driver for Bare {
        fn language(&self) -> &'static str {
            "bare"
        }
        fn runner(&self) -> &'static str {
            "bare test"
        }
        fn producer(&self) -> String {
            "p".to_owned()
        }
        fn regression(&self, _: &[String]) -> Option<RegressionPlan> {
            None
        }
        fn run_env(&self, _: &[String]) -> Vec<(String, String)> {
            Vec::new()
        }
        fn install_pins(&self, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn is_runner_config(&self, _: &str) -> bool {
            false
        }
        fn runner_config_globs(&self) -> Vec<String> {
            Vec::new()
        }
        fn summary(&self, _: &str) -> Option<Summary> {
            None
        }
        fn failures(&self, _: &str) -> Vec<String> {
            Vec::new()
        }
    }

    fn run() -> Run<'static> {
        Run {
            repo: Path::new("."),
            view: Path::new("."),
            baseline: "b",
            candidate: "c",
            command: &[],
            test_globs: &[],
            source: ".",
            link: &[],
            pins: Vec::new(),
            roots: &[],
            cache: None,
        }
    }

    #[test]
    fn a_stage_a_driver_leaves_alone_is_unsupported_never_empty() {
        let r = run();
        let unsupported = |e: Unmeasured| matches!(e, Unmeasured::Unsupported(why) if why.starts_with("the bare driver does not measure"));
        assert!(Bare.change(&r).is_err_and(unsupported));
        assert!(Bare.passing(&r, &[]).is_err_and(unsupported));
        assert!(
            Bare.preserve(&r, &PreserveAsk::default())
                .is_err_and(unsupported)
        );
        assert!(Bare.adequacy(&r, &[], 1).is_err_and(unsupported));
        assert!(Bare.signals("x", &[]).is_err_and(unsupported));
        let files = LawFiles {
            dir: ".emi/laws",
            approved: &BTreeSet::new(),
            proposed: &BTreeSet::new(),
            max_stand_in_runs: 0,
        };
        let laws = Bare.laws(&r, &files);
        assert!(
            laws.fault
                .is_some_and(|f| f.contains("does not measure laws"))
        );
    }

    #[test]
    fn ids_name_their_file_and_caches_their_producer() {
        assert_eq!(file_of("tests/test_a.py::test_x[1]"), "tests/test_a.py");
        assert_eq!(file_of("tests/adds.rs::adds_two"), "tests/adds.rs");
        let c = ListingCache::new(Path::new("/s"), "env", "p1");
        assert_eq!(c.dir, Path::new("/s/p1"));
        let s = Summary {
            passed: 2,
            failed: 1,
            skipped: 4,
            ..Summary::default()
        };
        assert_eq!(s.ran(), 3);
    }
}
