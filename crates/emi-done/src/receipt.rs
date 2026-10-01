//! The receipt (`emi-done-receipt-0.4`): the two evidence parts, the
//! changed old tests, the action, and what none of it establishes. The
//! change part arrives as a value ([`ChangeEvidence`]); everything else is
//! assembled from what this crate measured.

use std::path::Path;

use serde_json::{Value, json};

use crate::Setting;
use crate::action::{self, Action};
use crate::baseline::Baseline;
use crate::candidate::Candidate;
use crate::change::ChangeEvidence;
use crate::config::OnUnproven;
use crate::oldtests::Entry;
use crate::regression::{Regression, Status};

pub const SCHEMA: &str = "emi-done-receipt-0.4";

/// Everything a receipt is made of.
pub struct Parts<'a> {
    pub repo: &'a Path,
    pub baseline: &'a Baseline,
    pub candidate: &'a Candidate,
    pub setting: Setting,
    /// `sandboxed`, or what ran instead; `unknown` when nothing ran.
    pub isolation: &'a str,
    pub regression: &'a Regression,
    pub old_tests: &'a [Entry],
    pub change: &'a ChangeEvidence,
    pub on_unproven: OnUnproven,
    /// Environment faults beyond those the two evidence parts carry.
    pub faults: Vec<String>,
    pub not_verified: Vec<String>,
    /// The part of `not_verified` that is particular to this run.
    pub this_run: Vec<String>,
}

/// Parts that could not be measured for an environment reason.
fn faults(p: &Parts) -> Vec<String> {
    let mut out = p.faults.clone();
    if p.regression.status == Status::Unknown {
        out.push(format!(
            "regression evidence could not be measured: {}",
            p.regression
                .reasons
                .first()
                .map_or("no reason given", String::as_str)
        ));
    }
    out.extend(
        p.change
            .fault()
            .map(|f| format!("change evidence could not be measured: {f}")),
    );
    out
}

fn facts(p: &Parts) -> action::Facts {
    let change = p.change.facts();
    action::Facts {
        faults: faults(p),
        regression_broken: p.regression.status == Status::Broken,
        failing_tests: change.failing.len(),
        redundant_tests: change.redundant.len(),
        unproven_functions: change.unproven.len(),
        on_unproven: p.on_unproven,
        unapproved_old_tests: p.old_tests.iter().filter(|e| !e.approved).count(),
    }
}

/// The action the parts lead to.
pub fn action_of(p: &Parts) -> Action {
    action::decide(&facts(p))
}

/// Most items one repair names: a proposer told about five failures has
/// enough to do, and the count of the rest is stated.
const MAX_REPAIR_ITEMS: usize = 5;

/// One line per item, at most [`MAX_REPAIR_ITEMS`], then how many more.
fn itemize<T>(items: &[T], line: impl Fn(&T) -> String) -> Vec<String> {
    let mut out: Vec<String> = items.iter().take(MAX_REPAIR_ITEMS).map(line).collect();
    if items.len() > MAX_REPAIR_ITEMS {
        out.push(format!(
            "and {} more of the same kind.",
            items.len() - MAX_REPAIR_ITEMS
        ));
    }
    out
}

/// What to do about a redundant test, by why it is redundant: a proposer
/// told only to remove it deletes a test it could have fixed.
fn redundant_line(t: &crate::change::Redundant) -> String {
    match t.reason.as_str() {
        "passes at the baseline" => format!(
            "{} passes without your change, so it does not show the change works; make it fail on the old code, or remove it.",
            t.id
        ),
        "executes no changed line" => format!(
            "{} runs none of the lines you changed; make it exercise the change, or remove it.",
            t.id
        ),
        _ => format!("Remove {}: it does not establish changed behaviour.", t.id),
    }
}

/// The first failing kind of thing, in the order of the action table, with
/// every item of that kind: one round can then settle all of them. The
/// proposer reads it, so each line names one thing and says what to do.
fn repair_lines(p: &Parts) -> Vec<String> {
    let r = p.regression;
    let change = p.change.facts();
    if r.status == Status::Broken {
        let mut out = itemize(&r.failed, |id| {
            format!(
                "{id} fails; fix the code (an edited or deleted test is run in its original form)."
            )
        });
        out.extend(itemize(&r.missing, |id| {
            format!("{id} no longer runs; restore it.")
        }));
        if out.is_empty() {
            out.push(format!(
                "Regression broken: {}",
                r.reasons.first().map_or("see the receipt", String::as_str)
            ));
        }
        return out;
    }
    if !change.failing.is_empty() {
        return itemize(&change.failing, |id| {
            format!(
                "{id} is a test you added and it fails on your change; fix the code or the test."
            )
        });
    }
    if !change.redundant.is_empty() {
        return itemize(&change.redundant, redundant_line);
    }
    if p.on_unproven == OnUnproven::Repair {
        return itemize(&change.unproven, |f| {
            format!("No proving test runs {f}; add a test that fails without your change.")
        });
    }
    Vec::new()
}

/// Assemble the receipt and return it with its action.
pub fn assemble(p: &Parts) -> (Value, Action) {
    let action = action_of(p);
    let repair: Vec<String> = if action == Action::Repair {
        repair_lines(p)
    } else {
        Vec::new()
    };
    let receipt = json!({
        "schema": SCHEMA,
        "repo": p.repo,
        "baseline": {"sha": p.baseline.sha, "source": p.baseline.source},
        "candidate": {"id": p.candidate.id, "content_id": p.candidate.content_id},
        "assurance": {"setting": p.setting.as_str(), "isolation": p.isolation},
        "regression": p.regression.to_value(),
        "changed_old_tests": p.old_tests.iter().map(Entry::to_value).collect::<Vec<_>>(),
        "change": p.change.to_value(),
        "action": action.as_str(),
        "repair": repair,
        "not_verified": p.not_verified,
        "this_run": p.this_run,
    });
    (receipt, action)
}

/// What the person who reads the receipt must not take from it.
pub struct Limits<'a> {
    pub setting: Setting,
    pub link: &'a [String],
    pub pytest: bool,
    pub change: &'a ChangeEvidence,
    /// Why the configuration in use is not the baseline's, if it is not.
    pub config_note: Option<String>,
}

/// Everything the receipt does not establish: the limits every receipt of
/// this setting has, then those of this run ([`this_run`]).
pub fn not_verified(l: &Limits) -> Vec<String> {
    let mut out = standing(l);
    out.extend(this_run(l));
    out
}

/// The limits every receipt of this setting has. They are always in the
/// receipt; the short text form counts them instead of repeating them.
fn standing(l: &Limits) -> Vec<String> {
    let mut out = vec![
        "Behaviour that no test exercises: tests passing does not show the task's behaviour exists."
            .to_owned(),
        "The baseline's tests are taken as correct, and they ran once: a flaky test could change the result."
            .to_owned(),
        "Tests the baseline already skipped, expected to fail, could not collect or failed are not regression evidence: nothing is known about the behaviour they cover."
            .to_owned(),
        "Guardrail heuristics (secret files, CI edits, test suppressions) were not run.".to_owned(),
    ];
    out.push(match l.setting {
        Setting::Cooperative => "Cooperative verification: the proposer could alter the controller's inputs (the configuration, the recorded approvals, the dependencies the tests run). It catches a careless proposer, not a cheating one.".to_owned(),
        Setting::Enforced => "Enforced verification: the configuration was read from the baseline; approvals recorded on a person's machine were not used.".to_owned(),
    });
    if !l.link.is_empty() {
        out.push(format!(
            "Linked dependencies ({}) were taken as found on this machine.",
            l.link.join(", ")
        ));
    }
    out
}

/// What this run in particular did not establish, beyond the standing
/// limits: these are what a person should read every time.
pub fn this_run(l: &Limits) -> Vec<String> {
    let mut out = Vec::new();
    if !l.pytest {
        out.push("No test inventory was taken because the test command is not pytest: a command that ends with success before running its tests would count as held.".to_owned());
    }
    out.extend(l.config_note.clone());
    match l.change {
        ChangeEvidence::NotMeasured(why) | ChangeEvidence::Failed(why) => {
            out.push(format!("Whether the new behaviour exists: {why}."));
        }
        ChangeEvidence::Measured(_) => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oldtests::{Changed, Kind, What};
    use crate::regression::Restored;

    fn held() -> Regression {
        Regression {
            status: Status::Held,
            tests_run: Some(3),
            failed: Vec::new(),
            missing: Vec::new(),
            reasons: Vec::new(),
            attributed: Vec::new(),
            complete: true,
        }
    }

    fn old(approved: bool) -> Entry {
        Entry {
            changed: Changed {
                path: "tests/t.py".into(),
                what: What::OldTest,
                kind: Kind::Modified,
                content: "c".repeat(40),
            },
            restored: Restored::Passes,
            approved,
        }
    }

    fn with<R>(
        regression: &Regression,
        old_tests: &[Entry],
        change: &ChangeEvidence,
        on_unproven: OnUnproven,
        f: impl FnOnce(&Parts) -> R,
    ) -> R {
        let baseline = Baseline {
            sha: "a".repeat(40),
            source: "explicit".into(),
        };
        let candidate = Candidate {
            id: "worktree".into(),
            content_id: "b".repeat(40),
            rev: "c".repeat(40),
        };
        f(&Parts {
            repo: Path::new("/r"),
            baseline: &baseline,
            candidate: &candidate,
            setting: Setting::Cooperative,
            isolation: "sandboxed",
            regression,
            old_tests,
            change,
            on_unproven,
            faults: Vec::new(),
            not_verified: vec!["x".into()],
            this_run: Vec::new(),
        })
    }

    fn unmeasured() -> ChangeEvidence {
        ChangeEvidence::NotMeasured("change evidence is not measured yet".into())
    }

    #[test]
    fn a_held_regression_with_nothing_to_approve_stops_and_has_every_field() {
        let (v, action) = with(&held(), &[], &unmeasured(), OnUnproven::Report, assemble);
        assert_eq!(action, Action::Stop);
        for key in [
            "schema",
            "repo",
            "baseline",
            "candidate",
            "assurance",
            "regression",
            "changed_old_tests",
            "change",
            "action",
            "repair",
            "not_verified",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert_eq!(v["change"]["status"], "unknown");
        assert_eq!(v["repair"], json!([]));
    }

    #[test]
    fn an_unapproved_old_test_asks_a_person_and_an_approval_clears_it() {
        let (v, action) = with(
            &held(),
            &[old(false)],
            &unmeasured(),
            OnUnproven::Report,
            assemble,
        );
        assert_eq!(action, Action::AskPerson);
        assert_eq!(v["changed_old_tests"][0]["item"], "old-test:tests/t.py");
        let (_, action) = with(
            &held(),
            &[old(true)],
            &unmeasured(),
            OnUnproven::Report,
            assemble,
        );
        assert_eq!(action, Action::Stop);
    }

    #[test]
    fn a_broken_regression_repairs_naming_every_failing_test() {
        let broken = Regression {
            status: Status::Broken,
            failed: vec!["tests/a.py::test_x".into(), "tests/a.py::test_y".into()],
            ..held()
        };
        let (v, action) = with(
            &broken,
            &[old(false)],
            &unmeasured(),
            OnUnproven::Report,
            assemble,
        );
        assert_eq!(
            action,
            Action::Repair,
            "repair comes before asking a person"
        );
        // every failing test is named, one line each, in one round
        let lines = v["repair"].as_array().map_or(0, Vec::len);
        assert_eq!(lines, 2);
        let first = v["repair"][0].as_str().unwrap_or_default();
        let second = v["repair"][1].as_str().unwrap_or_default();
        assert!(
            first.contains("tests/a.py::test_x") && second.contains("tests/a.py::test_y"),
            "{first} / {second}"
        );
    }

    #[test]
    fn an_unknown_regression_or_failed_change_is_incomplete() {
        let unknown = Regression::unknown(vec!["bubblewrap is not available".into()]);
        let (_, action) = with(&unknown, &[], &unmeasured(), OnUnproven::Report, assemble);
        assert_eq!(action, Action::Incomplete);
        let failed = ChangeEvidence::Failed("coverage run died".into());
        let (_, action) = with(&held(), &[], &failed, OnUnproven::Report, assemble);
        assert_eq!(action, Action::Incomplete);
    }

    #[test]
    fn redundant_tests_and_unproven_functions_repair_as_configured() {
        let change = ChangeEvidence::Measured(json!({"status": "partly_proven",
            "functions": [{"file": "src/a.py", "function": "g", "line": 20, "status": "unproven"}],
            "proving_tests": [], "redundant_tests": [], "unknown_tests": [], "not_measured": []}));
        let (v, action) = with(&held(), &[], &change, OnUnproven::Report, assemble);
        assert_eq!((action, v["repair"].clone()), (Action::Stop, json!([])));
        let (v, action) = with(&held(), &[], &change, OnUnproven::Repair, assemble);
        assert_eq!(action, Action::Repair);
        assert!(
            v["repair"][0]
                .as_str()
                .unwrap_or_default()
                .contains("src/a.py:g (line 20)")
        );
        let redundant = ChangeEvidence::Measured(json!({"status": "proven", "functions": [],
            "proving_tests": [], "redundant_tests": [{"id": "tests/t.py::test_extra"}],
            "unknown_tests": [], "not_measured": []}));
        let (v, action) = with(&held(), &[], &redundant, OnUnproven::Report, assemble);
        assert_eq!(action, Action::Repair);
        assert!(
            v["repair"][0]
                .as_str()
                .unwrap_or_default()
                .contains("tests/t.py::test_extra")
        );
    }

    #[test]
    fn a_repair_names_every_item_of_one_kind_and_says_why() {
        let change = ChangeEvidence::Measured(json!({"status": "proven", "functions": [],
            "proving_tests": [],
            "redundant_tests": [
                {"id": "tests/t.py::test_a", "reason": "passes at the baseline"},
                {"id": "tests/t.py::test_b", "reason": "executes no changed line"}],
            "unknown_tests": [], "not_measured": []}));
        let (v, action) = with(&held(), &[], &change, OnUnproven::Report, assemble);
        assert_eq!(action, Action::Repair);
        let lines: Vec<&str> = v["repair"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(lines.len(), 2, "one round settles both: {lines:?}");
        assert!(lines[0].contains("test_a") && lines[0].contains("passes without your change"));
        assert!(lines[0].contains("make it fail on the old code, or remove it"));
        assert!(
            lines[1].contains("test_b") && lines[1].contains("runs none of the lines you changed")
        );
    }

    #[test]
    fn a_long_list_is_cut_and_the_rest_counted() {
        let many: Vec<Value> = (0..13)
            .map(|i| json!({"id": format!("tests/t.py::test_{i}"), "candidate_result": "failed"}))
            .collect();
        let change = ChangeEvidence::Measured(json!({"status": "none", "functions": [],
            "proving_tests": [], "redundant_tests": [], "failing_tests": many,
            "unknown_tests": [], "not_measured": []}));
        let (v, _) = with(&held(), &[], &change, OnUnproven::Report, assemble);
        let lines = v["repair"].as_array().map_or(0, Vec::len);
        assert_eq!(lines, 6);
        assert_eq!(v["repair"][5], "and 8 more of the same kind.");
    }

    #[test]
    fn not_verified_is_never_empty_and_names_the_setting() {
        for setting in [Setting::Cooperative, Setting::Enforced] {
            let change = unmeasured();
            let list = not_verified(&Limits {
                setting,
                link: &[],
                pytest: true,
                change: &change,
                config_note: None,
            });
            assert!(list.len() >= 4);
            assert!(list.iter().any(|l| l.contains(match setting {
                Setting::Cooperative => "Cooperative verification",
                Setting::Enforced => "Enforced verification",
            })));
            assert!(list.iter().any(|l| l.contains("new behaviour exists")));
        }
    }
}
