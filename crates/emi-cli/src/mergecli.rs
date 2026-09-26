//! `emi sarif-merge`: one SARIF log from many tools' logs (ruff, ESLint,
//! Semgrep, CodeQL, clippy, emi), so CI uploads and reviews a single file.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use serde_json::{Value, json};

#[derive(Args)]
pub struct MergeArgs {
    /// SARIF files to merge.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Write the merged log here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

/// Identity of a result for deduplication within one tool.
fn result_key(r: &Value) -> String {
    let loc = &r["locations"][0]["physicalLocation"];
    let fp = r
        .get("partialFingerprints")
        .or_else(|| r.get("fingerprints"))
        .cloned()
        .unwrap_or(Value::Null);
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        r["ruleId"],
        loc["artifactLocation"]["uri"],
        loc["region"]["startLine"],
        r["message"]["text"],
        fp
    )
}

/// Merge runs by tool name: rules are unioned by id, results deduplicated.
pub fn merge(logs: &[Value]) -> Value {
    let mut runs: BTreeMap<String, (Value, Vec<Value>, HashSet<String>, BTreeMap<String, Value>)> =
        BTreeMap::new();
    for log in logs {
        for run in log["runs"].as_array().into_iter().flatten() {
            let tool = run["tool"]["driver"]["name"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned();
            let entry = runs
                .entry(tool)
                .or_insert_with(|| (run.clone(), Vec::new(), HashSet::new(), BTreeMap::new()));
            for rule in run["tool"]["driver"]["rules"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(id) = rule["id"].as_str() {
                    entry.3.entry(id.to_owned()).or_insert_with(|| rule.clone());
                }
            }
            for r in run["results"].as_array().into_iter().flatten() {
                if entry.2.insert(result_key(r)) {
                    entry.1.push(r.clone());
                }
            }
        }
    }
    let merged: Vec<Value> = runs
        .into_values()
        .map(|(mut run, results, _, rules)| {
            run["results"] = Value::Array(results);
            if !rules.is_empty() {
                run["tool"]["driver"]["rules"] = Value::Array(rules.into_values().collect());
            }
            // Rule indices refer to the per-file rule arrays; drop them.
            if let Some(rs) = run["results"].as_array_mut() {
                for r in rs {
                    if let Some(o) = r.as_object_mut() {
                        o.remove("ruleIndex");
                    }
                }
            }
            run
        })
        .collect();
    json!({"$schema": "https://json.schemastore.org/sarif-2.1.0.json", "version": "2.1.0", "runs": merged})
}

pub fn run(args: &MergeArgs) -> anyhow::Result<u8> {
    let mut logs = Vec::new();
    for path in &args.inputs {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        logs.push(
            serde_json::from_str::<Value>(&text)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
        );
    }
    let merged = serde_json::to_string_pretty(&merge(&logs))?;
    match &args.output {
        Some(p) => std::fs::write(p, merged)?,
        None => writeln!(std::io::stdout().lock(), "{merged}")?,
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_by_tool_and_dedupes() {
        let r = |line: u64| {
            json!({"ruleId": "E1", "message": {"text": "m"},
            "locations": [{"physicalLocation": {"artifactLocation": {"uri": "a.py"}, "region": {"startLine": line}}}]})
        };
        let a = json!({"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "ruff", "rules": [{"id": "E1"}]}}, "results": [r(1), r(2)]}]});
        let b = json!({"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "ruff", "rules": [{"id": "E1"}, {"id": "E2"}]}}, "results": [r(2), r(3)]},
                                                     {"tool": {"driver": {"name": "emi"}}, "results": [r(1)]}]});
        let m = merge(&[a, b]);
        let runs = m["runs"].as_array().cloned().unwrap_or_default();
        assert_eq!(runs.len(), 2);
        let ruff = runs
            .iter()
            .find(|x| x["tool"]["driver"]["name"] == "ruff")
            .cloned()
            .unwrap_or_default();
        assert_eq!(ruff["results"].as_array().map(Vec::len), Some(3));
        assert_eq!(
            ruff["tool"]["driver"]["rules"].as_array().map(Vec::len),
            Some(2)
        );
    }
}
