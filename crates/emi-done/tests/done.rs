//! The done check from a fixture repository to its receipt and action.

mod support;

use emi_done::candidate::Spec;
use emi_done::{Action, Error, Given, Setting};
use serde_json::json;
use support::{APP, BROKEN_APP, CONFIG, Fixture, TEST_APP, git, strs};

#[test]
fn a_clean_change_holds_and_the_receipt_is_deterministic() {
    let fx = Fixture::new();
    fx.write(
        "app.py",
        &format!("{APP}\n\ndef mul(a, b):\n    return a * b\n"),
    );
    let first = fx.check();
    let r = &first.receipt;
    assert_eq!(first.action, Action::Stop, "{r}");
    assert_eq!(r["schema"], "emi-done-receipt-0.1");
    assert_eq!(r["regression"]["status"], "held", "{r}");
    assert_eq!(r["regression"]["tests_run"], 2, "{r}");
    assert_eq!(r["regression"]["failed"], json!([]));
    assert_eq!(r["baseline"]["source"], "merge-base:main");
    assert_eq!(r["candidate"]["id"], "worktree");
    assert_eq!(
        r["candidate"]["content_id"].as_str().map(str::len),
        Some(40)
    );
    assert_eq!(
        r["assurance"],
        json!({"setting": "cooperative", "isolation": "sandboxed"})
    );
    assert_eq!(r["changed_old_tests"], json!([]));
    assert_eq!(r["change"]["status"], "unknown");
    assert!(!strs(&r["not_verified"]).is_empty());
    let second = fx.check();
    assert_eq!(
        first.receipt, second.receipt,
        "same candidate, baseline and config, same receipt"
    );
}

#[test]
fn a_broken_regression_repairs_naming_the_first_failing_test() {
    let fx = Fixture::new();
    fx.write("app.py", BROKEN_APP);
    let out = fx.check();
    let r = &out.receipt;
    assert_eq!(out.action, Action::Repair, "{r}");
    assert_eq!(r["regression"]["status"], "broken");
    assert_eq!(
        r["regression"]["failed"],
        json!(["tests/test_app.py::test_add"])
    );
    assert_eq!(r["regression"]["tests_run"], 2);
    assert_eq!(strs(&r["repair"]).len(), 1, "one short line: {r}");
    assert!(strs(&r["repair"])[0].contains("tests/test_app.py::test_add"));
}

#[test]
fn an_old_test_edited_to_hide_a_bug_is_run_in_its_original_form() {
    let fx = Fixture::new();
    fx.write("app.py", BROKEN_APP);
    fx.write(
        "tests/test_app.py",
        "from app import add\n\n\ndef test_add():\n    assert add(1, 2) == -1\n\n\ndef test_zero():\n    assert add(0, 0) == 0\n",
    );
    let out = fx.check();
    let r = &out.receipt;
    assert_eq!(
        out.action,
        Action::Repair,
        "the table puts a broken regression before asking a person: {r}"
    );
    assert_eq!(r["regression"]["status"], "broken");
    assert_eq!(
        r["regression"]["failed"],
        json!(["tests/test_app.py::test_add"])
    );
    let old = &r["changed_old_tests"][0];
    assert_eq!(old["item"], "old-test:tests/test_app.py");
    assert_eq!(old["kind"], "modified");
    assert_eq!(old["restored_result"], "fails", "{r}");
    assert_eq!(old["approved"], false);
}

#[test]
fn a_changed_old_test_asks_a_person_and_the_approval_is_bound_to_that_version() {
    let fx = Fixture::new();
    let edited = format!("{TEST_APP}\n\ndef test_more():\n    assert add(2, 2) == 4\n");
    fx.write("tests/test_app.py", &edited);
    let out = fx.check();
    let r = &out.receipt;
    assert_eq!(out.action, Action::AskPerson, "{r}");
    assert_eq!(r["regression"]["status"], "held");
    assert_eq!(r["changed_old_tests"][0]["restored_result"], "passes");
    assert_eq!(r["changed_old_tests"][0]["approved"], false);
    let text = emi_done::text::render(r);
    assert!(
        text.contains("emi done --approve old-test:tests/test_app.py"),
        "{text}"
    );

    let item = vec!["old-test:tests/test_app.py".to_owned()];
    let approved = fx.run(&fx.request(&item));
    assert_eq!(approved.action, Action::Stop, "{}", approved.receipt);
    assert_eq!(approved.receipt["changed_old_tests"][0]["approved"], true);
    let remembered = fx.check();
    assert_eq!(
        remembered.action,
        Action::Stop,
        "the approval is recorded, not per call"
    );

    fx.write(
        "tests/test_app.py",
        &format!("{edited}\n\ndef test_again():\n    assert add(3, 3) == 6\n"),
    );
    let edited_again = fx.check();
    assert_eq!(
        edited_again.action,
        Action::AskPerson,
        "a further edit needs a new approval"
    );
}

#[test]
fn a_deleted_old_test_is_restored_for_the_run_and_approved_as_a_deletion() {
    let fx = Fixture::new();
    fx.remove("tests/test_app.py");
    let out = fx.check();
    let r = &out.receipt;
    assert_eq!(out.action, Action::AskPerson, "{r}");
    assert_eq!(
        r["regression"]["status"], "held",
        "the deleted tests still ran: {r}"
    );
    assert_eq!(r["regression"]["tests_run"], 2);
    assert_eq!(r["changed_old_tests"][0]["kind"], "deleted");
    let item = vec!["old-test:tests/test_app.py".to_owned()];
    assert_eq!(fx.run(&fx.request(&item)).action, Action::Stop);
}

#[test]
fn approving_something_that_is_not_a_changed_old_test_is_refused() {
    let fx = Fixture::new();
    let item = vec!["old-test:tests/test_app.py".to_owned()];
    let err = fx.try_run(&fx.request(&item)).err();
    assert!(
        matches!(&err, Some(Error::Usage(m)) if m.contains("not a changed old test")),
        "{err:?}"
    );
    let odd = vec!["whatever".to_owned()];
    assert!(fx.try_run(&fx.request(&odd)).is_err());
}

#[test]
fn a_missing_config_is_a_configuration_error_that_names_the_fix() {
    let fx = Fixture::new();
    git(fx.path(), &["rm", "-q", "-f", ".emi/config.json"]);
    fx.commit_all("drop the config");
    let err = fx.try_run(&fx.request(&[])).err();
    assert!(
        matches!(&err, Some(Error::Config(m)) if m.contains("emi init")),
        "{err:?}"
    );
}

#[test]
fn a_candidate_commit_is_checked_as_committed() {
    let fx = Fixture::new();
    fx.write("app.py", BROKEN_APP);
    fx.commit_all("break it");
    fx.write("app.py", APP);
    let request = emi_done::Request {
        candidate: Spec::Ref("HEAD".into()),
        ..fx.request(&[])
    };
    let out = fx.run(&request);
    assert_eq!(
        out.action,
        Action::Repair,
        "the working tree is fixed, the commit is not: {}",
        out.receipt
    );
    assert_eq!(
        out.receipt["candidate"]["id"].as_str().map(str::len),
        Some(40)
    );
}

#[test]
fn on_the_default_branch_a_baseline_must_be_given() {
    let fx = Fixture::new();
    git(fx.path(), &["checkout", "-q", "main"]);
    let err = fx.try_run(&fx.request(&[])).err();
    assert!(
        matches!(&err, Some(Error::Usage(m)) if m.contains("--baseline")),
        "{err:?}"
    );
    let session = fx.try_run(&fx.check_against("HEAD", Given::Session));
    let out = session.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out.receipt["baseline"]["source"], "session");
    assert_eq!(out.action, Action::Stop, "{}", out.receipt);
}

/// A candidate that rewrites the configuration to run nothing, on top of a
/// bug the baseline's configuration would catch.
fn tampered(fx: &Fixture) {
    fx.write("app.py", BROKEN_APP);
    let weak = CONFIG.replace(
        r#"[".venv/bin/python", "-m", "pytest", "-q"]"#,
        r#"[".venv/bin/python", "-c", "pass"]"#,
    );
    assert_ne!(weak, CONFIG);
    fx.write(".emi/config.json", &weak);
}

#[test]
fn enforced_reads_the_config_from_the_baseline_and_cooperative_from_the_candidate() {
    let fx = Fixture::new();
    tampered(&fx);
    let cooperative = fx.run(&fx.request(&[]));
    assert_eq!(
        cooperative.receipt["regression"]["status"], "held",
        "the candidate's own config was used"
    );
    assert!(
        strs(&cooperative.receipt["not_verified"])
            .iter()
            .any(|n| n.contains("differs from the baseline's")),
        "{}",
        cooperative.receipt
    );
    let enforced = emi_done::Request {
        setting: Setting::Enforced,
        ..fx.check_against("main", Given::Explicit)
    };
    let out = fx.run(&enforced);
    assert_eq!(out.receipt["assurance"]["setting"], "enforced");
    assert_eq!(
        out.receipt["regression"]["status"], "broken",
        "{}",
        out.receipt
    );
    assert_eq!(out.action, Action::Repair);
}

#[test]
fn enforced_without_a_config_at_the_baseline_is_incomplete() {
    let fx = Fixture::new();
    // a baseline that has the tests but was committed before `emi init`
    git(
        fx.path(),
        &["checkout", "-q", "-b", "uninitialised", "main"],
    );
    git(fx.path(), &["rm", "-q", "-f", ".emi/config.json"]);
    fx.commit_all("before emi init");
    fx.write(".emi/config.json", CONFIG);
    let enforced = emi_done::Request {
        setting: Setting::Enforced,
        ..fx.check_against("HEAD", Given::Explicit)
    };
    let out = fx.run(&enforced);
    assert_eq!(out.action, Action::Incomplete, "{}", out.receipt);
    assert_eq!(out.receipt["regression"]["status"], "unknown");
    assert!(strs(&out.receipt["regression"]["reasons"])[0].contains("absent at the baseline"));
    let cooperative = emi_done::Request {
        baseline: Some(("HEAD", Given::Explicit)),
        ..fx.request(&[])
    };
    let out = fx.run(&cooperative);
    assert_eq!(
        out.action,
        Action::Stop,
        "cooperative uses the working copy: {}",
        out.receipt
    );
}

#[test]
fn enforced_needs_a_baseline_and_refuses_local_approvals() {
    let fx = Fixture::new();
    let no_baseline = emi_done::Request {
        setting: Setting::Enforced,
        ..fx.request(&[])
    };
    assert!(matches!(fx.try_run(&no_baseline), Err(Error::Usage(m)) if m.contains("--baseline")));
    let approve = vec!["old-test:tests/test_app.py".to_owned()];
    let with_approval = emi_done::Request {
        setting: Setting::Enforced,
        baseline: Some(("main", Given::Explicit)),
        approve: &approve,
        ..fx.request(&[])
    };
    assert!(matches!(fx.try_run(&with_approval), Err(Error::Usage(m)) if m.contains("enforced")));
}

#[test]
fn an_added_conftest_is_not_a_pass() {
    let fx = Fixture::new();
    fx.write("tests/conftest.py", "# would rewrite outcomes\n");
    let out = fx.check();
    assert_eq!(out.action, Action::Repair, "{}", out.receipt);
    assert_eq!(out.receipt["regression"]["status"], "broken");
    let reasons = strs(&out.receipt["regression"]["reasons"]).join(" ");
    assert!(reasons.contains("conftest.py"), "{reasons}");
}

#[test]
fn a_state_directory_inside_the_repository_is_incomplete_not_a_pass() {
    let fx = Fixture::new();
    let inside = fx.path().join("state");
    let request = emi_done::Request {
        state_root: &inside,
        ..fx.request(&[])
    };
    let out = fx.run(&request);
    assert_eq!(out.action, Action::Incomplete, "{}", out.receipt);
    assert_eq!(out.receipt["regression"]["status"], "unknown");
}
