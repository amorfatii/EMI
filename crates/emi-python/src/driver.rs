//! Python behind EMI's language boundary (ADR 0009): pytest runs the
//! tests, coverage.py and Hypothesis gather the evidence, tree-sitter
//! reads the code. What the stages need from Python, and nothing about
//! how the orchestrator combines them.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use emi_driver::{
    Driver, LawEvidence, LawFiles, PreserveAsk, RegressionPlan, Run, Signal, Summary, Unmeasured,
};
use emi_kernel::pins::Pins;
use serde_json::Value;

use crate::drift::{self, InView};
use crate::proving::pytest::Command;
use crate::proving::{Candidate, Request};
use crate::sandbox::Runner;

mod environment;
mod imports;
mod signals;

/// The Python driver.
pub struct Python;

/// Files that decide how tests run rather than what they test: a
/// `conftest.py` can rewrite every test's outcome, a `pytest.ini` can load
/// a plugin or deselect tests, `sitecustomize.py` runs at interpreter
/// start (fnmatch patterns on the last path component). The JavaScript
/// runners' files are kept from before languages were drivers.
const RUNNER_CONFIG: &[&str] = &[
    "conftest.py",
    "pytest.ini",
    "pyproject.toml",
    "setup.cfg",
    "tox.ini",
    "noxfile.py",
    "sitecustomize.py",
    "usercustomize.py",
    "package.json",
    "jest.config.*",
    "vitest.config.*",
    "vite.config.*",
    ".mocharc*",
    "karma.conf.*",
];

/// The proving request for `run`: the frozen candidate, the configured
/// command, and the same pins and import path as the regression run, so
/// that every stage runs the candidate's code.
fn request(run: &Run) -> Request {
    let mut req = Request::new(run.repo, run.baseline, run.command.to_vec());
    req.candidate = Candidate::Ref(run.candidate.to_owned());
    req.test_globs = run.test_globs.to_vec();
    run.source.clone_into(&mut req.source);
    req.link = run.link.to_vec();
    req.env.extend(run.pins.iter().cloned());
    req.env.extend(Python.run_env(run.roots));
    req.listing_cache.clone_from(&run.cache);
    req
}

/// The environment of a run of the project's code: the pins, then the
/// import path.
fn env_of(run: &Run) -> Vec<(String, String)> {
    let mut env = run.pins.clone();
    env.extend(Python.run_env(run.roots));
    env
}

impl Driver for Python {
    fn language(&self) -> &'static str {
        "python"
    }

    fn runner(&self) -> &'static str {
        "pytest"
    }

    fn exit_says(&self, code: i64) -> Option<String> {
        match code {
            5 => Some("pytest found no tests to run".to_owned()),
            4 => Some("pytest did not understand its command line".to_owned()),
            3 => Some("pytest failed internally".to_owned()),
            _ => None,
        }
    }

    fn producer(&self) -> String {
        crate::producer::producer()
    }

    fn regression(&self, command: &[String]) -> Option<RegressionPlan> {
        let pytest = Command::parse(command).ok()?;
        Some(RegressionPlan {
            run: pytest.report_argv(),
            list: pytest.list_argv(),
            format: "pytest",
        })
    }

    fn environment(&self, repo: &Path, command: &[String]) -> Option<Result<String, String>> {
        let pytest = Command::parse(command).ok()?;
        Some(environment::id(repo, pytest.interpreter()))
    }

    fn run_env(&self, roots: &[String]) -> Vec<(String, String)> {
        vec![("PYTHONPATH".to_owned(), Pins::python_path(roots))]
    }

    fn install_pins(&self, dir: &Path) -> Result<(), String> {
        crate::pins::install(dir)
    }

    fn is_runner_config(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        RUNNER_CONFIG
            .iter()
            .any(|n| emi_kernel::glob::fnmatch(name, n))
    }

    fn runner_config_globs(&self) -> Vec<String> {
        RUNNER_CONFIG
            .iter()
            .flat_map(|n| [(*n).to_owned(), format!("*/{n}")])
            .collect()
    }

    fn import_check(&self, run: &Run, files: &[String]) -> Option<Vec<String>> {
        imports::check(run, files)
    }

    fn summary(&self, output: &str) -> Option<Summary> {
        crate::report::summary(output).map(|s| Summary {
            passed: s.passed,
            failed: s.failed,
            errors: s.errors,
            skipped: s.skipped,
            xfailed: s.xfailed,
            xpassed: s.xpassed,
        })
    }

    fn failures(&self, output: &str) -> Vec<String> {
        crate::report::failures(output)
    }

    /// `ERROR tests/test_a.py - ImportError: ...`: none of its tests ran.
    /// A test's own error line names an id with `::` and is read from its
    /// outcome line instead.
    fn uncollected_file<'l>(&self, line: &'l str) -> Option<&'l str> {
        let file = crate::report::id_of(line.strip_prefix("ERROR ")?);
        (!file.contains("::") && !file.contains(char::is_whitespace) && file.ends_with(".py"))
            .then_some(file)
    }

    fn change(&self, run: &Run) -> Result<Value, Unmeasured> {
        crate::proving::measure_in(&request(run), run.view)
            .map(|evidence| evidence.to_json())
            .map_err(Unmeasured::Failed)
    }

    fn passing(&self, run: &Run, ids: &[String]) -> Result<BTreeSet<String>, Unmeasured> {
        crate::proving::passing(&request(run), run.view, ids).map_err(Unmeasured::Failed)
    }

    fn laws(&self, run: &Run, files: &LawFiles) -> LawEvidence {
        crate::proving::laws::measure_laws(&request(run), run.view, files)
    }

    fn preserve(&self, run: &Run, ask: &PreserveAsk) -> Result<Value, Unmeasured> {
        let env = env_of(run);
        let side = |rev| InView {
            runner: Runner {
                repo: run.repo,
                objects: Some(run.view),
                rev,
                link: run.link,
                env: &env,
                timeout: Duration::from_secs(1800),
                network: false,
            },
            objects: run.view,
        };
        let (base, head) = (side(run.baseline), side(run.candidate));
        let commands = [run.command.to_vec()];
        drift::measure(&drift::Request {
            repo: run.view,
            base: run.baseline.to_owned(),
            head: run.candidate.to_owned(),
            commands: &commands,
            call_timeout_s: ask.call_timeout_s,
            base_side: &base,
            head_side: &head,
            generated_examples: ask.generated_examples,
            module_roots: run.roots,
            paths: ask.paths,
            cache: run.cache.as_ref(),
        })
        .map_err(Unmeasured::Failed)
    }

    fn adequacy(&self, run: &Run, ids: &[String], max: usize) -> Result<Value, Unmeasured> {
        let command = Command::parse(run.command).map_err(Unmeasured::Failed)?;
        let argv = command.run_argv(ids, ".emi-proving/adequacy.xml");
        let env = env_of(run);
        let plan = crate::mutation::Plan {
            runner: Runner {
                repo: run.repo,
                objects: Some(run.view),
                rev: run.candidate,
                link: run.link,
                env: &env,
                timeout: Duration::from_secs(600),
                network: false,
            },
            command: &argv,
        };
        let changed = crate::functions::changed_functions(run.view, run.baseline, run.candidate)
            .map_err(Unmeasured::Failed)?;
        crate::diffmut::diff_mutation(&plan, run.baseline, &changed, max)
            .map(|result| result.to_json())
            .map_err(Unmeasured::Failed)
    }

    fn signals(&self, _path: &str, added: &[(u32, String)]) -> Result<Vec<Signal>, Unmeasured> {
        Ok(signals::find(added))
    }

    fn is_product_file(&self, path: &str) -> bool {
        path.ends_with(".py")
    }
}
