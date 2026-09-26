//! PR-comment Markdown for a gated review: verdict, per-dimension
//! margins, the components responsible, and scope facts, in well under
//! 60 lines, with links to the files.

use serde_json::Value;

const DIMENSIONS: [(&str, &str); 3] = [("E", "erosion"), ("V", "redundancy"), ("C", "coupling")];

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn signed(x: f64) -> String {
    if x.abs() < 0.05 {
        "0".into()
    } else {
        format!("{x:+.1}")
    }
}

fn interval(v: &Value) -> String {
    match (num(&v[0]), num(&v[1])) {
        (Some(a), Some(b)) if (a - b).abs() < 0.05 => signed(a),
        (Some(a), Some(b)) => format!("{} … {}", signed(a), signed(b)),
        _ => "unavailable".into(),
    }
}

/// Link for a scope file; `link_base` is e.g.
/// `https://github.com/o/r/blob/<sha>` and `source` the scope directory.
fn link(file: &str, source: &str, link_base: Option<&str>) -> String {
    let path = if source.is_empty() || source == "." {
        file.to_owned()
    } else {
        format!("{}/{file}", source.trim_end_matches('/'))
    };
    match link_base {
        Some(base) => format!("[`{path}`]({}/{path})", base.trim_end_matches('/')),
        None => format!("`{path}`"),
    }
}

fn verdict(report: &Value, out: &mut Vec<String>) {
    let gate = &report["guardrail"];
    let decision = gate["decision"].as_str().unwrap_or("unavailable");
    let icon = match decision {
        "pass" => "✅",
        "warn" => "⚠️",
        "block" => "⛔",
        _ => "❔",
    };
    out.push(format!("### {icon} EMI: {decision}"));
    out.push(String::new());
    out.extend(
        gate["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    out.push(String::new());
    out.push("| Dimension | Burden Δ (proven … possible) | Margin to allowance |".into());
    out.push("|---|---|---|".into());
    for (key, name) in DIMENSIONS {
        let delta = interval(&report["burden_change"][key]["delta"]);
        let margin = num(&gate["margin"][key]["proven_excess"]).map_or("—".into(), |m| {
            if m.abs() < 0.05 {
                "0.0".into()
            } else {
                format!("{:.1}", -m)
            }
        });
        out.push(format!("| {key} {name} | {delta} | {margin} |"));
    }
}

fn evidence(
    gate: &Value,
    source: &str,
    link_base: Option<&str>,
    max_rows: usize,
    out: &mut Vec<String>,
) {
    let rows: Vec<&Value> = gate["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .take(max_rows)
        .collect();
    if rows.is_empty() {
        return;
    }
    out.push(String::new());
    out.push(format!(
        "**Responsible** (next step: `{}`):",
        gate["next_step"].as_str().unwrap_or("")
    ));
    for e in rows {
        let file = e["id"].as_str().unwrap_or("");
        let what = if e["delta"].is_null() {
            "new component".to_owned()
        } else {
            format!(
                "{} {}",
                e["dimension"].as_str().unwrap_or(""),
                interval(&e["delta"])
            )
        };
        out.push(format!(
            "- {} — {what} ({})",
            link(file, source, link_base),
            e["kind"].as_str().unwrap_or("")
        ));
    }
}

fn scope(report: &Value, out: &mut Vec<String>) {
    let growth = &report["growth"];
    let unchanged = report["scope"]["unchanged_components"]
        .as_array()
        .map_or(0, Vec::len);
    let files = report["scope"]["expected_components"]
        .as_array()
        .map_or(0, Vec::len);
    let errors: usize = ["before", "after"]
        .iter()
        .map(|s| {
            report["collection_errors"][s]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum();
    out.push(String::new());
    out.push("<details><summary>Scope</summary>".into());
    out.push(String::new());
    out.push(format!(
        "{files} files ({unchanged} unchanged) · SLOC {} → {} ({}) · matched density: {}",
        num(&growth["before_sloc"]).unwrap_or(0.0),
        num(&growth["after_sloc"]).unwrap_or(0.0),
        signed(num(&growth["net_sloc"]).unwrap_or(0.0)),
        report["matched"]["classification"].as_str().unwrap_or("")
    ));
    if errors > 0 {
        out.push(format!(
            "⚠️ {errors} file(s) could not be measured; the scope is incomplete."
        ));
    }
    out.push(String::new());
    out.push("</details>".into());
}

/// Render the comment. `report` carries its `guardrail` verdict.
pub fn render(report: &Value, source: &str, link_base: Option<&str>, max_rows: usize) -> String {
    let mut out = Vec::new();
    verdict(report, &mut out);
    evidence(&report["guardrail"], source, link_base, max_rows, &mut out);
    scope(report, &mut out);
    out.push(String::new());
    out.push(format!(
        "<sub>emi {} · {} · policy {}</sub>",
        env!("CARGO_PKG_VERSION"),
        report["measurement_profile"].as_str().unwrap_or(""),
        report["guardrail"]["gate_policy_id"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect::<String>()
    ));
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_a_short_linked_comment() {
        let evidence: Vec<Value> = (0..20)
            .map(|i| json!({"id": format!("f{i}.py"), "dimension": "E", "delta": [3.0, 3.0], "kind": "surviving"}))
            .collect();
        let report = json!({
            "measurement_profile": "p",
            "guardrail": {"decision": "block", "reasons": ["absolute burden grew beyond allowance: E +60 (allowance 10)"],
                          "next_step": "shrink", "gate_policy_id": "abcdef0123456789",
                          "margin": {"E": {"proven_excess": 50.0}, "V": {"proven_excess": -10.0}, "C": {"proven_excess": -10.0}},
                          "evidence": evidence},
            "burden_change": {"E": {"delta": [60.0, 60.0]}, "V": {"delta": [0.0, 0.0]}, "C": {"delta": [-2.0, 5.0]}},
            "growth": {"before_sloc": 100.0, "after_sloc": 180.0, "net_sloc": 80.0},
            "scope": {"expected_components": ["a"], "unchanged_components": []},
            "collection_errors": {"before": [], "after": []},
            "matched": {"classification": "regression"}});
        let md = render(&report, "src", Some("https://example.org/blob/abc"), 8);
        assert!(md.starts_with("### ⛔ EMI: block"));
        assert!(md.contains("| C coupling | -2.0 … +5.0 |"));
        assert!(md.contains("[`src/f0.py`](https://example.org/blob/abc/src/f0.py)"));
        assert!(md.lines().count() < 60, "{} lines", md.lines().count());
        assert!(!md.contains("f8.py"), "evidence capped");
    }
}
