//! Refactoring tasks: the target must get simpler, measured, while the
//! other gates keep its behaviour (trusted tests restored, the test
//! inventory unchanged).
//!
//! Progress is cognitive complexity summed over the target files. Graded
//! erosion picks what to refactor (it ranks bug-prone functions, emi-lab
//! G2), but its complexity term saturates in very complex code, so it is
//! only required not to rise. Every non-test file the candidate adds or
//! modifies is measured with the targets, on both sides, so moving code
//! out of the target (into a new module or an existing one) is not a
//! reduction; splitting a function evenly leaves both measures unchanged.

use std::path::Path;

use emi_diff::hunks::text_at;
use emi_panel::Lang;
use emi_panel::explain::mass;
use emi_scan::classify::{PathClass, classify};
use emi_scan::walk::fnmatch;
use serde_json::{Map, Value, json};

use crate::contract::Result;

/// Validate a contract's `refactor` section: `{"targets": [globs],
/// "min_cognitive_reduction": 0.1}`.
pub fn spec(v: Option<&Value>) -> Result<Option<Value>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(obj) = v.as_object() else {
        return Err("refactor must be an object".into());
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "targets" | "min_cognitive_reduction"))
    {
        return Err(format!("refactor: unknown key {k}"));
    }
    let targets: Vec<Value> = obj
        .get("targets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if targets.is_empty() || !targets.iter().all(Value::is_string) {
        return Err("refactor.targets must be a non-empty list of path globs".into());
    }
    let min = obj
        .get("min_cognitive_reduction")
        .map_or(Some(0.1), Value::as_f64)
        .ok_or("refactor.min_cognitive_reduction must be a number")?;
    if !(min > 0.0 && min <= 1.0) {
        return Err("refactor.min_cognitive_reduction must be in (0, 1]".into());
    }
    Ok(Some(
        json!({"targets": targets, "min_cognitive_reduction": min}),
    ))
}

/// Summed cognitive complexity and graded erosion of some files.
#[derive(Default)]
struct Totals {
    cognitive: u64,
    graded: f64,
    files: Map<String, Value>,
}

impl Totals {
    fn add(&mut self, path: &str, text: &str) {
        let Some(lang) = Lang::of_path(path) else {
            return;
        };
        let fns = emi_panel::functions(lang, text.as_bytes());
        let cognitive: u64 = fns.iter().map(|f| u64::from(f.cognitive)).sum();
        let graded: f64 = fns
            .iter()
            .map(|f| mass(f.sloc, f.cognitive, f.halstead_volume).total)
            .sum();
        self.cognitive += cognitive;
        self.graded += graded;
        self.files.insert(
            path.to_owned(),
            json!({"cognitive": cognitive, "graded": graded, "functions": fns.len()}),
        );
    }

    fn json(&self) -> Value {
        json!({"cognitive": self.cognitive, "graded": self.graded, "files": self.files})
    }
}

fn is_target(path: &str, targets: &[String]) -> bool {
    targets.iter().any(|g| fnmatch(path, g))
}

/// Files of the tree at `rev` matching `targets`.
fn target_files(repo: &Path, rev: &str, targets: &[String]) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", "-z", "--name-only", rev])
        .output();
    out.map(|o| {
        o.stdout
            .split(|b| *b == 0)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .filter(|p| is_target(p, targets))
            .collect()
    })
    .unwrap_or_default()
}

/// The refactor gate. `workspace` holds the candidate as the checks saw
/// it; `changed` are its `(path, kind)` changes from the baseline.
pub fn gate(
    contract: &Value,
    repo: &Path,
    baseline: &str,
    workspace: &Path,
    changed: &[(String, &'static str)],
) -> Value {
    let r = &contract["refactor"];
    if r.is_null() {
        return json!({"state": "not_required"});
    }
    let targets: Vec<String> = r["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let min = r["min_cognitive_reduction"].as_f64().unwrap_or(0.1);
    // the targets plus every non-test source file the candidate touches
    let mut paths = target_files(repo, baseline, &targets);
    paths.extend(
        changed
            .iter()
            .filter(|(p, kind)| {
                *kind != "delete" && Lang::of_path(p).is_some() && classify(p) != PathClass::Test
            })
            .map(|(p, _)| p.clone()),
    );
    paths.sort();
    paths.dedup();
    let (mut before, mut after) = (Totals::default(), Totals::default());
    for path in &paths {
        if let Some(text) = text_at(repo, Some(baseline), path) {
            before.add(path, &text);
        }
        if let Ok(text) = std::fs::read_to_string(workspace.join(path)) {
            after.add(path, &text);
        }
    }
    if before.cognitive == 0 {
        return json!({"state": "unverified",
                      "detail": "the refactor targets have no measured cognitive complexity at the baseline",
                      "before": before.json()});
    }
    let reduction = 1.0 - after.cognitive as f64 / before.cognitive as f64;
    let graded_rose = after.graded > before.graded + 1e-9;
    let state = if reduction + 1e-12 < min {
        "fail"
    } else if graded_rose {
        "needs_review"
    } else {
        "pass"
    };
    json!({"state": state, "cognitive_reduction": reduction, "required": min,
           "graded_change": after.graded - before.graded,
           "before": before.json(), "after": after.json()})
}
