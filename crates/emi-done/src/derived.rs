//! The derived contract: a task contract built from `.emi/config.json`
//! because nobody wrote one. It says one thing: the tests the baseline had
//! must still pass on the candidate, with the baseline's tests and
//! test-runner configuration restored to their baseline versions (except
//! files a person approved changing), run sandboxed without network.
//! Everything else (restoring, comparing the inventory of tests, the
//! sandbox) is `emi-controller`'s.

use std::collections::BTreeSet;
use std::path::Path;

use emi_controller::{Candidate, Store, contract, runner_config, verify};
use emi_ledger::pyfmt::digest;
use serde_json::{Value, json};

use crate::Error;
use crate::config::Config;

/// The check that runs the tests.
const CHECK: &str = "tests";
/// Generous bounds: a check that ends at the timeout is `incomplete`, not
/// a failure, so these only guard against a hung runner.
const TEST_TIMEOUT_S: u64 = 1800;
const INVENTORY_TIMEOUT_S: u64 = 600;

/// Whether the configured test command runs pytest, whose output and exit
/// statuses the receipt knows how to read.
pub fn is_pytest(command: &[String]) -> bool {
    emi_proving::pytest::Command::parse(command).is_ok()
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
            runner_config::matches(p) || globs.iter().any(|g| emi_scan::walk::fnmatch(p, g))
        })
        .map(|p| literal(&p))
        .collect();
    files.sort();
    Ok(files)
}

/// The task spec for `config` at `baseline`, trusting the files `trusted`
/// lists. There are no budgets: repair rounds are counted by the hook, and
/// a repeated check must not run out. The guardrail heuristics are off;
/// they are review hints, and what they look for in tests is covered by
/// restoring the originals. Test-runner configuration a candidate adds
/// cannot be restored, so it is a review item, which the receipt turns
/// into a question for a person.
pub fn spec(repo: &Path, config: &Config, baseline: &str, trusted: &[String]) -> Value {
    let pytest = is_pytest(&config.tests.command);
    let mut command = config.tests.command.clone();
    if pytest {
        // failures are named in the short summary; tracebacks would only
        // push those lines out of the output the controller keeps
        command.push("--tb=no".to_owned());
    }
    let mut verification = json!({"trusted": trusted, "runner_config": "review"});
    if let Ok(pytest) = emi_proving::pytest::Command::parse(&config.tests.command) {
        // Two quiet flags make real pytest print counts by file, not ids.
        // Share the normalizer used by the proving measurement.
        let inventory = pytest.list_argv();
        verification["inventory"] =
            json!({"command": inventory, "format": "pytest", "timeout_s": INVENTORY_TIMEOUT_S});
    }
    json!({
        "task_id": "",
        "repo": repo,
        "baseline": baseline,
        "objective": "The baseline's tests still pass on the candidate.",
        "requirements": [{"id": "R1", "acceptance": "every test the baseline had still passes, with the trusted tests restored", "checks": [CHECK]}],
        "checks": {CHECK: {"command": command, "success": {"exit_code": 0}, "timeout_s": TEST_TIMEOUT_S}},
        "envelope": {"permitted_paths": ["*"]},
        "guardrails": false,
        "verification": verification,
        "execution": {"isolation": "best", "network": false, "link": config.link},
    })
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
}

fn recorded_check(store: &Store, decision: &Value) -> Result<Option<Value>, String> {
    Ok(store.rows("check_run")?.into_iter().rev().find(|r| {
        r["check_id"] == CHECK
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
    let check = recorded_check(store, &decision)?;
    Ok(Verified { decision, check })
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
            repair_rounds: 3,
            on_unproven: OnUnproven::Report,
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
        let spec = spec(dir.path(), &config(), SHA, &trusted);
        assert_eq!(names(&spec["verification"]["trusted"]), ["tests/test_a.py"]);
        assert_eq!(spec["verification"]["runner_config"], "review");
        assert_eq!(spec["envelope"]["permitted_paths"], json!(["*"]));
        assert!(spec["envelope"].get("protected_paths").is_none());
        let inventory = &spec["verification"]["inventory"]["command"];
        assert_eq!(
            inventory,
            &json!([
                ".venv/bin/python",
                "-m",
                "pytest",
                "--collect-only",
                "-q",
                "-p",
                "no:cacheprovider"
            ])
        );
        assert_eq!(
            spec["checks"]["tests"]["command"].as_array().map(Vec::len),
            Some(5)
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
    fn a_command_that_is_not_pytest_has_no_inventory() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut c = config();
        c.tests.command = vec!["python3".into(), "-m".into(), "unittest".into()];
        assert!(
            spec(dir.path(), &c, SHA, &[])["verification"]
                .get("inventory")
                .is_none()
        );
        assert!(!is_pytest(&c.tests.command));
    }

    #[test]
    fn the_same_inputs_name_the_same_task_and_any_change_names_another() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let id =
            |c: &Config, sha: &str, trusted: &[String]| task_id(&spec(dir.path(), c, sha, trusted));
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
            let hit = emi_scan::walk::fnmatch(path, glob);
            assert_eq!(hit, path == "tests/test_[b].py", "{path} against {glob}");
        }
        let approved: BTreeSet<String> = ["tests/test_a.py".to_owned()].into();
        let less = trusted(view.path(), &head, &globs, &approved).unwrap_or_default();
        assert!(!less.contains(&"tests/test_a.py".to_owned()) && less.len() == 3);
    }
}
