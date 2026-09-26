//! `emi backtest`: gate every consecutive pair of first-parent commits
//! touching the scope and report the verdict curve per headroom — the
//! calibration step before enforcing a gate.

use std::path::PathBuf;

use emi_ledger::pyfmt::{g, repr};
use emi_scan::Probe;
use rayon::prelude::*;
use serde_json::{Map, Value, json};

use crate::change::{Request, build_change};
use crate::snapshot::Result;

#[derive(Clone, Debug)]
pub struct Backtest {
    pub repo: PathBuf,
    pub source: String,
    pub probe: Probe,
    pub commits: usize,
    /// Where history is read from (the oracle always used `HEAD`).
    pub head: String,
    pub allowance: f64,
    pub headrooms: Vec<f64>,
    pub tolerate_syntax_errors: bool,
}

/// Newest `commits` first-parent commits touching the scope, oldest first.
fn history(bt: &Backtest) -> Result<Vec<String>> {
    let filter = if matches!(bt.source.as_str(), "." | "") {
        ".".to_owned()
    } else {
        format!("{}/", bt.source.trim_end_matches('/'))
    };
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&bt.repo)
        .args([
            "log",
            "--format=%H",
            "--first-parent",
            &format!("-{}", bt.commits),
            "--end-of-options",
            &bt.head,
            "--",
            &filter,
        ])
        .output()
        .map_err(|e| format!("git log --format=%H --first-parent failed: {e}"))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(300)
            .collect::<String>();
        return Err(format!(
            "git log --format=%H --first-parent failed: {detail}"
        ));
    }
    let mut shas: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    shas.reverse();
    Ok(shas)
}

fn evaluate(bt: &Backtest, old: &str, new: &str) -> Value {
    let short = |s: &str| s.chars().take(12).collect::<String>();
    let mut req = Request::new(&bt.repo, &bt.source, old, new, bt.probe);
    req.tolerate_syntax_errors = bt.tolerate_syntax_errors;
    let outcome = build_change(&req).and_then(|change| {
        let report = emi_ledger::review(&change)?;
        let mut verdicts = Map::new();
        for h in &bt.headrooms {
            let gate = emi_ledger::guardrail(&report, bt.allowance, Some(*h), 5)?;
            verdicts.insert(repr(*h), gate["decision"].clone());
        }
        Ok(json!({"old": short(old), "new": short(new), "verdicts": verdicts,
                  "burden": report["burden_summary"], "matched": report["matched"]["classification"]}))
    });
    outcome.unwrap_or_else(|e| {
        json!({"old": short(old), "new": short(new), "error": e.chars().take(300).collect::<String>()})
    })
}

/// Run the backtest; returns the oracle's JSON payload.
pub fn run(bt: &Backtest) -> Result<Value> {
    let shas = history(bt)?;
    let pairs: Vec<(&String, &String)> = shas.iter().zip(shas.iter().skip(1)).collect();
    let rows: Vec<Value> = pairs
        .par_iter()
        .map(|(old, new)| evaluate(bt, old, new))
        .collect();
    let mut summary = Map::new();
    for h in &bt.headrooms {
        let mut counts = Map::new();
        for row in &rows {
            let decision = row["verdicts"]
                .get(repr(*h))
                .and_then(Value::as_str)
                .unwrap_or("ERROR");
            let n = counts.get(decision).and_then(Value::as_u64).unwrap_or(0);
            counts.insert(decision.to_owned(), json!(n + 1));
        }
        summary.insert(repr(*h), Value::Object(counts));
    }
    Ok(
        json!({"pairs": rows.len(), "allowance": bt.allowance, "headrooms": bt.headrooms,
              "summary": summary, "rows": rows}),
    )
}

/// The oracle's human table.
pub fn table(payload: &Value) -> String {
    let allowance = payload["allowance"].as_f64().unwrap_or(0.0);
    let headrooms: Vec<f64> = payload["headrooms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    let mut lines = vec![format!(
        "pairs: {}  allowance: {}",
        payload["pairs"],
        g(allowance)
    )];
    for h in &headrooms {
        let counts = &payload["summary"][repr(*h)];
        let cells: Vec<String> = ["pass", "warn", "block", "ERROR"]
            .iter()
            .map(|k| {
                format!(
                    "{k} {}",
                    counts.get(*k).and_then(Value::as_u64).unwrap_or(0)
                )
            })
            .collect();
        lines.push(format!("headroom {:>8}: {}", g(*h), cells.join("  ")));
    }
    for row in payload["rows"].as_array().into_iter().flatten() {
        let new = row["new"].as_str().unwrap_or("");
        if let Some(error) = row.get("error").and_then(Value::as_str) {
            lines.push(format!(
                "{new} ERROR {}",
                error.chars().take(80).collect::<String>()
            ));
            continue;
        }
        let letters: Vec<String> = headrooms
            .iter()
            .map(|h| {
                row["verdicts"][repr(*h)]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(1)
                    .collect::<String>()
                    .to_uppercase()
            })
            .collect();
        lines.push(format!(
            "{new} {} {}",
            letters.join("/"),
            row["burden"].as_str().unwrap_or("")
        ));
    }
    lines.join("\n") + "\n"
}
