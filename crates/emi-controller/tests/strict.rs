//! What a contract may say and how a decision reduces: unsupported or
//! ignored contract content is refused at freeze, and an acceptance cannot
//! rest on a requirement that was not verified.

use std::path::Path;
use std::process::Command;

use emi_controller::{Candidate, Store, create, verify};
use serde_json::{Value, json};
use tempfile::TempDir;

const BUGGY: &str = "def add(a, b):\n    return a - b\n";
const FIXED: &str = "def add(a, b):\n    return a + b\n";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository holding `app.py` at `app`, committed, and an empty
/// authority directory outside it.
fn repo_with(app: &str) -> (TempDir, TempDir) {
    let repo = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let authority = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("app.py"), app).unwrap_or_else(|e| panic!("{e}"));
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "base"]);
    (repo, authority)
}

/// The spec `emi task draft` writes in miniature: one requirement, one
/// check that asserts the fix, `app.py` the only file the change may touch.
fn spec(repo: &Path, task: &str) -> Value {
    json!({
        "task_id": task, "repo": repo, "baseline": "HEAD",
        "objective": "make add() add",
        "requirements": [{"id": "R1", "acceptance": "add(1, 2) == 3", "checks": ["unit"]}],
        "checks": {"unit": {"command": ["python3", "-c", "import app; assert app.add(1, 2) == 3"],
                            "success": {"exit_code": 0}, "timeout_s": 30}},
        "envelope": {"permitted_paths": ["app.py"]},
        "budgets": {"max_retries": 4},
    })
}

fn refusal(authority: &Path, spec: &Value) -> String {
    create(authority, spec).err().unwrap_or_default()
}

#[test]
fn a_token_budget_is_refused_because_nothing_measures_tokens() {
    let (repo, authority) = repo_with(BUGGY);
    let mut s = spec(repo.path(), "tok");
    s["budgets"] = json!({"max_retries": 4, "max_tokens": 100_000});
    let err = refusal(authority.path(), &s);
    assert!(
        err.contains("max_tokens") && err.contains("cannot observe token use"),
        "{err}"
    );

    s["budgets"] = json!({"max_retries": 4, "max_time_s": 60});
    create(authority.path(), &s).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(authority.path(), "tok", repo.path()).unwrap_or_else(|e| panic!("{e}"));
    let err = store
        .revise(&json!({"budgets": {"max_tokens": 5}}), "lead", "why")
        .err()
        .unwrap_or_default();
    assert!(err.contains("max_tokens"), "{err}");
}

#[test]
fn the_unbroken_spec_freezes_and_verifies() {
    // the control for every refusal in this file: the spec they patch is valid
    let (repo, authority) = repo_with(FIXED);
    create(authority.path(), &spec(repo.path(), "ok")).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(authority.path(), "ok", repo.path()).unwrap_or_else(|e| panic!("{e}"));
    let d = verify(&store, &Candidate::Ref("HEAD".into())).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(d["decision"], "accepted", "{d}");
}

/// A spec patch and the error naming what it got wrong.
type Patch = fn(&mut Value);

fn assert_refused(cases: &[(&str, Patch, &str)]) {
    let (repo, authority) = repo_with(FIXED);
    for (place, patch, expected) in cases {
        let mut s = spec(repo.path(), "strict");
        patch(&mut s);
        let err = refusal(authority.path(), &s);
        assert!(err.contains(expected), "{place}: {err:?}");
    }
}

#[test]
fn success_criteria_the_verifier_does_not_evaluate_are_refused() {
    assert_refused(&[
        (
            "an output criterion",
            |s| s["checks"]["unit"]["success"]["stdout_contains"] = json!("ok"),
            "check unit: success: unknown key stdout_contains",
        ),
        (
            "an artifact criterion",
            |s| s["checks"]["unit"]["success"] = json!({"exit_code": 0, "report": "out.xml"}),
            "check unit: success: unknown key report",
        ),
    ]);
}

#[test]
fn the_expected_exit_status_must_be_stated_as_an_integer() {
    let message = "check unit success.exit_code must be an integer exit status in 0..=255";
    assert_refused(&[
        (
            "absent",
            |s| s["checks"]["unit"]["success"] = json!({}),
            message,
        ),
        (
            "a string",
            |s| s["checks"]["unit"]["success"] = json!({"exit_code": "0"}),
            message,
        ),
        (
            "a fraction",
            |s| s["checks"]["unit"]["success"] = json!({"exit_code": 0.5}),
            message,
        ),
        (
            "a boolean",
            |s| s["checks"]["unit"]["success"] = json!({"exit_code": true}),
            message,
        ),
        (
            "negative",
            |s| s["checks"]["unit"]["success"] = json!({"exit_code": -1}),
            message,
        ),
        (
            "beyond a status byte",
            |s| s["checks"]["unit"]["success"] = json!({"exit_code": 256}),
            message,
        ),
    ]);
}

#[test]
fn keys_no_part_of_the_contract_reads_are_refused_wherever_they_are() {
    assert_refused(&[
        (
            "a check",
            |s| s["checks"]["unit"]["cwd"] = json!("sub"),
            "check unit: unknown key cwd",
        ),
        (
            "structural",
            |s| s["structural"] = json!({"probe": "python", "source": ".", "allowence": 3}),
            "structural: unknown key allowence",
        ),
        (
            "warning_policy",
            |s| s["warning_policy"] = json!({"treatment": "review", "severity": "high"}),
            "warning_policy: unknown key severity",
        ),
        (
            "guardrails",
            |s| s["guardrails"] = json!({"tires": {"tests": "block"}}),
            "guardrails: unknown key tires",
        ),
    ]);
}

#[test]
fn guardrail_settings_that_would_be_ignored_are_refused() {
    assert_refused(&[
        (
            "scope belongs to the envelope",
            |s| s["guardrails"] = json!({"permitted_paths": ["app.py"]}),
            "guardrails.permitted_paths is not read here: scope is the envelope's",
        ),
        (
            "a budget of the wrong type",
            |s| s["guardrails"] = json!({"max_net_sloc": "300"}),
            "guardrails.max_net_sloc must be an integer",
        ),
        (
            "a negative file budget",
            |s| s["guardrails"] = json!({"max_files": -1}),
            "guardrails.max_files must be a nonnegative integer",
        ),
        (
            "tiers that are not a map",
            |s| s["guardrails"] = json!({"tiers": ["tests"]}),
            "guardrails.tiers must be an object",
        ),
    ]);
}

#[test]
fn a_list_of_exclusions_is_refused_because_nothing_enforces_it() {
    assert_refused(&[(
        "exclusions",
        |s| s["exclusions"] = json!(["no network calls"]),
        "exclusions is not enforced",
    )]);
    // an empty list says nothing, so it stays accepted
    let (repo, authority) = repo_with(FIXED);
    let mut s = spec(repo.path(), "empty-exclusions");
    s["exclusions"] = json!([]);
    create(authority.path(), &s).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn a_revision_is_held_to_the_same_rules() {
    let (repo, authority) = repo_with(FIXED);
    create(authority.path(), &spec(repo.path(), "rev")).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(authority.path(), "rev", repo.path()).unwrap_or_else(|e| panic!("{e}"));
    let check =
        |success: Value| json!({"checks": {"unit": {"command": ["true"], "success": success}}});
    for (changes, expected) in [
        (check(json!({})), "success.exit_code must be an integer"),
        (
            check(json!({"exit_code": 0, "stderr_empty": true})),
            "unknown key stderr_empty",
        ),
        (
            json!({"guardrails": {"max_files": "x"}}),
            "guardrails.max_files",
        ),
        (json!({"exclusions": ["x"]}), "exclusions is not enforced"),
    ] {
        let err = store
            .revise(&changes, "lead", "why")
            .err()
            .unwrap_or_default();
        assert!(err.contains(expected), "{changes}: {err:?}");
    }
}

/// A task whose only check outlives its timeout: the requirement it binds
/// can be neither verified nor failed.
fn unverifiable_task(name: &str, policy: Option<Value>) -> (TempDir, TempDir, Store) {
    let (repo, authority) = repo_with(FIXED);
    let mut s = spec(repo.path(), name);
    s["checks"]["unit"] = json!({"command": ["python3", "-c", "import time; time.sleep(30)"],
                                 "success": {"exit_code": 0}, "timeout_s": 1});
    if let Some(policy) = policy {
        s["completion_policy"] = policy;
    }
    create(authority.path(), &s).unwrap_or_else(|e| panic!("{e}"));
    let store = Store::open(authority.path(), name, repo.path()).unwrap_or_else(|e| panic!("{e}"));
    (repo, authority, store)
}

fn verify_head(store: &Store) -> Value {
    verify(store, &Candidate::Ref("HEAD".into())).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn an_unverified_requirement_is_never_accepted_by_default() {
    for policy in [
        None,
        Some(json!({"require_all_requirements_verified": true})),
    ] {
        let (_repo, _authority, store) = unverifiable_task("unv-default", policy);
        let d = verify_head(&store);
        assert_eq!(d["decision"], "incomplete", "{d}");
        assert_eq!(d["requirements"][0]["state"], "unverified", "{d}");
        assert!(d.get("relied_on_policy").is_none(), "{d}");
    }
}

#[test]
fn an_acceptance_that_relies_on_the_policy_says_so() {
    let policy = json!({"require_all_requirements_verified": false});
    let (_repo, _authority, store) = unverifiable_task("unv-policy", Some(policy));
    let d = verify_head(&store);
    assert_eq!(d["decision"], "accepted", "{d}");
    let reason = d["reasons"][0].as_str().unwrap_or_default();
    assert!(
        reason.contains("completion_policy.require_all_requirements_verified is false")
            && reason.contains("R1")
            && !reason.contains("every requirement verified"),
        "{d}"
    );
    assert_eq!(
        d["relied_on_policy"],
        json!({"policy": "completion_policy.require_all_requirements_verified",
               "value": false, "unverified_requirements": ["R1"]}),
        "{d}"
    );
    assert!(
        d["limits"].as_array().into_iter().flatten().any(|l| l
            .as_str()
            .is_some_and(|l| l.contains("left unverified were accepted") && l.contains("R1"))),
        "{d}"
    );
    let receipt = emi_controller::receipt(&store, &Candidate::Ref("HEAD".into()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        receipt["not_verified"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|n| n["what"] == "requirement R1"),
        "{receipt}"
    );
    let (done, record) = emi_controller::complete(&store, "HEAD").unwrap_or_else(|e| panic!("{e}"));
    assert!(done, "{record}");
    assert!(
        record["reason"]
            .as_str()
            .is_some_and(|r| r.contains("require_all_requirements_verified")),
        "{record}"
    );
}

/// Re-stamp the stored contract after `edit`, as a contract frozen by an
/// earlier release looks: a valid id over content the current rules refuse.
fn store_edited(store: &Store, edit: impl FnOnce(&mut Value)) {
    let mut contract = store.contract().unwrap_or_else(|e| panic!("{e}"));
    edit(&mut contract);
    let contract = emi_controller::contract::finalize(contract);
    let id = contract["contract_id"].as_str().unwrap_or_default();
    let file = store.dir.join(format!("contract-r1-{}.json", &id[..16]));
    std::fs::write(&file, contract.to_string()).unwrap_or_else(|e| panic!("{e}"));
    let mut state = store.state().unwrap_or_else(|e| panic!("{e}"));
    state["current_contract_id"] = json!(id);
    store.save_state(&state).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn a_stored_check_without_an_exit_status_is_not_judged_by_a_default() {
    for success in [
        json!({}),
        json!({"exit_code": "0"}),
        json!({"stdout_contains": "ok"}),
    ] {
        let (repo, authority) = repo_with(FIXED);
        create(authority.path(), &spec(repo.path(), "legacy")).unwrap_or_else(|e| panic!("{e}"));
        let store =
            Store::open(authority.path(), "legacy", repo.path()).unwrap_or_else(|e| panic!("{e}"));
        store_edited(&store, |c| c["checks"]["unit"]["success"] = success.clone());
        let d = verify_head(&store);
        assert_eq!(d["decision"], "incomplete", "{success}: {d}");
        assert_eq!(d["checks"][0]["status"], "error", "{success}: {d}");
    }
}
