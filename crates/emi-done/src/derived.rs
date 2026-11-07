//! The derived contract: a task contract built from `.emi/config.json`
//! because nobody wrote one. It says one thing: the tests the baseline had
//! must still pass on the candidate, with the baseline's tests and
//! test-runner configuration restored to their baseline versions (except
//! files a person approved changing), run sandboxed without network.
//! Everything else (restoring, comparing the inventory of tests, the
//! sandbox) is the engine's ([`crate::engine`]).

use std::collections::BTreeSet;
use std::path::Path;

use crate::engine::{Store, contract, verify};
use emi_driver::Driver;
use emi_kernel::json::digest;
use serde_json::{Value, json};

use crate::Error;
use crate::config::Config;

/// The check that runs the tests.
const CHECK: &str = "tests";
/// The check that the tests import the candidate's own code.
const IMPORTS: &str = "imports";
/// The project's files at `baseline`, from which a driver names the
/// project's own code (the same baseline always gives the same contract).
fn baseline_files(view: &Path, baseline: &str) -> Result<Vec<String>, Error> {
    let raw = emi_git::run(view, &["ls-tree", "-r", "-z", "--name-only", baseline])
        .map_err(|e| Error::Usage(format!("cannot list the files of the baseline: {e}")))?;
    Ok(raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// The command that checks the tests import the candidate's own code, when
/// the driver has one for this project.
pub fn import_check(
    run: &emi_driver::Run,
    driver: &dyn Driver,
) -> Result<Option<Vec<String>>, Error> {
    let files = baseline_files(run.view, run.baseline)?;
    Ok(driver.import_check(run, &files))
}

/// Generous bounds: a check that ends at the timeout is `incomplete`, not
/// a failure, so these only guard against a hung runner.
const TEST_TIMEOUT_S: u64 = 1800;
const INVENTORY_TIMEOUT_S: u64 = 600;

/// The globs of the files restored from the baseline for every run: the
/// tests, and the laws (should the test command collect them).
pub fn trusted_globs(config: &Config) -> Vec<String> {
    let mut globs = config.tests.globs.clone();
    globs.push(format!("{}/*", config.laws.dir));
    globs
}

/// A glob that matches exactly `path`: the controller's trusted set is a
/// list of globs, and a file name may contain glob characters.
fn literal(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            '*' | '?' => format!("[{c}]"),
            '[' => "[[]".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

/// The baseline's files the run restores: its test files and test-runner
/// configuration, except `approved` ones, which run as the candidate left
/// them. Listing them one by one is what lets an approval release a single
/// file; the list is sorted, so the same inputs name the same contract.
pub fn trusted(
    (view, baseline): (&Path, &str),
    globs: &[String],
    approved: &BTreeSet<String>,
    driver: &dyn Driver,
) -> Result<Vec<String>, Error> {
    let raw = emi_git::run(view, &["ls-tree", "-r", "-z", "--name-only", baseline])
        .map_err(|e| Error::Usage(format!("cannot list the files of the baseline: {e}")))?;
    let mut files: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !approved.contains(p))
        .filter(|p| {
            driver.is_runner_config(p) || globs.iter().any(|g| emi_kernel::glob::fnmatch(p, g))
        })
        .map(|p| literal(&p))
        .collect();
    files.sort();
    Ok(files)
}

/// The task spec for `config` at `baseline`, trusting the files `trusted`
/// lists. There are no budgets: repair rounds are counted by the hook, and
/// a repeated check must not run out. What a weakened test would do is
/// covered by restoring the originals. Test-runner configuration a candidate adds
/// cannot be restored, so it is a review item, which the receipt turns
/// into a question for a person.
pub fn spec(
    repo: &Path,
    config: &Config,
    baseline: &str,
    (trusted, imports): (&[String], Option<&[String]>),
    (environment, driver): (Option<&str>, &dyn Driver),
) -> Value {
    let mut command = config.tests.command.clone();
    let mut verification = json!({"trusted": trusted, "runner_config": "review"});
    let mut evidence = Value::Null;
    if let Some(plan) = driver.regression(&config.tests.command) {
        // one outcome line per test, so this run can be compared test by
        // test with the baseline's: a test the baseline already skipped is
        // not a regression, one that stopped running is
        command = plan.run;
        verification["inventory"] = json!({"command": plan.list, "format": plan.format, "timeout_s": INVENTORY_TIMEOUT_S,
                   "collection_errors": "tolerate"});
        // The test check runs the same invocation, so its own report is
        // required too: a passing exit with no readable summary (an early
        // exit) or with skips is not regression evidence.
        evidence = json!({"format": plan.format});
    }
    // the pins, and the import path: relative, so each run resolves it in
    // its own copy of the code (the project's roots, then the pins plugin)
    let mut env: serde_json::Map<String, Value> = config
        .pins
        .env()
        .into_iter()
        .map(|(k, v)| (k, json!(v)))
        .collect();
    for (key, value) in driver.run_env(&config.python_path) {
        env.insert(key, json!(value));
    }
    // which installed code the tests run against: another environment is
    // another task, so the baseline's recorded runs are not reused for it
    if let Some(id) = environment {
        env.insert("EMI_ENVIRONMENT".to_owned(), json!(id));
    }
    let execution = json!({"isolation": "best", "network": false, "link": config.link, "env": env});
    let mut spec = json!({
        "task_id": "",
        "repo": repo,
        "baseline": baseline,
        "objective": "The baseline's tests still pass on the candidate.",
        "requirements": [{"id": "R1", "acceptance": "every test the baseline had still passes, with the trusted tests restored", "checks": [CHECK]}],
        "checks": {CHECK: {"command": command, "success": {"exit_code": 0}, "timeout_s": TEST_TIMEOUT_S, "evidence": evidence}},
        "verification": verification,
        "execution": execution,
    });
    // the tests prove something about the candidate only if they import it
    if let Some(argv) = imports {
        spec["checks"][IMPORTS] =
            json!({"command": argv, "success": {"exit_code": 0}, "timeout_s": 120});
        if let Some(list) = spec["requirements"].as_array_mut() {
            list.push(json!({"id": "R2", "acceptance": "the tests import the candidate's own code, not an installed copy", "checks": [IMPORTS]}));
        }
    }
    spec
}

/// The name of the task for `spec`: the same repository, baseline,
/// configuration and trusted files reuse one task, and any change of them
/// starts another.
pub fn task_id(spec: &Value) -> String {
    let id = digest("done-task", spec);
    format!("done-{}", &id[..24])
}

/// The task's store under `authority`, frozen on first use. The linked
/// dependency directories are pinned then, and again by [`run`] when they
/// change between checks.
pub fn open(
    (authority, repo): (&Path, &Path),
    baseline: &str,
    spec: &Value,
    driver: &'static dyn Driver,
) -> Result<Store, Error> {
    let task = task_id(spec);
    let store = Store::open(authority, &task, repo)
        .map_err(Error::Usage)?
        .reading(driver);
    if !store.exists() {
        let mut named = spec.clone();
        named["task_id"] = json!(task);
        let frozen = contract::freeze(&named, &repo.to_string_lossy(), baseline)
            .map_err(|e| Error::Config(format!("cannot derive a task contract: {e}")))?;
        // a concurrent run may have created it first, which is fine
        if let Err(e) = store.create(&frozen.contract)
            && !store.exists()
        {
            return Err(Error::Usage(e));
        }
    }
    Ok(store)
}

/// The decision and the recorded run of the test check for one candidate.
pub struct Verified {
    pub decision: Value,
    pub check: Option<Value>,
    /// Why the tests cannot be taken to exercise the candidate's code,
    /// when the import check says so or could not say.
    pub foreign_imports: Option<String>,
}

/// What the import check found, when the contract has one: `None` when it
/// passed or is not part of the contract.
fn foreign_imports(store: &Store, decision: &Value) -> Result<Option<String>, String> {
    if store.contract()?["checks"][IMPORTS].is_null() {
        return Ok(None);
    }
    let run = recorded_check(store, decision, IMPORTS)?;
    Ok(match run.as_ref().map(|r| (r["status"].as_str(), r)) {
        Some((Some("passed"), _)) => None,
        Some((Some("failed"), r)) if r["exit_code"] == 3 => Some(
            r["stdout_tail"]
                .as_str()
                .unwrap_or("the tests would not import the code being checked")
                .trim()
                .to_owned(),
        ),
        _ => Some(
            "whether the tests import the code being checked could not be established".to_owned(),
        ),
    })
}

/// How many times the baseline's tests run under the same pins: a test
/// whose outcome differs between them is not regression evidence.
pub const BASELINE_RUNS: usize = 2;

/// The recorded runs of the test check on the baseline itself, under the
/// task's current contract: what the candidate's run is compared with. The
/// baseline's outcomes do not change, so the runs serve every later check
/// of the same task; a contract revision (new dependencies) runs them again.
pub fn baseline_checks(store: &Store, baseline: &str) -> Result<Vec<Value>, String> {
    let contract = store.contract()?;
    let repo = Path::new(contract["repo"].as_str().unwrap_or(""));
    let view = emi_git::CleanView::of(repo)?;
    baseline_checks_in(store, view.path(), baseline)
}

/// [`baseline_checks`] reading from the caller's clean object view.
pub fn baseline_checks_in(
    store: &Store,
    view: &Path,
    baseline: &str,
) -> Result<Vec<Value>, String> {
    let contract = store.contract()?;
    let id = contract["contract_id"].as_str().unwrap_or("");
    // a run recorded by an older reader names its tests differently
    let kept = |store: &Store| -> Result<Vec<Value>, String> {
        Ok(store
            .rows("check_run")?
            .into_iter()
            .filter(|r| {
                r["check_id"] == CHECK
                    && r["candidate_id"] == baseline
                    && r["contract_id"] == id
                    && r["evidence"]["reader"] == crate::engine::evidence::READER
            })
            .collect())
    };
    let mut runs = kept(store)?;
    while runs.len() < BASELINE_RUNS {
        let decision = verify::verify_in(store, view, baseline, baseline)?;
        if recorded_check(store, &decision, CHECK)?.is_none() {
            // the check did not run at all: running it again says no more
            return Ok(runs);
        }
        runs = kept(store)?;
    }
    runs.truncate(BASELINE_RUNS);
    Ok(runs)
}

/// [`baseline_checks`], one run at a time for a repository: a check that
/// arrives while another process is running the baseline (the hook warms
/// it when the person sends a message) waits for that run and reads its
/// result instead of running the suite a second time beside it.
pub fn baseline_checks_once(
    store: &Store,
    baseline: &str,
    authority: &Path,
) -> Result<Vec<Value>, String> {
    let path = authority.join("baseline.lock");
    let lock = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    lock.lock()
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    baseline_checks(store, baseline)
}

/// [`baseline_checks_once`] reading from the caller's clean object view.
pub fn baseline_checks_once_in(
    store: &Store,
    view: &Path,
    baseline: &str,
    authority: &Path,
) -> Result<Vec<Value>, String> {
    let path = authority.join("baseline.lock");
    let lock = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    lock.lock()
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    baseline_checks_in(store, view, baseline)
}

fn recorded_check(store: &Store, decision: &Value, id: &str) -> Result<Option<Value>, String> {
    Ok(store.rows("check_run")?.into_iter().rev().find(|r| {
        r["check_id"] == id
            && r["candidate_id"] == decision["candidate_id"]
            && r["contract_id"] == decision["contract_id"]
    }))
}

/// Verify `candidate` (already read into `view`) under the task.
/// With `repin`, linked dependencies that changed since the task was
/// frozen are pinned as they are now and the check runs once more: on a
/// person's machine the environment changes between checks, and a fresh
/// pin still catches a change during a run.
pub fn run(
    store: &Store,
    view: &Path,
    candidate: &crate::candidate::Candidate,
    repin: bool,
) -> Result<Verified, String> {
    let mut decision = verify::verify_in(store, view, &candidate.id, &candidate.rev)?;
    if repin && decision["gates"]["dependencies"]["state"] == "unverified" {
        let execution = store.contract()?["execution"].clone();
        store.revise(
            &json!({"execution": execution}),
            "emi done",
            "linked dependencies changed between checks",
        )?;
        decision = verify::verify_in(store, view, &candidate.id, &candidate.rev)?;
    }
    let check = recorded_check(store, &decision, CHECK)?;
    let foreign_imports = foreign_imports(store, &decision)?;
    Ok(Verified {
        decision,
        check,
        foreign_imports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OnUnproven, Tests};
    use crate::testutil::{commit_file, git_out, repo};

    fn config() -> Config {
        Config {
            language: "python".into(),
            source: "src".into(),
            default_branch: "main".into(),
            tests: Tests {
                command: vec![
                    ".venv/bin/python".into(),
                    "-m".into(),
                    "pytest".into(),
                    "-q".into(),
                ],
                globs: vec!["tests/*".into()],
            },
            link: vec![".venv".into()],
            python_path: Vec::new(),
            repair_rounds: 3,
            on_unproven: OnUnproven::Report,
            on_redundant: OnUnproven::Report,
            pins: Default::default(),
            laws: Default::default(),
            preserve: Default::default(),
            adequacy: Default::default(),
        }
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn names(v: &Value) -> Vec<&str> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    }

    #[test]
    fn the_spec_trusts_listed_files_and_validates_as_a_contract() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let trusted = vec!["tests/test_a.py".to_owned()];
        let spec = spec(
            dir.path(),
            &config(),
            SHA,
            (&trusted, None),
            (None, &emi_python::driver::Python),
        );
        assert_eq!(names(&spec["verification"]["trusted"]), ["tests/test_a.py"]);
        assert_eq!(spec["verification"]["runner_config"], "review");
        let inventory = names(&spec["verification"]["inventory"]["command"]);
        assert_eq!((inventory[0], inventory[1]), (".venv/bin/python", "-c"));
        assert!(
            inventory[2].contains("EMI-ITEM"),
            "the reporter lists the tests"
        );
        assert_eq!(
            inventory[3..],
            [
                "--collect-only",
                "-q",
                "-p",
                "emi_pins",
                "-p",
                "no:randomly",
                "-p",
                "no:cacheprovider"
            ]
        );
        let command = names(&spec["checks"]["tests"]["command"]);
        assert_eq!((command[0], command[1]), (".venv/bin/python", "-c"));
        assert!(
            command[2].contains("EMI-OUTCOME"),
            "the reporter runs pytest"
        );
        assert_eq!(
            command[3..],
            [
                "-q",
                "--tb=no",
                "--continue-on-collection-errors",
                "-p",
                "emi_pins",
                "-p",
                "no:randomly",
                "-p",
                "no:cacheprovider"
            ]
        );
        assert!(
            spec.get("budgets").is_none(),
            "repair rounds are the hook's, not a budget"
        );
        assert_eq!(spec["execution"]["network"], false);
        let mut named = spec.clone();
        named["task_id"] = json!(task_id(&spec));
        assert!(contract::freeze(&named, &dir.path().to_string_lossy(), SHA).is_ok());
    }

    #[test]
    fn own_code_goes_first_on_the_import_path_and_its_origin_is_checked() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut c = config();
        c.python_path = vec!["src".into()];
        let files = vec!["src/app/__init__.py".to_owned()];
        let run = crate::run_of((dir.path(), dir.path()), &c, (SHA, SHA), None);
        let imports = emi_driver::Driver::import_check(&emi_python::driver::Python, &run, &files);
        let spec = spec(
            dir.path(),
            &c,
            SHA,
            (&[], imports.as_deref()),
            (None, &emi_python::driver::Python),
        );
        assert_eq!(spec["execution"]["env"]["PYTHONPATH"], "src:.emi-pins");
        assert_eq!(spec["execution"]["env"]["PYTHONHASHSEED"], "0");
        let argv = names(&spec["checks"]["imports"]["command"]);
        assert_eq!(
            (argv[0], argv[1], argv[3]),
            (".venv/bin/python", "-c", "app")
        );
        assert_eq!(spec["requirements"][1]["checks"], json!(["imports"]));
        let mut named = spec.clone();
        named["task_id"] = json!(task_id(&spec));
        assert!(contract::freeze(&named, &dir.path().to_string_lossy(), SHA).is_ok());
        // without a src layout or known names: the pins alone, no import check
        let plain = super::spec(
            dir.path(),
            &config(),
            SHA,
            (&[], None),
            (None, &emi_python::driver::Python),
        );
        assert_eq!(plain["execution"]["env"]["PYTHONPATH"], ".emi-pins");
        assert!(plain["checks"].get("imports").is_none());
    }

    #[test]
    fn a_command_that_is_not_pytest_has_no_inventory() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut c = config();
        c.tests.command = vec!["python3".into(), "-m".into(), "unittest".into()];
        assert!(
            spec(
                dir.path(),
                &c,
                SHA,
                (&[], None),
                (None, &emi_python::driver::Python)
            )["verification"]
                .get("inventory")
                .is_none()
        );
        assert!(
            emi_driver::Driver::regression(&emi_python::driver::Python, &c.tests.command).is_none()
        );
    }

    #[test]
    fn the_same_inputs_name_the_same_task_and_any_change_names_another() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let id = |c: &Config, sha: &str, trusted: &[String]| {
            task_id(&spec(
                dir.path(),
                c,
                sha,
                (trusted, None),
                (None, &emi_python::driver::Python),
            ))
        };
        let files = vec!["tests/a.py".to_owned()];
        let base = id(&config(), SHA, &files);
        assert!(contract::valid_task_id(&base));
        assert_eq!(base, id(&config(), SHA, &files));
        let mut other = config();
        other.tests.command.push("-x".into());
        assert_ne!(base, id(&other, SHA, &files));
        assert_ne!(
            base,
            id(
                &config(),
                "fedcba9876543210fedcba9876543210fedcba98",
                &files
            )
        );
        assert_ne!(
            base,
            id(&config(), SHA, &[]),
            "approving a file changes what is trusted"
        );
    }

    #[test]
    fn the_trusted_list_is_the_baselines_tests_and_runner_files_minus_approved_ones() {
        let dir = repo();
        for name in [
            "tests/test_a.py",
            "tests/test_[b].py",
            "src/app.py",
            "pyproject.toml",
            "tests/data.json",
        ] {
            commit_file(dir.path(), name, "x\n", name);
        }
        let head = git_out(dir.path(), &["rev-parse", "HEAD"]);
        let view = emi_git::CleanView::of(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        let globs = vec!["tests/*".to_owned()];
        let all = trusted(
            (view.path(), &head),
            &globs,
            &BTreeSet::new(),
            &emi_python::driver::Python,
        )
        .unwrap_or_default();
        assert_eq!(
            all,
            [
                "pyproject.toml",
                "tests/data.json",
                "tests/test_[[]b].py",
                "tests/test_a.py"
            ]
        );
        for (path, glob) in [("tests/test_[b].py", &all[2]), ("tests/test_b.py", &all[2])] {
            let hit = emi_kernel::glob::fnmatch(path, glob);
            assert_eq!(hit, path == "tests/test_[b].py", "{path} against {glob}");
        }
        let approved: BTreeSet<String> = ["tests/test_a.py".to_owned()].into();
        let less = trusted(
            (view.path(), &head),
            &globs,
            &approved,
            &emi_python::driver::Python,
        )
        .unwrap_or_default();
        assert!(!less.contains(&"tests/test_a.py".to_owned()) && less.len() == 3);
    }
}
