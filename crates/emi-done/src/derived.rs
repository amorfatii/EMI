//! The derived contract: a task contract built from `.emi/config.json`
//! because nobody wrote one. It says one thing: the tests the baseline had
//! must still pass on the candidate, with those tests and the test-runner
//! configuration restored to their baseline versions, run sandboxed
//! without network. Everything else (restoring, comparing the inventory of
//! tests, the sandbox) is `emi-controller`'s.

use std::path::Path;

use emi_controller::contract::{self, finalize};
use emi_controller::{Candidate, Store, runner_config, verify};
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
    command.iter().any(|c| c.contains("pytest"))
}

/// The task spec for `config` at `baseline`. There are no budgets: repair
/// rounds are counted by the hook, and a repeated check must not run out.
/// The guardrail heuristics are off; they are review hints, and what they
/// look for in tests is covered by restoring the originals.
fn spec(repo: &Path, config: &Config, baseline: &str) -> Value {
    let runner = runner_config::globs();
    let mut trusted = config.tests.globs.clone();
    trusted.extend(runner.iter().cloned());
    let pytest = is_pytest(&config.tests.command);
    let mut command = config.tests.command.clone();
    if pytest {
        // failures are named in the short summary; tracebacks would only
        // push those lines out of the output the controller keeps
        command.push("--tb=no".to_owned());
    }
    let mut verification = json!({"trusted": trusted});
    if pytest {
        let mut inventory = config.tests.command.clone();
        inventory.extend(["--collect-only".to_owned(), "-q".to_owned()]);
        verification["inventory"] =
            json!({"command": inventory, "format": "pytest", "timeout_s": INVENTORY_TIMEOUT_S});
    }
    json!({
        "task_id": "done-pending",
        "repo": repo,
        "baseline": baseline,
        "objective": "The baseline's tests still pass on the candidate.",
        "requirements": [{"id": "R1", "acceptance": "every test the baseline had still passes, with the trusted tests restored", "checks": [CHECK]}],
        "checks": {CHECK: {"command": command, "success": {"exit_code": 0}, "timeout_s": TEST_TIMEOUT_S}},
        "envelope": {"permitted_paths": ["*"], "protected_paths": runner},
        "guardrails": false,
        "verification": verification,
        "execution": {"isolation": "best", "network": false, "link": config.link},
    })
}

/// The frozen contract for `config` at `baseline`. Its id names the task,
/// so the same repository, baseline, configuration and dependencies reuse
/// one task and any change of them starts a fresh one.
pub fn contract(repo: &Path, config: &Config, baseline: &str) -> Result<Value, Error> {
    let frozen = contract::freeze(
        &spec(repo, config, baseline),
        &repo.to_string_lossy(),
        baseline,
    )
    .map_err(|e| Error::Config(format!("cannot derive a task contract: {e}")))?;
    let mut core = frozen.contract;
    let suffix: String = core["contract_id"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(24)
        .collect();
    core["task_id"] = json!(format!("done-{suffix}"));
    Ok(finalize(core))
}

/// The task's store under `authority`, created on first use. A stored
/// contract that is not the derived one means the store was altered.
pub fn open(authority: &Path, repo: &Path, contract: &Value) -> Result<Store, String> {
    let task = contract["task_id"].as_str().unwrap_or_default();
    let store = Store::open(authority, task, repo)?;
    if !store.exists() {
        // a concurrent run may have created it first, which is fine
        if let Err(e) = store.create(contract)
            && !store.exists()
        {
            return Err(e);
        }
    }
    if store.contract()?["contract_id"] != contract["contract_id"] {
        return Err(format!(
            "the stored task {task} is not the contract derived from the configuration"
        ));
    }
    Ok(store)
}

/// The decision and the recorded run of the test check for one candidate.
pub struct Verified {
    pub decision: Value,
    pub check: Option<Value>,
}

/// Verify `candidate` (a commit id, or the working tree) under the task.
pub fn run(store: &Store, candidate: &Candidate) -> Result<Verified, String> {
    let decision = verify(store, candidate)?;
    let check = store.rows("check_run")?.into_iter().rev().find(|r| {
        r["check_id"] == CHECK
            && r["candidate_id"] == decision["candidate_id"]
            && r["contract_id"] == decision["contract_id"]
    });
    Ok(Verified { decision, check })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OnUnproven, Tests};

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

    #[test]
    fn the_spec_trusts_tests_and_runner_configuration_and_protects_the_latter() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let spec = spec(dir.path(), &config(), SHA);
        let trusted: Vec<&str> = spec["verification"]["trusted"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            trusted.contains(&"tests/*") && trusted.contains(&"conftest.py"),
            "{trusted:?}"
        );
        let protected = &spec["envelope"]["protected_paths"];
        assert!(
            protected
                .as_array()
                .is_some_and(|p| p.iter().any(|g| g == "*/conftest.py"))
        );
        assert_eq!(
            spec["envelope"]["permitted_paths"],
            json!(["*"]),
            "no scope beyond the runner configuration"
        );
        let inventory = &spec["verification"]["inventory"]["command"];
        assert_eq!(
            inventory,
            &json!([
                ".venv/bin/python",
                "-m",
                "pytest",
                "-q",
                "--collect-only",
                "-q"
            ])
        );
        let command = spec["checks"]["tests"]["command"]
            .as_array()
            .map(|c| c.len());
        assert_eq!(command, Some(5), "--tb=no is added to the check only");
        assert!(
            spec.get("budgets").is_none(),
            "repair rounds are the hook's, not a budget"
        );
        assert_eq!(spec["execution"]["network"], false);
        assert_eq!(spec["execution"]["link"], json!([".venv"]));
    }

    #[test]
    fn a_command_that_is_not_pytest_has_no_inventory() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut c = config();
        c.tests.command = vec!["python3".into(), "-m".into(), "unittest".into()];
        assert!(
            spec(dir.path(), &c, SHA)["verification"]
                .get("inventory")
                .is_none()
        );
        assert!(!is_pytest(&c.tests.command));
    }

    #[test]
    fn the_same_inputs_name_the_same_task_and_any_change_names_another() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let id = |c: &Config, sha: &str| {
            let v = contract(dir.path(), c, sha).unwrap_or_else(|e| panic!("{e}"));
            v["task_id"].as_str().unwrap_or_default().to_owned()
        };
        let base = id(&config(), SHA);
        assert!(base.starts_with("done-"));
        assert!(contract::valid_task_id(&base));
        assert_eq!(base, id(&config(), SHA));
        let mut other = config();
        other.tests.globs.push("spec/*".into());
        assert_ne!(base, id(&other, SHA));
        assert_ne!(
            base,
            id(&config(), "fedcba9876543210fedcba9876543210fedcba98")
        );
    }
}
