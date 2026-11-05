//! The derived contract: a task contract built from `.emi/config.json`
//! because nobody wrote one. It says one thing: the tests the baseline had
//! must still pass on the candidate, with the baseline's tests and
//! test-runner configuration restored to their baseline versions (except
//! files a person approved changing), run sandboxed without network.
//! Everything else (restoring, comparing the inventory of tests, the
//! sandbox) is the engine's ([`crate::engine`]).

use std::collections::BTreeSet;
use std::path::Path;

use crate::engine::{Candidate, Store, contract, runner_config, verify};
use emi_kernel::json::digest;
use serde_json::{Value, json};

use crate::Error;
use crate::config::Config;

/// The check that runs the tests.
const CHECK: &str = "tests";
/// The check that the tests import the candidate's own code.
const IMPORTS: &str = "imports";
/// Most names the import check looks up.
const MAX_OWN_NAMES: usize = 30;

/// Where each of the project's own top-level names would be imported from,
/// seen by the Python that runs the tests, in the directory the tests run
/// in. A name found outside that directory is an installed copy (an
/// editable install of another checkout, a package in site-packages): the
/// tests would then exercise that copy, not the candidate. A name that
/// cannot be found is left to the tests themselves to report.
const IMPORT_ORIGINS: &str = r#"
import importlib.util, os, sys
here = os.path.realpath(os.getcwd()) + os.sep
foreign = []
for name in sys.argv[1:]:
    if name in getattr(sys, "stdlib_module_names", ()):
        continue
    try:
        spec = importlib.util.find_spec(name)
    except Exception:
        spec = None
    if spec is None:
        continue
    origin = spec.origin
    if origin is None and spec.submodule_search_locations:
        origin = list(spec.submodule_search_locations)[0]
    if origin and not (os.path.realpath(origin) + os.sep).startswith(here):
        foreign.append(f"{name} from {origin}")
if foreign:
    print("the tests would not import the code being checked: " + "; ".join(foreign))
    sys.exit(3)
print("own code is imported from the checked copy")
"#;

/// Names that are files of a repository, not code its tests import.
const NOT_OWN: &[&str] = &["setup", "conftest", "noxfile", "tests", "test", "docs"];

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The top-level name `path` gives under import root `root`: a package
/// (`root/pkg/__init__.py`) anywhere, a module (`root/mod.py`) only under
/// a root other than the repository's (a module at the repository root may
/// share its name with anything).
fn own_name<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let rest = if root == "." {
        path
    } else {
        path.strip_prefix(root)?.strip_prefix('/')?
    };
    let name = match rest.split_once('/') {
        Some((pkg, "__init__.py")) => pkg,
        None if root != "." => rest.strip_suffix(".py")?,
        _ => return None,
    };
    (identifier(name) && !NOT_OWN.contains(&name)).then_some(name)
}

/// The project's own top-level importable names at `baseline`: what the
/// import check looks up. Taken from the baseline, so the same baseline
/// and configuration always give the same contract.
pub fn own_names(view: &Path, baseline: &str, config: &Config) -> Result<Vec<String>, Error> {
    let raw = emi_git::run(view, &["ls-tree", "-r", "-z", "--name-only", baseline])
        .map_err(|e| Error::Usage(format!("cannot list the files of the baseline: {e}")))?;
    let mut roots: Vec<&str> = config.python_path.iter().map(String::as_str).collect();
    roots.extend([config.source.as_str(), "."]);
    let is_test = |p: &str| {
        config
            .tests
            .globs
            .iter()
            .any(|g| emi_kernel::glob::fnmatch(p, g))
    };
    let mut names: BTreeSet<String> = BTreeSet::new();
    for path in raw.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path);
        if is_test(&path) {
            continue;
        }
        names.extend(
            roots
                .iter()
                .filter_map(|r| own_name(&path, r))
                .map(str::to_owned),
        );
    }
    Ok(names.into_iter().take(MAX_OWN_NAMES).collect())
}
/// Generous bounds: a check that ends at the timeout is `incomplete`, not
/// a failure, so these only guard against a hung runner.
const TEST_TIMEOUT_S: u64 = 1800;
const INVENTORY_TIMEOUT_S: u64 = 600;

/// Whether the configured test command runs pytest, whose output and exit
/// statuses the receipt knows how to read.
pub fn is_pytest(command: &[String]) -> bool {
    emi_python::proving::pytest::Command::parse(command).is_ok()
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
    view: &Path,
    baseline: &str,
    globs: &[String],
    approved: &BTreeSet<String>,
) -> Result<Vec<String>, Error> {
    let raw = emi_git::run(view, &["ls-tree", "-r", "-z", "--name-only", baseline])
        .map_err(|e| Error::Usage(format!("cannot list the files of the baseline: {e}")))?;
    let mut files: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !approved.contains(p))
        .filter(|p| {
            runner_config::matches(p) || globs.iter().any(|g| emi_kernel::glob::fnmatch(p, g))
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
    trusted: &[String],
    own: &[String],
) -> Value {
    let mut command = config.tests.command.clone();
    let mut verification = json!({"trusted": trusted, "runner_config": "review"});
    let mut evidence = Value::Null;
    if let Ok(pytest) = emi_python::proving::pytest::Command::parse(&config.tests.command) {
        // one outcome line per test, so this run can be compared test by
        // test with the baseline's: a test the baseline already skipped is
        // not a regression, one that stopped running is
        command = pytest.report_argv();
        // Two quiet flags make real pytest print counts by file, not ids.
        // Share the normalizer used by the proving measurement.
        let inventory = pytest.list_argv();
        verification["inventory"] = json!({"command": inventory, "format": "pytest", "timeout_s": INVENTORY_TIMEOUT_S,
                   "collection_errors": "tolerate"});
        // The test check runs the same pytest invocation, so its own
        // report is required too: a passing exit with no readable summary
        // (an early exit) or with skips is not regression evidence.
        evidence = json!({"format": "pytest"});
    }
    let mut execution = json!({"isolation": "best", "network": false, "link": config.link});
    if !config.python_path.is_empty() {
        // relative: each run resolves it in its own copy of the code
        execution["env"] = json!({"PYTHONPATH": config.python_path.join(":")});
    }
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
    if let (Ok(pytest), false) = (
        emi_python::proving::pytest::Command::parse(&config.tests.command),
        own.is_empty(),
    ) {
        let mut argv = vec![
            pytest.interpreter().to_owned(),
            "-c".to_owned(),
            IMPORT_ORIGINS.to_owned(),
        ];
        argv.extend(own.iter().cloned());
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
pub fn open(authority: &Path, repo: &Path, baseline: &str, spec: &Value) -> Result<Store, Error> {
    let task = task_id(spec);
    let store = Store::open(authority, &task, repo).map_err(Error::Usage)?;
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

/// The recorded run of the test check on the baseline itself, under the
/// task's current contract: what the candidate's run is compared with. The
/// baseline's outcomes do not change, so one run serves every later check
/// of the same task; a contract revision (new dependencies) runs it again.
pub fn baseline_check(store: &Store, baseline: &str) -> Result<Option<Value>, String> {
    let contract = store.contract()?;
    let id = contract["contract_id"].as_str().unwrap_or("");
    if let Some(known) = store.latest_decision(baseline, id)? {
        // a run recorded by an older reader names its tests differently
        let kept = recorded_check(store, &known, CHECK)?
            .filter(|c| c["evidence"]["reader"] == crate::engine::evidence::READER);
        if kept.is_some() {
            return Ok(kept);
        }
    }
    let decision = verify(store, &Candidate::Ref(baseline.to_owned()))?;
    recorded_check(store, &decision, CHECK)
}

/// [`baseline_check`], one run at a time for a repository: a check that
/// arrives while another process is running the baseline (the hook warms
/// it when the person sends a message) waits for that run and reads its
/// result instead of running the suite a second time beside it.
pub fn baseline_check_once(
    store: &Store,
    baseline: &str,
    authority: &Path,
) -> Result<Option<Value>, String> {
    let path = authority.join("baseline.lock");
    let lock = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    lock.lock()
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    baseline_check(store, baseline)
}

fn recorded_check(store: &Store, decision: &Value, id: &str) -> Result<Option<Value>, String> {
    Ok(store.rows("check_run")?.into_iter().rev().find(|r| {
        r["check_id"] == id
            && r["candidate_id"] == decision["candidate_id"]
            && r["contract_id"] == decision["contract_id"]
    }))
}

/// Verify `candidate` (a commit id, or the working tree) under the task.
/// With `repin`, linked dependencies that changed since the task was
/// frozen are pinned as they are now and the check runs once more: on a
/// person's machine the environment changes between checks, and a fresh
/// pin still catches a change during a run.
pub fn run(store: &Store, candidate: &Candidate, repin: bool) -> Result<Verified, String> {
    let mut decision = verify(store, candidate)?;
    if repin && decision["gates"]["dependencies"]["state"] == "unverified" {
        let execution = store.contract()?["execution"].clone();
        store.revise(
            &json!({"execution": execution}),
            "emi done",
            "linked dependencies changed between checks",
        )?;
        decision = verify(store, candidate)?;
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
        let spec = spec(dir.path(), &config(), SHA, &trusted, &[]);
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
            ["--collect-only", "-q", "-p", "no:cacheprovider"]
        );
        let command = names(&spec["checks"]["tests"]["command"]);
        assert_eq!((command[0], command[1]), (".venv/bin/python", "-c"));
        assert!(
            command[2].contains("EMI-OUTCOME"),
            "the reporter runs pytest"
        );
        assert_eq!(
            command[3..],
            ["-q", "--tb=no", "--continue-on-collection-errors"]
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
        let own = vec!["app".to_owned()];
        let spec = spec(dir.path(), &c, SHA, &[], &own);
        assert_eq!(spec["execution"]["env"], json!({"PYTHONPATH": "src"}));
        let argv = names(&spec["checks"]["imports"]["command"]);
        assert_eq!(
            (argv[0], argv[1], argv[3]),
            (".venv/bin/python", "-c", "app")
        );
        assert_eq!(spec["requirements"][1]["checks"], json!(["imports"]));
        let mut named = spec.clone();
        named["task_id"] = json!(task_id(&spec));
        assert!(contract::freeze(&named, &dir.path().to_string_lossy(), SHA).is_ok());
        // without a src layout or known names, the contract is as before
        let plain = super::spec(dir.path(), &config(), SHA, &[], &[]);
        assert!(plain["execution"].get("env").is_none());
        assert!(plain["checks"].get("imports").is_none());
    }

    #[test]
    fn own_names_are_the_packages_and_src_modules_of_the_baseline() {
        let dir = repo();
        for name in [
            "src/app/__init__.py",
            "src/tool.py",
            "pkg/__init__.py",
            "top.py",
            "setup.py",
            "tests/__init__.py",
            "tests/test_a.py",
            "docs/conf.py",
        ] {
            commit_file(dir.path(), name, "x = 1\n", name);
        }
        let head = git_out(dir.path(), &["rev-parse", "HEAD"]);
        let view = emi_git::CleanView::of(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        let mut c = config();
        c.source = "src".into();
        c.python_path = vec!["src".into()];
        let own = own_names(view.path(), &head, &c).unwrap_or_default();
        // a module at the repository root is not looked up, nor are tests
        assert_eq!(own, ["app", "pkg", "tool"]);
    }

    #[test]
    fn a_command_that_is_not_pytest_has_no_inventory() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut c = config();
        c.tests.command = vec!["python3".into(), "-m".into(), "unittest".into()];
        assert!(
            spec(dir.path(), &c, SHA, &[], &[])["verification"]
                .get("inventory")
                .is_none()
        );
        assert!(!is_pytest(&c.tests.command));
    }

    #[test]
    fn the_same_inputs_name_the_same_task_and_any_change_names_another() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let id = |c: &Config, sha: &str, trusted: &[String]| {
            task_id(&spec(dir.path(), c, sha, trusted, &[]))
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
        let all = trusted(view.path(), &head, &globs, &BTreeSet::new()).unwrap_or_default();
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
        let less = trusted(view.path(), &head, &globs, &approved).unwrap_or_default();
        assert!(!less.contains(&"tests/test_a.py".to_owned()) && less.len() == 3);
    }
}
