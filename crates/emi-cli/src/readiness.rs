//! The test evidence behind the functions a change touches, from a saved
//! `emi profile --format json` of the change's head: which of them the
//! tests would notice going wrong. Advisory; it gates nothing (whether it
//! should is emi-lab S9's question).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

/// Added lines per file, read between two commits through a clean view.
fn added_lines(
    repo: &Path,
    before: &str,
    after: &str,
) -> Result<BTreeMap<String, Vec<u32>>, String> {
    let view = emi_git::CleanView::of(repo)?;
    let files = emi_diff::hunks::changes(view.path(), before, Some(after))?;
    Ok(files
        .into_iter()
        .filter(|f| !f.added.is_empty())
        .map(|f| (f.path, f.added.into_iter().map(|(l, _)| l).collect()))
        .collect())
}

/// Order rows by how much they need attention.
fn rank(readiness: &str) -> u8 {
    match readiness {
        "unexecuted" => 0,
        "weak" => 1,
        "unknown" => 2,
        "executed" => 3,
        _ => 4,
    }
}

/// `{"state": ..., "functions": [...], "uncovered_files": [...]}` for the
/// change `before..after`, from `profile`.
pub fn changed(profile: &Value, repo: &Path, before: &str, after: &str) -> Value {
    if profile["head"].as_str() != Some(after) {
        return json!({"state": "unverified",
                      "detail": format!("the profile is of {}, not of the change's head {after}", profile["head"])});
    }
    let lines = match added_lines(repo, before, after) {
        Ok(l) => l,
        Err(e) => return json!({"state": "unverified", "detail": e}),
    };
    let entries = profile["functions"].as_array().cloned().unwrap_or_default();
    let mut touched: Vec<Value> = entries
        .iter()
        .filter(|f| {
            let (start, end) = (
                f["start"].as_u64().unwrap_or(0),
                f["end"].as_u64().unwrap_or(0),
            );
            lines
                .get(f["file"].as_str().unwrap_or(""))
                .is_some_and(|ls| ls.iter().any(|l| (start..=end).contains(&u64::from(*l))))
        })
        .cloned()
        .collect();
    touched.sort_by_key(|f| rank(f["readiness"].as_str().unwrap_or("")));
    let profiled: std::collections::BTreeSet<&str> =
        entries.iter().filter_map(|f| f["file"].as_str()).collect();
    let uncovered: Vec<&String> = lines
        .keys()
        .filter(|f| emi_panel::Lang::of_path(f).is_some() && !profiled.contains(f.as_str()))
        .collect();
    json!({"state": "measured", "functions": touched, "unprofiled_files": uncovered})
}

fn caught(f: &Value) -> String {
    let t = &f["facts"]["mutation"];
    if t.is_null() {
        return "not sampled".into();
    }
    let hit = t["killed"].as_u64().unwrap_or(0) + t["timed_out"].as_u64().unwrap_or(0);
    let all = hit + t["survived"].as_u64().unwrap_or(0);
    match t["survivors"]
        .as_array()
        .and_then(|s| s.first())
        .and_then(Value::as_str)
    {
        Some(missed) => format!("{hit} of {all} (missed {missed})"),
        None => format!("{hit} of {all}"),
    }
}

fn icon(readiness: &str) -> &'static str {
    match readiness {
        "strong" => "✅",
        "weak" | "unexecuted" => "⚠️",
        _ => "❔",
    }
}

/// The section for the PR comment.
pub fn markdown(section: &Value, out: &mut Vec<String>) {
    out.push(String::new());
    out.push(
        "<details open><summary>Test evidence for the changed functions (advisory)</summary>"
            .into(),
    );
    out.push(String::new());
    if section["state"] != "measured" {
        out.push(format!(
            "Not shown: {}.",
            section["detail"].as_str().unwrap_or("")
        ));
        out.push("</details>".into());
        return;
    }
    let functions = section["functions"].as_array().cloned().unwrap_or_default();
    if functions.is_empty() {
        out.push("The profile lists no function this change touches.".into());
    } else {
        out.push("| Function | Readiness | Lines run | Wrong changes caught |".into());
        out.push("|---|---|---|---|".into());
        for f in &functions {
            let cov = &f["facts"]["coverage"];
            let run = if cov.is_null() {
                "unknown".to_owned()
            } else {
                format!("{}/{}", cov["executed"], cov["executable"])
            };
            let readiness = f["readiness"].as_str().unwrap_or("");
            out.push(format!(
                "| `{}:{} {}` | {} {readiness} | {run} | {} |",
                f["file"].as_str().unwrap_or(""),
                f["start"],
                f["function"].as_str().unwrap_or(""),
                icon(readiness),
                caught(f).replace('|', "\\|")
            ));
        }
    }
    let unprofiled = section["unprofiled_files"].as_array().map_or(0, Vec::len);
    if unprofiled > 0 {
        out.push(String::new());
        out.push(format!(
            "{unprofiled} changed source file(s) have no function in the profile (profile with `--top 0`)."
        ));
    }
    out.push(String::new());
    out.push("Readiness says whether the tests would notice a wrong change there; it is not a bug prediction and gates nothing.".into());
    out.push("</details>".into());
}
