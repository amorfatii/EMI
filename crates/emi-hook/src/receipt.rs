//! Reading what `emi done` returned.
//!
//! The hook is the receipt's consumer, so it checks the receipt instead of
//! trusting it: an unknown schema, an action that disagrees with the exit
//! code, or a receipt that contradicts its own action is `incomplete` and is
//! shown to the person. The text a person reads is rendered here from the
//! JSON receipt, so one run of the check serves both the proposer and the
//! person, and the first line of a NOT VERIFIED message stays under the
//! hook's control.

use serde_json::Value;

use crate::decision::{Action, DoneResult};

/// Current schema; the previous additive form remains readable.
pub const SCHEMA: &str = "emi-done-receipt-1.0";

/// Items listed before the rest are only counted.
const LIST_LIMIT: usize = 5;

fn action_for_exit(code: i32) -> Option<Action> {
    match code {
        0 => Some(Action::Stop),
        3 => Some(Action::Repair),
        4 => Some(Action::AskPerson),
        5 => Some(Action::Incomplete),
        _ => None,
    }
}

fn action_named(name: &str) -> Option<Action> {
    match name {
        "stop" => Some(Action::Stop),
        "repair" => Some(Action::Repair),
        "ask_person" => Some(Action::AskPerson),
        "incomplete" => Some(Action::Incomplete),
        _ => None,
    }
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Interpret one run of `emi done --format json`: its exit code (`None` when
/// it was killed by a signal) and what it printed.
pub fn interpret(exit: Option<i32>, stdout: &str, stderr: &str) -> DoneResult {
    let Some(code) = exit else {
        return DoneResult::incomplete("emi done was killed by a signal before it finished");
    };
    if code == 2 {
        return DoneResult::incomplete(format!(
            "emi done rejected its arguments or configuration: {}",
            first_line(stderr)
        ));
    }
    let Some(expected) = action_for_exit(code) else {
        return DoneResult::incomplete(format!(
            "emi done exited with unexpected code {code}: {}",
            first_line(stderr)
        ));
    };
    parse(expected, stdout).unwrap_or_else(|why| {
        DoneResult::incomplete(format!("emi done's receipt cannot be trusted: {why}"))
    })
}

fn string_list(v: &Value) -> Option<Vec<String>> {
    v.as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn unapproved_items(receipt: &Value) -> Option<Vec<String>> {
    let Some(tests) = receipt.get("changed_old_tests") else {
        return Some(Vec::new());
    };
    let mut items = Vec::new();
    for test in tests.as_array()? {
        let approved = test
            .get("approved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let item = test.get("item")?.as_str()?;
        if !approved {
            items.push(item.to_owned());
        }
    }
    Some(items)
}

fn parse(exit_action: Action, stdout: &str) -> Result<DoneResult, String> {
    let receipt: Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        if stdout.trim().is_empty() {
            "emi done printed nothing on stdout".to_owned()
        } else {
            format!("stdout is not JSON ({e})")
        }
    })?;
    let named = receipt["action"].as_str().and_then(action_named);
    let repair = string_list(&receipt["repair"]);
    let not_verified = string_list(&receipt["not_verified"]);
    let approve = unapproved_items(&receipt);
    // receipts written before the field existed asked only for approvals
    let ask = if receipt.get("ask").is_none() {
        Some(Vec::new())
    } else {
        string_list(&receipt["ask"])
    };
    let held = receipt["regression"]["status"].as_str() == Some("held");
    let checks = [
        (
            matches!(
                receipt["schema"].as_str(),
                Some("emi-done-receipt-0.1" | "emi-done-receipt-0.4" | SCHEMA)
            ),
            "the receipt schema is not supported",
        ),
        (named.is_some(), "the action is missing or unknown"),
        (
            named == Some(exit_action),
            "the action disagrees with the exit code",
        ),
        (
            repair.is_some(),
            "the repair list is missing or not a list of strings",
        ),
        (
            not_verified.is_some(),
            "the not_verified list is missing or not a list of strings",
        ),
        (
            approve.is_some(),
            "changed_old_tests entries need a string item",
        ),
        (
            named != Some(Action::Stop) || held,
            "the action is stop but regression is not held",
        ),
        (
            named != Some(Action::Repair) || repair.as_ref().is_some_and(|r| !r.is_empty()),
            "the action is repair but there are no repair lines",
        ),
        (ask.is_some(), "the ask list is not a list of strings"),
        (
            named != Some(Action::AskPerson)
                || approve.as_ref().is_some_and(|a| !a.is_empty())
                || ask.as_ref().is_some_and(|a| !a.is_empty()),
            "the action is ask_person but it names nothing to approve or decide",
        ),
    ];
    if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
        return Err((*why).to_owned());
    }
    Ok(DoneResult {
        action: exit_action,
        repair: repair.unwrap_or_default(),
        summary: render(&receipt, exit_action, &not_verified.unwrap_or_default()),
        approve: approve.unwrap_or_default(),
        ask: ask.unwrap_or_default(),
        evidence: evidence(&receipt, exit_action),
    })
}

/// Which rule the receipt's action rests on, in the order of the action
/// table: what the journal counts interventions by.
fn rule(receipt: &Value, action: Action) -> &'static str {
    let any = |v: &Value| v.as_array().is_some_and(|a| !a.is_empty());
    let change = &receipt["change"];
    match action {
        Action::Stop => "none",
        Action::Incomplete => "incomplete",
        Action::AskPerson => "changed_old_test",
        Action::Repair if receipt["regression"]["status"] == "broken" => "regression",
        Action::Repair if any(&change["failing_tests"]) => "failing_test",
        Action::Repair if any(&change["redundant_tests"]) => "redundant_test",
        Action::Repair => "unexercised",
    }
}

/// The receipt in counts, for the journal.
fn evidence(receipt: &Value, action: Action) -> Value {
    let change = &receipt["change"];
    let count = |v: &Value| v.as_array().map_or(0, Vec::len);
    let functions = |status: &str| {
        change["functions"]
            .as_array()
            .map_or(0, |f| f.iter().filter(|x| x["status"] == status).count())
    };
    serde_json::json!({
        "kind": rule(receipt, action),
        "regression": receipt["regression"]["status"],
        "tests_run": receipt["regression"]["tests_run"],
        "change": change["status"],
        "functions_exercised": functions("exercised"),
        "functions_unexercised": functions("unexercised"),
        "functions_unknown": functions("unknown"),
        "proving_tests": count(&change["proving_tests"]),
        "redundant_tests": count(&change["redundant_tests"]),
        "failing_tests": count(&change["failing_tests"]),
        "changed_old_tests": count(&receipt["changed_old_tests"]),
    })
}

/// `a, b, c (and 2 more)`.
fn listed(items: &[&str]) -> String {
    let shown = items
        .iter()
        .take(LIST_LIMIT)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(LIST_LIMIT) {
        0 => shown,
        more => format!("{shown} (and {more} more)"),
    }
}

fn strings(v: &Value) -> Vec<&str> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn headline(receipt: &Value, action: Action) -> String {
    let baseline = &receipt["baseline"];
    let sha = baseline["sha"].as_str().unwrap_or("unknown");
    let source = baseline["source"].as_str().unwrap_or("unknown");
    let setting = receipt["assurance"]["setting"]
        .as_str()
        .unwrap_or("unknown");
    let isolation = receipt["assurance"]["isolation"]
        .as_str()
        .unwrap_or("unknown");
    let name = match action {
        Action::Stop => "stop",
        Action::Repair => "repair",
        Action::AskPerson => "ask_person",
        Action::Incomplete => "incomplete",
    };
    format!(
        "EMI receipt ({name}): baseline {} from {source}; {setting} verification, {isolation}",
        sha.chars().take(10).collect::<String>()
    )
}

fn regression_lines(receipt: &Value) -> Vec<String> {
    let regression = &receipt["regression"];
    let status = regression["status"].as_str().unwrap_or("not reported");
    let mut line = format!("Regression evidence: {status}");
    if let Some(n) = regression["tests_run"].as_u64() {
        line.push_str(&format!(", {n} tests run"));
    }
    let mut lines = vec![line];
    for (key, label) in [
        ("failed", "failed"),
        ("missing", "missing"),
        ("reasons", "because"),
    ] {
        let items = strings(&regression[key]);
        if !items.is_empty() {
            lines.push(format!("  {label}: {}", listed(&items)));
        }
    }
    lines
}

fn changed_old_test_lines(receipt: &Value) -> Vec<String> {
    let Some(tests) = receipt["changed_old_tests"]
        .as_array()
        .filter(|t| !t.is_empty())
    else {
        return Vec::new();
    };
    let described: Vec<String> = tests
        .iter()
        .map(|t| {
            let path = t["path"].as_str().unwrap_or("?");
            let kind = t["kind"].as_str().unwrap_or("changed");
            let state = if t["approved"].as_bool() == Some(true) {
                "approved"
            } else {
                "awaiting approval"
            };
            format!("{path} ({kind}, {state})")
        })
        .collect();
    let refs: Vec<&str> = described.iter().map(String::as_str).collect();
    vec![format!("Changed old tests: {}", listed(&refs))]
}

fn exercised_counts(change: &Value) -> Option<(usize, usize)> {
    let functions = change["functions"].as_array()?;
    let exercised = functions
        .iter()
        .filter(|f| f["status"].as_str() == Some("exercised"))
        .count();
    Some((exercised, functions.len()))
}

fn change_lines(receipt: &Value) -> Vec<String> {
    let change = &receipt["change"];
    let mut line = format!(
        "Change evidence: {}",
        change["status"].as_str().unwrap_or("not reported")
    );
    if let Some((exercised, total)) = exercised_counts(change).filter(|(_, total)| *total > 0) {
        line.push_str(&format!(
            ", {exercised} of {total} changed functions exercised"
        ));
    }
    let mut lines = vec![line];
    let redundant: Vec<&str> = change["redundant_tests"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["id"].as_str()).collect())
        .unwrap_or_default();
    if !redundant.is_empty() {
        lines.push(format!("  redundant tests: {}", listed(&redundant)));
    }
    let not_measured = strings(&change["not_measured"]);
    if !not_measured.is_empty() {
        lines.push(format!("  not measured: {}", listed(&not_measured)));
    }
    lines
}

fn render(receipt: &Value, action: Action, not_verified: &[String]) -> Vec<String> {
    let mut lines = vec![headline(receipt, action)];
    lines.extend(regression_lines(receipt));
    lines.extend(changed_old_test_lines(receipt));
    lines.extend(change_lines(receipt));
    // what this run in particular did not establish is read every time;
    // the limits every receipt has are counted, not repeated
    let particular = strings(&receipt["this_run"]);
    for item in particular.iter().take(LIST_LIMIT * 2) {
        lines.push(format!("Note: {item}"));
    }
    lines.push(match not_verified.len().saturating_sub(particular.len()) {
        1 => "1 standing limit applies (emi done --full lists it).".to_owned(),
        n => format!("{n} standing limits apply (emi done --full lists them)."),
    });
    lines
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

    fn receipt(action: &str) -> Value {
        json!({
            "schema": SCHEMA,
            "repo": "/r",
            "baseline": {"sha": "0123456789abcdef", "source": "session"},
            "candidate": {"id": "worktree", "content_id": "t"},
            "assurance": {"setting": "cooperative", "isolation": "unsandboxed (test)"},
            "regression": {"status": "held", "tests_run": 12, "failed": [], "missing": [], "reasons": []},
            "changed_old_tests": [],
            "change": {"status": "unexercised", "functions": [], "proving_tests": [],
                       "redundant_tests": [], "unknown_tests": [], "not_measured": []},
            "action": action,
            "repair": [],
            "not_verified": ["behaviour no test exercises", "the baseline has no configuration"],
            "this_run": ["the baseline has no configuration"]
        })
    }

    fn run(exit: i32, v: &Value) -> DoneResult {
        interpret(Some(exit), &v.to_string(), "")
    }

    #[test]
    fn a_stop_receipt_is_rendered_for_the_person() {
        let r = run(0, &receipt("stop"));
        assert_eq!(r.action, Action::Stop);
        let text = r.summary.join("\n");
        assert!(text.contains("baseline 0123456789 from session"), "{text}");
        assert!(
            text.contains("Regression evidence: held, 12 tests run"),
            "{text}"
        );
        assert!(text.contains("Change evidence: unexercised"), "{text}");
        // this run's note is shown; the standing limit is counted
        assert!(
            text.contains("Note: the baseline has no configuration"),
            "{text}"
        );
        assert!(text.contains("1 standing limit applies"), "{text}");
        assert!(!text.contains("behaviour no test exercises"), "{text}");
    }

    #[test]
    fn a_repair_receipt_carries_its_repair_lines() {
        let mut v = receipt("repair");
        v["regression"]["status"] = json!("broken");
        v["regression"]["failed"] = json!(["tests/test_a.py::test_x"]);
        v["repair"] = json!(["tests/test_a.py::test_x fails: expected 1, got 2"]);
        let r = run(3, &v);
        assert_eq!(r.action, Action::Repair);
        assert_eq!(
            r.repair,
            vec!["tests/test_a.py::test_x fails: expected 1, got 2"]
        );
        assert!(
            r.summary
                .join("\n")
                .contains("failed: tests/test_a.py::test_x")
        );
    }

    #[test]
    fn an_ask_person_receipt_lists_only_unapproved_items() {
        let mut v = receipt("ask_person");
        v["changed_old_tests"] = json!([
            {"item": "old-test:tests/a.py", "path": "tests/a.py", "kind": "modified", "approved": false},
            {"item": "old-test:tests/b.py", "path": "tests/b.py", "kind": "deleted", "approved": true}
        ]);
        let r = run(4, &v);
        assert_eq!(r.action, Action::AskPerson);
        assert_eq!(r.approve, vec!["old-test:tests/a.py"]);
        assert!(
            r.summary
                .join("\n")
                .contains("tests/b.py (deleted, approved)")
        );
    }

    #[test]
    fn an_ask_person_receipt_may_name_a_decision_instead_of_an_approval() {
        let mut v = receipt("ask_person");
        v["ask"] = json!(["The checks ran without a sandbox: accept, or check it elsewhere."]);
        let r = run(4, &v);
        assert_eq!(r.action, Action::AskPerson);
        assert!(r.approve.is_empty());
        assert_eq!(
            r.ask,
            vec!["The checks ran without a sandbox: accept, or check it elsewhere."]
        );
        let mut wrong = receipt("ask_person");
        wrong["ask"] = json!([1]);
        assert_eq!(run(4, &wrong).action, Action::Incomplete);
    }

    #[test]
    fn an_incomplete_receipt_stays_incomplete() {
        let mut v = receipt("incomplete");
        v["regression"]["status"] = json!("unknown");
        v["regression"]["reasons"] = json!(["the test command is not installed"]);
        let r = run(5, &v);
        assert_eq!(r.action, Action::Incomplete);
        assert!(
            r.summary
                .join("\n")
                .contains("the test command is not installed")
        );
    }

    #[test]
    fn untrustworthy_output_is_incomplete() {
        let cases: Vec<(&str, DoneResult)> = vec![
            ("garbage", interpret(Some(0), "this is not json", "")),
            ("empty", interpret(Some(0), "", "")),
            ("signal", interpret(None, "", "")),
            (
                "exit 1",
                interpret(Some(1), &receipt("stop").to_string(), "boom"),
            ),
            ("exit 7", interpret(Some(7), "", "")),
            ("usage", interpret(Some(2), "", "no .emi/config.json")),
        ];
        for (name, r) in cases {
            assert_eq!(r.action, Action::Incomplete, "{name}");
            assert!(r.repair.is_empty(), "{name}");
        }
    }

    #[test]
    fn a_receipt_that_disagrees_with_itself_is_incomplete() {
        let mut other_schema = receipt("stop");
        other_schema["schema"] = json!("emi-done-receipt-9.9");
        let mut unknown_action = receipt("stop");
        unknown_action["action"] = json!("merge");
        let mut stop_on_broken = receipt("stop");
        stop_on_broken["regression"]["status"] = json!("broken");
        let mut stop_unknown = receipt("stop");
        stop_unknown["regression"]["status"] = json!("unknown");
        let mut no_not_verified = receipt("stop");
        no_not_verified
            .as_object_mut()
            .expect("object")
            .remove("not_verified");
        let empty_repair = receipt("repair");
        let mut ask_nothing = receipt("ask_person");
        ask_nothing["changed_old_tests"] = json!([]);
        let mut bad_item = receipt("ask_person");
        bad_item["changed_old_tests"] = json!([{"path": "x", "approved": false}]);
        let cases = [
            (0, other_schema),
            (0, unknown_action),
            (0, stop_on_broken),
            (0, stop_unknown),
            (0, no_not_verified),
            (3, empty_repair),
            (4, ask_nothing),
            (4, bad_item),
            (3, receipt("stop")),
            (0, receipt("repair")),
        ];
        for (exit, v) in cases {
            let r = run(exit, &v);
            assert_eq!(r.action, Action::Incomplete, "{v}");
            assert!(
                r.summary[0].contains("cannot be trusted"),
                "{:?}",
                r.summary
            );
        }
    }

    #[test]
    fn unknown_receipt_fields_are_tolerated() {
        let mut v = receipt("stop");
        v["future"] = json!({"anything": [1, 2]});
        assert_eq!(run(0, &v).action, Action::Stop);
    }

    #[test]
    fn long_lists_are_counted_not_dumped() {
        let mut v = receipt("repair");
        v["regression"]["status"] = json!("broken");
        v["regression"]["failed"] = json!((1..=9).map(|n| format!("t{n}")).collect::<Vec<_>>());
        v["repair"] = json!(["fix t1"]);
        let text = run(3, &v).summary.join("\n");
        assert!(text.contains("t5 (and 4 more)"), "{text}");
        assert!(!text.contains("t6"), "{text}");
    }
}
