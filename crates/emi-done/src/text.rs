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

fn old_test_lines(tests: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    if !tests.is_empty() {
        out.push("Changed old tests:".to_owned());
    }
    for t in tests {
        let path = t["path"].as_str().unwrap_or("?");
        let item = t["item"].as_str().unwrap_or("?");
        let kind = t["kind"].as_str().unwrap_or("changed");
        let restored = match t["restored_result"].as_str() {
            Some("passes") => "its original version passes on the candidate",
            Some("fails") => "its original version FAILS on the candidate",
            _ => "whether its original version passes is unknown",
        };
        if t["approved"] == true {
            out.push(format!("  {path} ({kind}; {restored}): approved"));
        } else {
            out.push(format!(
                "  {path} ({kind}; {restored}): needs approval: emi done --approve {item}"
            ));
        }
    }
    out
}

/// The receipt as text.
pub fn render(receipt: &Value) -> String {
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
    lines.push("Not verified:".to_owned());
    lines.extend(
        strs(&receipt["not_verified"])
            .into_iter()
            .map(|n| format!("  - {n}")),
    );
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
            "action": "stop", "repair": [], "not_verified": ["a thing"]})
    }

    #[test]
    fn it_leads_with_the_two_parts_in_plain_words() {
        let text = render(&receipt());
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("Regression: held (212 tests ran)."));
        assert_eq!(lines.next(), Some("New behaviour: not measured."));
        assert!(text.contains("Action: stop"));
        assert!(text.contains("  - a thing"));
    }

    #[test]
    fn a_changed_old_test_shows_the_exact_approve_argument() {
        let mut r = receipt();
        r["changed_old_tests"] = json!([
            {"item": "old-test:tests/t.py", "path": "tests/t.py", "kind": "modified", "restored_result": "passes", "approved": false},
            {"item": "old-test:tests/u.py", "path": "tests/u.py", "kind": "deleted", "restored_result": "fails", "approved": true}]);
        r["action"] = json!("ask_person");
        let text = render(&r);
        assert!(
            text.contains("emi done --approve old-test:tests/t.py"),
            "{text}"
        );
        assert!(
            text.contains(
                "tests/u.py (deleted; its original version FAILS on the candidate): approved"
            ),
            "{text}"
        );
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
