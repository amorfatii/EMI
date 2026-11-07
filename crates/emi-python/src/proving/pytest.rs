//! The test command as pytest argv: how to list tests, run chosen ones and
//! run them under coverage, and how to read the list.
//!
//! The configured command is an invocation of pytest with options. Its path
//! selectors (`tests/`, `tests/test_a.py::test_x`) are kept for listing, so
//! the list is what the command would run, and dropped for running chosen
//! ids, which must not drag the rest of the suite along.

use std::collections::{BTreeMap, BTreeSet};

use emi_kernel::pins::Pins;

/// Options that take their value as the next argument.
const VALUE_OPTIONS: &[&str] = &[
    "-k",
    "-m",
    "-p",
    "-c",
    "-o",
    "-W",
    "-n",
    "-r",
    "--maxfail",
    "--tb",
    "--ignore",
    "--ignore-glob",
    "--deselect",
    "--rootdir",
    "--confcutdir",
    "--import-mode",
    "--durations",
    "--durations-min",
    "--basetemp",
    "--junitxml",
    "--junit-xml",
    "--numprocesses",
    "--dist",
    "--cov",
    "--cov-report",
    "--cov-config",
    "--log-level",
    "--override-ini",
    "--capture",
    "--color",
    "--assert",
    "--timeout",
];

/// Flags that change what the listing prints, or stop a run at the first
/// failure and so hide the other tests' outcomes.
const VERBOSITY: &[&str] = &["-q", "-qq", "-v", "-vv", "--quiet", "--verbose"];
const STOP_EARLY: &[&str] = &["-x", "--exitfirst"];

/// A recognised pytest invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// What starts pytest: `python -m pytest`, or the `pytest` script.
    launcher: Vec<String>,
    /// Python that can run `-m coverage` next to the same packages.
    interpreter: String,
    options: Vec<String>,
    selectors: Vec<String>,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The Python beside a `pytest` script (the same virtualenv), else
/// `python3`.
fn sibling_python(script: &str) -> String {
    match script.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/python"),
        None => "python3".to_owned(),
    }
}

/// Split the arguments after the launcher into options and selectors.
fn split(rest: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut options, mut selectors) = (Vec::new(), Vec::new());
    let mut takes_value = false;
    for arg in rest {
        if takes_value {
            options.push(arg.clone());
            takes_value = false;
        } else if arg.starts_with('-') {
            takes_value = VALUE_OPTIONS.contains(&arg.as_str());
            options.push(arg.clone());
        } else {
            selectors.push(arg.clone());
        }
    }
    (options, selectors)
}

impl Command {
    /// Recognise `python -m pytest ...`, `pytest ...` or `py.test ...`; any
    /// other command (a wrapper, a Makefile target) is refused with why,
    /// because its per-test results cannot be read.
    pub fn parse(argv: &[String]) -> Result<Command, String> {
        let first = argv.first().ok_or("the test command is empty")?;
        let name = basename(first);
        let (launcher, interpreter, rest) = if name.starts_with("python") {
            if argv.get(1).map(String::as_str) != Some("-m")
                || argv.get(2).map(String::as_str) != Some("pytest")
            {
                return Err(not_pytest(first));
            }
            (argv[..3].to_vec(), first.clone(), &argv[3..])
        } else if name == "pytest" || name == "py.test" {
            (vec![first.clone()], sibling_python(first), &argv[1..])
        } else {
            return Err(not_pytest(first));
        };
        let (options, selectors) = split(rest);
        Ok(Command {
            launcher,
            interpreter,
            options,
            selectors,
        })
    }

    /// The same command collecting from `selectors` instead of its own:
    /// the laws are run with the project's options from their own
    /// directory.
    pub fn with_selectors(&self, selectors: Vec<String>) -> Command {
        Command {
            selectors,
            ..self.clone()
        }
    }

    fn without(&self, drop: &[&[&str]]) -> Vec<String> {
        let mut out = Vec::new();
        let mut skip_value = false;
        for opt in &self.options {
            if skip_value {
                skip_value = false;
                continue;
            }
            let name = opt.split('=').next().unwrap_or(opt);
            if drop.iter().any(|set| set.contains(&name)) {
                skip_value = VALUE_OPTIONS.contains(&name) && !opt.contains('=');
                continue;
            }
            out.push(opt.clone());
        }
        out
    }

    /// The Python that runs the tests, for a command that must see the
    /// same import path they do.
    pub fn interpreter(&self) -> &str {
        &self.interpreter
    }

    /// List the node ids the configured command would run, exactly (see
    /// `emi_report.py`): the project's own options cannot change the form.
    pub fn list_argv(&self) -> Vec<String> {
        let mut argv = self.reporter();
        argv.extend(self.without(&[VERBOSITY]));
        argv.extend(self.selectors.clone());
        argv.extend(["--collect-only", "-q"].map(str::to_owned));
        argv.extend(Pins::pytest_args());
        argv
    }

    /// The tests' own Python running the reporter in place of `-m pytest`;
    /// the import path is the same (the current directory first).
    fn reporter(&self) -> Vec<String> {
        vec![self.interpreter.clone(), "-c".to_owned(), REPORT.to_owned()]
    }

    /// Run what the configured command runs, ending with one exact outcome
    /// line per test (see `emi_report.py`) and no tracebacks: the form a
    /// reader can compare test by test with another run. It starts the
    /// tests' own Python with the reporter in place of `-m pytest`; the
    /// import path is the same (the current directory first).
    pub fn report_argv(&self) -> Vec<String> {
        let mut argv = self.reporter();
        argv.extend(self.without(&[VERBOSITY, &["--tb"]]));
        argv.extend(self.selectors.clone());
        // a file that cannot be imported must not keep the others from
        // running: its error is one outcome among the rest
        argv.extend(["-q", "--tb=no", "--continue-on-collection-errors"].map(str::to_owned));
        argv.extend(Pins::pytest_args());
        argv
    }

    /// Everything before the ids of a run of chosen tests.
    fn run_prefix(&self, junit: &str) -> Vec<String> {
        let mut argv: Vec<String> = Vec::new();
        argv.extend(self.without(&[STOP_EARLY, &["--maxfail", "--tb"]]));
        // a file that cannot be imported must not stop the other files
        argv.push("--continue-on-collection-errors".to_owned());
        // Python's own traceback, which folds a frame repeated by deep
        // recursion into one line: pytest's styles render every frame, and
        // a failure 50,000 frames deep took 48 s to report instead of 2. The
        // assertion explanation and Hypothesis's example are in it alike.
        argv.push("--tb=native".to_owned());
        argv.extend(Pins::pytest_args());
        argv.push(format!("--junitxml={junit}"));
        argv
    }

    /// Run exactly `ids`, writing the JUnit report to `junit`.
    pub fn run_argv(&self, ids: &[String], junit: &str) -> Vec<String> {
        let mut argv = self.launcher.clone();
        argv.extend(self.run_prefix(junit));
        argv.extend(ids.iter().cloned());
        argv
    }

    /// Run exactly `ids` under coverage.py, measuring `sources`, then write
    /// the JSON report to `output`. Two commands, run in order.
    pub fn coverage_argv(&self, ids: &[String], rcfile: &str, output: &str) -> Vec<Vec<String>> {
        let mut run = vec![
            self.interpreter.clone(),
            "-c".into(),
            COVERAGE_RUN.into(),
            rcfile.into(),
            output.into(),
        ];
        run.extend(["-p".into(), "emi_proving_contexts".into()]);
        run.extend(self.run_prefix(".emi-proving/coverage-run.xml"));
        run.extend(ids.iter().cloned());
        vec![run]
    }
}

/// Pytest under coverage and the JSON report, in one interpreter.
pub(crate) const COVERAGE_RUN: &str = include_str!("emi_coverage_run.py");
/// The reporter that prints one exact outcome line per test.
pub(crate) const REPORT: &str = include_str!("emi_report.py");

fn not_pytest(first: &str) -> String {
    format!(
        "the test command is not a pytest invocation (expected `python -m pytest ...` or `pytest ...`, got `{first}`): per-test results cannot be read"
    )
}

/// A listed node id: a path, `::`, then names, with no space before the
/// first `::` (a warning or summary line has one).
fn is_node_id(line: &str) -> bool {
    line.split_once("::")
        .is_some_and(|(path, _)| !path.is_empty() && !path.contains(char::is_whitespace))
}

/// A file-count line (`tests/test_a.py: 3`): what `-qq` prints instead of
/// node ids, which the listing cannot be read from.
fn is_file_count(line: &str) -> bool {
    line.rsplit_once(": ").is_some_and(|(path, n)| {
        !path.contains(char::is_whitespace)
            && n.bytes().all(|b| b.is_ascii_digit())
            && !n.is_empty()
    })
}

/// The test files a listing or a run could not collect: from its short
/// summary (`ERROR tests/test_a.py - ImportError: ...`), or, for a pytest
/// too old to print one, from the heading of its error report
/// (`____ ERROR collecting tests/test_a.py ____`).
pub fn uncollected_files(stdout: &str) -> BTreeSet<String> {
    stdout
        .lines()
        .filter_map(|l| {
            let line = l.trim().trim_matches('_').trim();
            line.strip_prefix("EMI-BROKEN ")
                .or_else(|| line.strip_prefix("ERROR collecting "))
                .or_else(|| line.strip_prefix("ERROR "))
        })
        .map(|rest| rest.split(" - ").next().unwrap_or(rest).trim())
        .filter(|f| f.ends_with(".py") && !f.contains("::") && !f.contains(char::is_whitespace))
        .map(str::to_owned)
        .collect()
}

/// The exact listing the reporter prints (`EMI-ITEM <id>` lines closed by
/// `EMI-ITEMS <count>`): `None` when the output holds none, `Err` when it
/// is cut short or does not add up.
pub fn reporter_listing(stdout: &str) -> Option<Result<BTreeSet<String>, String>> {
    let mut ids = BTreeSet::new();
    let (mut rows, mut closed) = (0usize, None);
    for line in stdout.lines() {
        if let Some(count) = line.strip_prefix("EMI-ITEMS ") {
            closed = count.trim().parse::<usize>().ok();
        } else if let Some(id) = line.strip_prefix("EMI-ITEM ") {
            rows += 1;
            ids.insert(id.to_owned());
        }
    }
    match (rows, closed) {
        (0, None) => None,
        (n, Some(count)) if n == count => Some(Ok(ids)),
        _ => Some(Err(
            "the listing of tests is cut short or does not add up to its own count".to_owned(),
        )),
    }
}

/// The node ids in the output of `pytest --collect-only -q`.
pub fn parse_listing(stdout: &str) -> Result<BTreeSet<String>, String> {
    if let Some(exact) = reporter_listing(stdout) {
        return exact;
    }
    let ids: BTreeSet<String> = stdout
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .filter(|l| is_node_id(l))
        .map(str::to_owned)
        .collect();
    if ids.is_empty() && stdout.lines().any(is_file_count) {
        return Err(
            "pytest listed file counts, not node ids (a quiet option in its configuration?)".into(),
        );
    }
    Ok(ids)
}

/// The tests the candidate adds: ids it lists that the baseline does not.
/// A test whose body changed keeps its id and is not added; a renamed or
/// newly parametrized one has a new id and is. A test in a file git sees as
/// moved (`renames`: new path to old path) is the same test at a new path,
/// not an added one: it would otherwise look redundant, since it passes at
/// the baseline.
pub fn added_ids(
    candidate: &BTreeSet<String>,
    baseline: &BTreeSet<String>,
    renames: &BTreeMap<String, String>,
) -> Vec<String> {
    let moved = |id: &str| {
        let file = file_of(id);
        renames
            .get(file)
            .is_some_and(|old| baseline.contains(&format!("{old}{}", &id[file.len()..])))
    };
    candidate
        .iter()
        .filter(|id| !baseline.contains(*id) && !moved(id))
        .cloned()
        .collect()
}

/// The file part of a node id.
pub fn file_of(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

/// The test function a case belongs to, as file and function name: its id
/// without the parameters and without the class (`t.py::TestA::test_x[1]`
/// is a case of `test_x` in `t.py`). The class is left out because a test
/// written once in a base class runs under each subclass's name.
pub fn function_of(id: &str) -> (&str, &str) {
    let file = file_of(id);
    let names = &id[file.len()..];
    let names = names.find('[').map_or(names, |at| &names[..at]);
    (file, names.rsplit("::").next().unwrap_or(names))
}

/// Ids (sorted, so a file's ids are together) in batches that each stay in
/// one file and fit one argument list. One file per run because a file that
/// cannot be imported makes pytest abort a run that names it, with the
/// other files' tests not run: a broken file must not hide the others.
pub fn batches(ids: &[String]) -> Vec<&[String]> {
    const BYTES: usize = 100_000;
    let mut out = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (at, id) in ids.iter().enumerate() {
        let new_file = at > start && file_of(id) != file_of(&ids[at - 1]);
        if (new_file || size + id.len() + 1 > BYTES) && at > start {
            out.push(&ids[start..at]);
            (start, size) = (at, 0);
        }
        size += id.len() + 1;
    }
    if start < ids.len() {
        out.push(&ids[start..]);
    }
    out
}

#[cfg(test)]
mod tests;
