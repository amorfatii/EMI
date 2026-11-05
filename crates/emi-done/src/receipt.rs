//! The receipt (`emi-done-receipt-1.0`): the two evidence parts, the
//! changed old tests, the action, and what none of it establishes. The
//! change part arrives as a value ([`ChangeEvidence`]); everything else is
//! assembled from what this crate measured.

use std::path::Path;

use serde_json::{Value, json};

use crate::Setting;
use crate::baseline::Baseline;
use crate::candidate::Candidate;
use crate::change::ChangeEvidence;
use crate::config::{OnRedundant, OnUnproven};
use crate::oldtests::Entry;
use crate::regression::{Regression, Status};
use emi_kernel::decide::{self, Action};

pub const SCHEMA: &str = "emi-done-receipt-1.0";

/// What a decision is a function of: the two revisions, the configuration
/// and the pins it ran under, the Python environment, the setting, and the
/// version of EMI and of its decision rules. Two checks with the same
/// inputs must reach the same decision; `emi replay` checks that they do.
pub struct Inputs<'a> {
    pub baseline: &'a Baseline,
    pub candidate: &'a Candidate,
    pub setting: Setting,
    /// `None` when there is no configuration to read.
    pub config: Option<&'a crate::Config>,
    /// The environment digest, or why there is none.
    pub environment: Option<&'a Result<String, String>>,
    /// The digest of the laws the check held the candidate to.
    pub laws: Option<String>,
    /// What the person declared the change must keep (`preserve`).
    pub intent: Value,
}

/// The receipt's `inputs`.
pub fn inputs(i: &Inputs) -> Value {
    json!({
        "emi": env!("CARGO_PKG_VERSION"),
        "policy": emi_kernel::decide::POLICY,
        "baseline": i.baseline.sha,
        "candidate": i.candidate.content_id,
        "setting": i.setting.as_str(),
        "config": i.config.map(|c| emi_kernel::json::digest("emi-config", &c.to_value())),
        "pins": i.config.map(|c| c.pins.to_value()),
        "environment": i.environment.and_then(|e| e.as_ref().ok()),
        "laws": i.laws,
        "intent": i.intent,
    })
}

/// Everything of `receipt` that a replay must reproduce: all of it but
/// where it was computed and the ids derived from the rest.
fn identity(receipt: &Value) -> Value {
    let mut v = receipt.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("meta");
        m.remove("decision_id");
    }
    v
}

/// The id of a receipt's decision and the evidence it rests on.
pub fn decision_id(receipt: &Value) -> String {
    emi_kernel::json::digest("emi-decision", &identity(receipt))
}

/// Everything a receipt is made of.
pub struct Parts<'a> {
    pub repo: &'a Path,
    /// Everything the decision is a function of ([`inputs`]).
    pub inputs: Value,
    pub baseline: &'a Baseline,
    pub candidate: &'a Candidate,
    pub setting: Setting,
    /// `sandboxed`, or what ran instead; `unknown` when nothing ran.
    pub isolation: &'a str,
    pub regression: &'a Regression,
    pub old_tests: &'a [Entry],
    pub change: &'a ChangeEvidence,
    pub laws: &'a emi_python::proving::laws::LawEvidence,
    /// The behaviour comparison, when the person asked for it.
    pub preserved: Option<&'a crate::preserve::Preserved>,
    pub on_unproven: OnUnproven,
    pub on_redundant: OnRedundant,
    /// Environment faults beyond those the two evidence parts carry.
    pub faults: Vec<String>,
    pub not_verified: Vec<String>,
    /// The part of `not_verified` that is particular to this run.
    pub this_run: Vec<String>,
}

/// Parts that could not be measured for an environment reason.
fn faults(p: &Parts) -> Vec<String> {
    let mut out = p.faults.clone();
    // nondeterministic outcomes are their own row of the table
    if p.regression.status == Status::Unknown && p.regression.nondeterministic.is_empty() {
        out.push(format!(
            "regression evidence could not be measured: {}",
            p.regression
                .reasons
                .first()
                .map_or("no reason given", String::as_str)
        ));
    }
    out.extend(p.laws.fault.clone());
    let unknown_laws = p.laws.count("unknown");
    if unknown_laws > 0 {
        out.push(format!("{unknown_laws} law(s) could not be measured"));
    }
    // a break is established whatever else could not be measured, and a
    // suite the change stopped from starting cannot show the change either
    if p.regression.status != Status::Broken {
        out.extend(
            p.change
                .fault()
                .map(|f| format!("change evidence could not be measured: {f}")),
        );
    }
    out
}

/// The added tests that fail on the candidate: those the change evidence
/// names, and those the regression run saw fail, whichever measured them.
fn failing(p: &Parts) -> Vec<crate::change::Failing> {
    let mut out = p.change.facts().failing;
    for id in &p.regression.added_failing {
        if !out.iter().any(|f| &f.id == id) {
            out.push(crate::change::Failing {
                id: id.clone(),
                result: "failed".to_owned(),
            });
        }
    }
    out
}

fn failing_line(t: &crate::change::Failing) -> String {
    if t.result == "not collected" {
        format!(
            "{} cannot be collected on your change, so none of its tests run; fix the code or the test file.",
            t.id
        )
    } else {
        format!(
            "{} is a test you added and it fails on your change; fix the code or the test.",
            t.id
        )
    }
}

fn facts(p: &Parts) -> decide::Facts {
    let change = p.change.facts();
    decide::Facts {
        faults: faults(p),
        regression_broken: p.regression.status == Status::Broken,
        failing_tests: failing(p).len(),
        redundant_tests: change.redundant.len(),
        unexercised_functions: change.unexercised.len(),
        nondeterministic: p.regression.nondeterministic.len() + p.laws.count("nondeterministic"),
        laws_broken: p.laws.count("broken"),
        laws_unmet: p.laws.count("unmet"),
        divergences: p.preserved.map_or(0, crate::preserve::Preserved::count),
        unapproved: p.old_tests.iter().filter(|e| !e.approved).count(),
        ..decide::Facts::default()
    }
}

/// The configured treatment of each finding.
fn policy(p: &Parts) -> decide::Policy {
    decide::Policy {
        on_redundant: p.on_redundant,
        on_unexercised: p.on_unproven,
        ..decide::Policy::default()
    }
}

/// The action the parts lead to.
pub fn action_of(p: &Parts) -> Action {
    decide::decide(&policy(p), &facts(p))
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
    let laws = crate::laws::repair_lines(p.laws);
    let (broken_laws, unmet_laws): (Vec<String>, Vec<String>) = laws
        .into_iter()
        .partition(|l| l.contains("held at the baseline"));
    if r.status != Status::Broken && !broken_laws.is_empty() {
        return itemize(&broken_laws, String::clone);
    }
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
        out.extend(itemize(&broken_laws, String::clone));
        return out;
    }
    let failing = failing(p);
    let diverged = p
        .preserved
        .map(crate::preserve::repair_lines)
        .unwrap_or_default();
    if !failing.is_empty() || !unmet_laws.is_empty() || !diverged.is_empty() {
        let mut out = itemize(&failing, failing_line);
        out.extend(itemize(&unmet_laws, String::clone));
        out.extend(itemize(&diverged, String::clone));
        return out;
    }
    if p.on_redundant == OnRedundant::Repair && !change.redundant.is_empty() {
        return itemize(&change.redundant, redundant_line);
    }
    if p.on_unproven == OnUnproven::Repair {
        return itemize(&change.unexercised, |f| {
            format!("No proving test runs {f}; add a test that fails without your change.")
        });
    }
    Vec::new()
}

/// The change evidence with, on each changed function, the laws that
/// execute what the change wrote in it.
fn with_laws(mut change: Value, laws: &emi_python::proving::laws::LawEvidence) -> Value {
    if let Some(functions) = change["functions"].as_array_mut() {
        for f in functions {
            let key = format!(
                "{}::{}",
                f["file"].as_str().unwrap_or(""),
                f["function"].as_str().unwrap_or("")
            );
            f["laws"] = json!(laws.checked.get(&key).cloned().unwrap_or_default());
        }
    }
    change
}

/// Assemble the receipt and return it with its action.
pub fn assemble(p: &Parts) -> (Value, Action) {
    let action = action_of(p);
    let repair: Vec<String> = if action == Action::Repair {
        repair_lines(p)
    } else {
        Vec::new()
    };
    let mut receipt = json!({
        "schema": SCHEMA,
        "policy": emi_kernel::decide::POLICY,
        "input_id": emi_kernel::json::digest("emi-inputs", &p.inputs),
        "inputs": p.inputs,
        "baseline": {"sha": p.baseline.sha, "source": p.baseline.source},
        "candidate": {"id": p.candidate.id, "content_id": p.candidate.content_id},
        "assurance": {"setting": p.setting.as_str(), "isolation": p.isolation},
        "regression": p.regression.to_value(),
        "changed_old_tests": p.old_tests.iter().map(Entry::to_value).collect::<Vec<_>>(),
        "change": with_laws(p.change.to_value(), p.laws),
        "laws": p.laws.to_json(),
        "preserve": p.preserved.map(crate::preserve::Preserved::to_value),
        "action": action.as_str(),
        "repair": repair,
        "not_verified": p.not_verified,
        "this_run": p.this_run,
    });
    receipt["decision_id"] = json!(decision_id(&receipt));
    // where it was computed: not part of what a replay reproduces
    receipt["meta"] = json!({"repo": p.repo});
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
        "The baseline's tests are taken as correct. They ran twice under the same pins; a test whose outcome differed is quarantined, and an outcome that depends on something the pins do not hold (the clock, the network, another process) can still differ."
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
            added_failing: Vec::new(),
            quarantined: Vec::new(),
            nondeterministic: Vec::new(),
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
        with_policy(
            regression,
            old_tests,
            change,
            (on_unproven, OnRedundant::Report),
            f,
        )
    }

    fn with_policy<R>(
        regression: &Regression,
        old_tests: &[Entry],
        change: &ChangeEvidence,
        (on_unproven, on_redundant): (OnUnproven, OnRedundant),
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
        let laws = emi_python::proving::laws::LawEvidence::default();
        f(&Parts {
            laws: &laws,
            preserved: None,
            inputs: json!({"test": true}),
            repo: Path::new("/r"),
            baseline: &baseline,
            candidate: &candidate,
            setting: Setting::Cooperative,
            isolation: "sandboxed",
            regression,
            old_tests,
            change,
            on_unproven,
            on_redundant,
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
            "policy",
            "input_id",
            "inputs",
            "decision_id",
            "meta",
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
        assert_eq!(v["meta"]["repo"], "/r");
    }

    #[test]
    fn the_decision_id_covers_the_evidence_and_not_where_it_ran() {
        let (v, _) = with(&held(), &[], &unmeasured(), OnUnproven::Report, assemble);
        assert_eq!(v["decision_id"], json!(decision_id(&v)));
        let mut elsewhere = v.clone();
        elsewhere["meta"]["repo"] = json!("/somewhere/else");
        assert_eq!(decision_id(&elsewhere), decision_id(&v));
        let mut other = v.clone();
        other["regression"]["tests_run"] = json!(99);
        assert_ne!(decision_id(&other), decision_id(&v));
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
    fn failing_added_tests_repair_whichever_run_saw_them_fail() {
        // the regression run saw an added test fail and nothing measured
        // the change: still a repair, never a stop
        let mut seen = held();
        seen.added_failing = vec!["tests/n.py::test_new".into()];
        let (v, action) = with(&seen, &[], &unmeasured(), OnUnproven::Report, assemble);
        assert_eq!(action, Action::Repair);
        assert_eq!(
            v["repair"],
            json!([
                "tests/n.py::test_new is a test you added and it fails on your change; fix the code or the test."
            ])
        );
        // both saw it: named once; a file that does not collect says so
        let change = ChangeEvidence::Measured(json!({"status": "none", "functions": [],
            "proving_tests": [], "redundant_tests": [], "unknown_tests": [], "not_measured": [],
            "failing_tests": [{"id": "tests/n.py::test_new", "candidate_result": "failed"},
                              {"id": "tests/b.py", "candidate_result": "not collected"}]}));
        let (v, _) = with(&seen, &[], &change, OnUnproven::Report, assemble);
        assert_eq!(v["repair"].as_array().map(Vec::len), Some(2), "{v}");
        assert_eq!(
            v["repair"][1],
            "tests/b.py cannot be collected on your change, so none of its tests run; fix the code or the test file."
        );
    }

    #[test]
    fn a_break_is_repaired_even_when_the_change_could_not_be_measured() {
        let mut broken = Regression::unknown(vec!["the tests do not start: ImportError".into()]);
        broken.status = Status::Broken;
        let failed = ChangeEvidence::Failed("listing the tests exited 4".into());
        let (v, action) = with(&broken, &[], &failed, OnUnproven::Report, assemble);
        assert_eq!(action, Action::Repair);
        assert_eq!(
            v["repair"],
            json!(["Regression broken: the tests do not start: ImportError"])
        );
    }

    #[test]
    fn redundant_tests_and_unexercised_functions_repair_as_configured() {
        let change = ChangeEvidence::Measured(json!({"status": "partly_exercised",
            "functions": [{"file": "src/a.py", "function": "g", "line": 20, "status": "unexercised"}],
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
        let redundant = ChangeEvidence::Measured(json!({"status": "exercised", "functions": [],
            "proving_tests": [], "redundant_tests": [{"id": "tests/t.py::test_extra"}],
            "unknown_tests": [], "not_measured": []}));
        // a test that passes before and after may guard what must stay:
        // stated, not sent back, unless the project asks for that
        let (v, action) = with(&held(), &[], &redundant, OnUnproven::Report, assemble);
        assert_eq!((action, v["repair"].clone()), (Action::Stop, json!([])));
        let strict = (OnUnproven::Report, OnRedundant::Repair);
        let (v, action) = with_policy(&held(), &[], &redundant, strict, assemble);
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
        let change = ChangeEvidence::Measured(json!({"status": "exercised", "functions": [],
            "proving_tests": [],
            "redundant_tests": [
                {"id": "tests/t.py::test_a", "reason": "passes at the baseline"},
                {"id": "tests/t.py::test_b", "reason": "executes no changed line"}],
            "unknown_tests": [], "not_measured": []}));
        let strict = (OnUnproven::Report, OnRedundant::Repair);
        let (v, action) = with_policy(&held(), &[], &change, strict, assemble);
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
