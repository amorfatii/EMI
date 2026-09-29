//! Test evidence from a saved `emi profile --format json` of the reported
//! commit: how many functions the tests would notice going wrong, and the
//! least verifiable of the most changed ones. Readiness is a state, so it
//! wears the status colors, always with an icon and a label.

use serde_json::{Value, json};

use super::page::{count, esc, section};

/// Readiness levels in display order: `(key, icon, label)`.
const LEVELS: &[(&str, &str, &str)] = &[
    ("unexecuted", "✕", "no test runs it"),
    ("weak", "!", "tests miss wrong changes"),
    ("executed", "·", "run, not sampled"),
    ("unknown", "?", "no evidence"),
    ("strong", "✓", "tests catch wrong changes"),
];

fn level(key: &str) -> (&'static str, &'static str) {
    LEVELS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map_or(("?", "unknown"), |(_, icon, label)| (*icon, *label))
}

/// The readiness data the report embeds, from `profile` if it is of `head`.
pub fn from_profile(profile: &Value, head: &str) -> Value {
    if profile["head"].as_str() != Some(head) {
        return json!({"state": "unverified",
                      "detail": format!("the profile is of {}, not of the reported commit {head}", profile["head"])});
    }
    let attention: Vec<Value> = profile["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| matches!(f["readiness"].as_str(), Some("unexecuted" | "weak")))
        .take(10)
        .cloned()
        .collect();
    json!({"state": "measured", "counts": profile["summary"]["readiness"],
           "functions": attention, "coverage": profile["coverage"]["format"],
           "mutation": !profile["mutation"].is_null()})
}

/// Functions no test runs, for the headline tile.
pub fn untested(r: &Value) -> Option<usize> {
    (r["state"] == "measured")
        .then(|| usize::try_from(r["counts"]["unexecuted"].as_u64().unwrap_or(0)).unwrap_or(0))
}

fn stack(counts: &Value) -> String {
    let total: u64 = LEVELS
        .iter()
        .map(|(k, _, _)| counts[*k].as_u64().unwrap_or(0))
        .sum();
    let mut bar =
        String::from(r#"<div class="stack" role="img" aria-label="Functions by readiness">"#);
    let mut legend = String::from(r#"<div class="legend">"#);
    for (key, icon, label) in LEVELS {
        let n = counts[*key].as_u64().unwrap_or(0);
        if n == 0 {
            continue;
        }
        bar += &format!(
            r#"<i class="s-{key}" style="flex:{n}" data-tip="{icon} {label}&#10;{}" tabindex="0"></i>"#,
            count(n as f64, "function", "functions")
        );
        legend += &format!(
            r#"<span class="key"><i class="dot s-{key}"></i>{icon} {label} <b>{n}</b></span>"#
        );
    }
    if total == 0 {
        return r#"<p class="empty">The profile has no functions.</p>"#.into();
    }
    format!("{bar}</div>{legend}</div>")
}

fn function_row(f: &Value) -> String {
    let key = f["readiness"].as_str().unwrap_or("unknown");
    let (icon, label) = level(key);
    let facts = &f["facts"];
    let run = if facts["coverage"].is_null() {
        "—".to_owned()
    } else {
        format!(
            "{}/{}",
            facts["coverage"]["executed"], facts["coverage"]["executable"]
        )
    };
    let caught = if facts["mutation"].is_null() {
        "not sampled".to_owned()
    } else {
        let t = &facts["mutation"];
        let hit = t["killed"].as_u64().unwrap_or(0) + t["timed_out"].as_u64().unwrap_or(0);
        format!("{hit} of {}", hit + t["survived"].as_u64().unwrap_or(0))
    };
    let tip = format!(
        "{}:{} {}\n{}",
        f["file"].as_str().unwrap_or(""),
        f["start"],
        f["function"].as_str().unwrap_or(""),
        f["advice"].as_str().unwrap_or("")
    );
    format!(
        r#"<li class="row" tabindex="0" data-tip="{}"><span class="status"><i class="dot s-{key}"></i>{icon} {label}</span><span class="path"><span class="dir">{}:{}</span> {}</span><span class="r">{}</span><span class="r">{run}</span><span class="r">{caught}</span></li>"#,
        esc(&tip),
        esc(f["file"].as_str().unwrap_or("")),
        f["start"],
        esc(f["function"].as_str().unwrap_or("")),
        facts["revisions"]
    )
}

/// The test evidence section.
pub fn render(r: &Value) -> String {
    let lede = "From emi profile at this commit: would the tests notice a wrong change in each function? Advisory; it gates nothing.";
    if r["state"] != "measured" {
        let note = format!(
            r#"<p class="empty">Not shown: {}.</p>"#,
            esc(r["detail"].as_str().unwrap_or(""))
        );
        return section("tests", "Test evidence", lede, &note);
    }
    let mut body = stack(&r["counts"]);
    let functions = r["functions"].as_array().cloned().unwrap_or_default();
    if !functions.is_empty() {
        body += r#"<div class="fn"><div class="row head"><span>Readiness</span><span>Function</span><span class="r">Changes</span><span class="r">Lines run</span><span class="r">Caught</span></div><ol class="rows">"#;
        for f in &functions {
            body += &function_row(f);
        }
        body += "</ol></div>";
    }
    section("tests", "Test evidence", lede, &body)
}
