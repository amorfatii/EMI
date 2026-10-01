//! The receipt in plain words, for the person who reads it. It leads with
//! the two evidence parts, then what needs a decision, then the action.

use serde_json::Value;

fn strs(v: &Value) -> Vec<&str> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn regression_line(r: &Value) -> String {
    let count = |n: usize, what: &str| format!("{n} {what}");
    match r["status"].as_str() {
        Some("held") => match r["tests_run"].as_u64() {
            Some(n) => format!("held ({n} tests ran)."),
            None => "held (the test count was not read from the output).".to_owned(),
        },
        Some("broken") => {
            let failed = strs(&r["failed"]);
            let missing = strs(&r["missing"]);
            let mut parts = Vec::new();
            if !failed.is_empty() {
                parts.push(count(failed.len(), "failed"));
            }
            if !missing.is_empty() {
                parts.push(count(missing.len(), "missing"));
            }
            let reasons = strs(&r["reasons"]);
            let first = failed.first().or(missing.first()).or(reasons.first());
            let detail = parts.join(", ");
            match first {
                Some(f) if detail.is_empty() => format!("broken ({f})."),
                Some(f) => format!("broken ({detail}; first: {f})."),
                None => "broken.".to_owned(),
            }
        }
        _ => match strs(&r["reasons"]).first() {
            Some(why) => format!("unknown ({why})."),
            None => "unknown.".to_owned(),
        },
    }
}

fn change_line(c: &Value) -> String {
    let functions = c["functions"].as_array().map_or(0, Vec::len);
    let proven = c["functions"]
        .as_array()
        .map_or(0, |f| f.iter().filter(|x| x["status"] == "proven").count());
    match c["status"].as_str() {
        Some("proven") => format!("proven ({functions} changed functions)."),
        Some("partly_proven") => {
            format!("partly proven ({proven} of {functions} changed functions).")
        }
        Some("unproven") => {
            format!("unproven ({functions} changed functions, none run by a proving test).")
        }
        Some("none") => "nothing to prove (no function changed).".to_owned(),
        _ => "not measured.".to_owned(),
    }
}

/// One changed file: what happened to it and what the person is asked.
fn old_test_line(t: &Value) -> String {
    let path = t["path"].as_str().unwrap_or("?");
    let item = t["item"].as_str().unwrap_or("?");
    let kind = t["kind"].as_str().unwrap_or("changed");
    if t["approved"] == true {
        return format!("  {path} ({kind}): approved");
    }
    let ask = format!("emi done --approve {item}");
    if t["type"] == "runner-config" {
        return format!("  {path} ({kind} test-runner configuration): needs approval: {ask}");
    }
    match t["restored_result"].as_str() {
        Some("fails") => format!(
            "  {path} ({kind}): this old test fails as it was at the baseline; the proposer \
             rewrote it. Approve with `{ask}`, or reject the change."
        ),
        Some("passes") => {
            format!("  {path} ({kind}; its baseline version passes): needs approval: {ask}")
        }
        _ => format!(
            "  {path} ({kind}; whether its baseline version passes is unknown): needs approval: {ask}"
        ),
    }
}

fn old_test_lines(tests: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    if !tests.is_empty() {
        out.push("Changed old tests and test-runner configuration:".to_owned());
    }
    out.extend(tests.iter().map(old_test_line));
    out
}

/// How much of the receipt's limits the text shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// What this run in particular did not establish, and how many
    /// standing limits apply: what a person reads every time.
    Short,
    /// Every limit, spelled out.
    Full,
}

/// The limits, as lines: all of them, or this run's and a count.
fn limit_lines(receipt: &Value, detail: Detail) -> Vec<String> {
    let all = strs(&receipt["not_verified"]);
    let particular = strs(&receipt["this_run"]);
    if detail == Detail::Full || !receipt["this_run"].is_array() {
        let mut lines = vec!["Not verified:".to_owned()];
        lines.extend(all.into_iter().map(|n| format!("  - {n}")));
        return lines;
    }
    let mut lines: Vec<String> = particular.iter().map(|n| format!("Note: {n}")).collect();
    lines.push(match all.len().saturating_sub(particular.len()) {
        1 => "1 standing limit applies (emi done --full lists it).".to_owned(),
        n => format!("{n} standing limits apply (emi done --full lists them)."),
    });
    lines
}

/// The receipt as text, short form.
pub fn render(receipt: &Value) -> String {
    render_with(receipt, Detail::Short)
}

/// The receipt as text.
pub fn render_with(receipt: &Value, detail: Detail) -> String {
    let mut lines = vec![
        format!("Regression: {}", regression_line(&receipt["regression"])),
        format!("New behaviour: {}", change_line(&receipt["change"])),
    ];
    lines.extend(old_test_lines(
        receipt["changed_old_tests"]
            .as_array()
            .map_or(&[][..], Vec::as_slice),
    ));
    let action = receipt["action"].as_str().unwrap_or("?");
    lines.push(format!("Action: {action}"));
    lines.extend(
        strs(&receipt["repair"])
            .into_iter()
            .map(|r| format!("  {r}")),
    );
    if action == "incomplete" {
        lines.push("  Nothing here is a pass: the check could not measure everything.".to_owned());
    }
    lines.push(format!(
        "Baseline {} ({}); candidate {}; {} verification, {}.",
        receipt["baseline"]["sha"]
            .as_str()
            .map_or("?", |s| &s[..s.len().min(10)]),
        receipt["baseline"]["source"].as_str().unwrap_or("?"),
        receipt["candidate"]["id"]
            .as_str()
            .map_or("?".to_owned(), |s| s[..s.len().min(10)].to_owned()),
        receipt["assurance"]["setting"].as_str().unwrap_or("?"),
        receipt["assurance"]["isolation"].as_str().unwrap_or("?"),
    ));
    lines.extend(limit_lines(receipt, detail));
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn receipt() -> Value {
        json!({
            "baseline": {"sha": "a".repeat(40), "source": "explicit"},
            "candidate": {"id": "worktree", "content_id": "b".repeat(40)},
            "assurance": {"setting": "cooperative", "isolation": "sandboxed"},
            "regression": {"status": "held", "tests_run": 212, "failed": [], "missing": [], "reasons": []},
            "changed_old_tests": [],
            "change": {"status": "unknown", "functions": [], "not_measured": ["x"]},
            "action": "stop", "repair": [],
            "not_verified": ["a standing thing", "another", "this run's thing"],
            "this_run": ["this run's thing"]})
    }

    #[test]
    fn it_leads_with_the_two_parts_in_plain_words() {
        let text = render(&receipt());
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("Regression: held (212 tests ran)."));
        assert_eq!(lines.next(), Some("New behaviour: not measured."));
        assert!(text.contains("Action: stop"));
        assert!(text.contains("Note: this run's thing"), "{text}");
        assert!(text.contains("2 standing limits apply"), "{text}");
        assert!(!text.contains("a standing thing"), "{text}");
        let full = render_with(&receipt(), Detail::Full);
        assert!(full.contains("  - a standing thing") && full.contains("  - this run's thing"));
    }

    #[test]
    fn changed_files_say_what_is_asked_and_show_the_exact_approve_argument() {
        let mut r = receipt();
        r["changed_old_tests"] = json!([
            {"item": "old-test:tests/t.py", "type": "old-test", "path": "tests/t.py", "kind": "modified", "restored_result": "passes", "approved": false},
            {"item": "old-test:tests/f.py", "type": "old-test", "path": "tests/f.py", "kind": "modified", "restored_result": "fails", "approved": false},
            {"item": "runner-config:pyproject.toml", "type": "runner-config", "path": "pyproject.toml", "kind": "modified", "restored_result": "unknown", "approved": false},
            {"item": "old-test:tests/u.py", "type": "old-test", "path": "tests/u.py", "kind": "deleted", "restored_result": "unknown", "approved": true}]);
        r["action"] = json!("ask_person");
        let text = render(&r);
        for want in [
            "tests/t.py (modified; its baseline version passes): needs approval: emi done --approve old-test:tests/t.py",
            "tests/f.py (modified): this old test fails as it was at the baseline; the proposer rewrote it. Approve with `emi done --approve old-test:tests/f.py`, or reject the change.",
            "pyproject.toml (modified test-runner configuration): needs approval: emi done --approve runner-config:pyproject.toml",
            "tests/u.py (deleted): approved",
        ] {
            assert!(text.contains(want), "{want}\n{text}");
        }
        assert!(!text.contains("--approve old-test:tests/u.py"));
    }

    #[test]
    fn broken_and_unknown_regressions_say_why() {
        let mut r = receipt();
        r["regression"] = json!({"status": "broken", "tests_run": 5, "failed": ["tests/a.py::t"], "missing": [], "reasons": []});
        assert!(render(&r).contains("Regression: broken (1 failed; first: tests/a.py::t)."));
        r["regression"] = json!({"status": "unknown", "tests_run": null, "failed": [], "missing": [], "reasons": ["no sandbox"]});
        r["action"] = json!("incomplete");
        let text = render(&r);
        assert!(text.contains("Regression: unknown (no sandbox)."), "{text}");
        assert!(text.contains("Nothing here is a pass"));
    }
}
