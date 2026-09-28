//! Refactoring tasks: the target must get simpler, measured, while the
//! other gates keep its behaviour (trusted tests restored, the test
//! inventory unchanged).
//!
//! Progress is cognitive complexity summed over the target files. Graded
//! erosion picks what to refactor (it ranks bug-prone functions, emi-lab
//! G2), but its complexity term saturates in very complex code, so it is
//! only required not to rise. Every source file the candidate adds or
//! modifies is measured with the targets, on both sides, so moving code
//! out of the target (into a new or existing module, or a helper under a
//! test directory) is not a reduction; only files that define tests are
//! left out. Extracting nested code into helpers does lower cognitive
//! complexity (nesting penalties reset), so a pass is evidence of
//! flatter code, not proof of a better design.
//!
//! The after side is the submitted tree, read from a snapshot no check
//! ran in. A file that cannot be read or parsed makes the gate
//! `unverified`: unmeasured code never counts as simpler.

use std::path::Path;

use emi_diff::hunks::text_at;
use emi_panel::Lang;
use emi_panel::explain::mass;
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
    // no universal threshold is justified: the contract states its own
    let min = obj
        .get("min_cognitive_reduction")
        .and_then(Value::as_f64)
        .ok_or("refactor.min_cognitive_reduction is required (a number in (0, 1])")?;
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
    /// Files that could not be measured, with why.
    unmeasured: Vec<String>,
}

/// A file with test definitions (by the guard's patterns): its complexity
/// is test code, not the refactored product.
fn defines_tests(path: &str, text: &str) -> bool {
    emi_guard::patterns::lang_of(path)
        .is_some_and(|l| emi_guard::patterns::count_tests(l, text) > 0)
}

impl Totals {
    fn add(&mut self, path: &str, text: &str) {
        let Some(lang) = Lang::of_path(path) else {
            return;
        };
        if !emi_panel::parses(lang, text.as_bytes()) {
            self.unmeasured.push(format!("{path}: syntax errors"));
            return;
        }
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
        json!({"cognitive": self.cognitive, "graded": self.graded, "files": self.files,
               "unmeasured": self.unmeasured})
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
    let Some(min) = r["min_cognitive_reduction"].as_f64() else {
        return json!({"state": "unverified", "detail": "refactor.min_cognitive_reduction missing"});
    };
    // the targets plus every source file the candidate touches
    let mut paths = target_files(repo, baseline, &targets);
    paths.extend(
        changed
            .iter()
            .filter(|(p, kind)| *kind != "delete" && Lang::of_path(p).is_some())
            .map(|(p, _)| p.clone()),
    );
    paths.sort();
    paths.dedup();
    let (mut before, mut after) = (Totals::default(), Totals::default());
    for path in &paths {
        let old = text_at(repo, Some(baseline), path);
        let new = match std::fs::read(workspace.join(path)) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(t) => Some(t),
                Err(_) => {
                    after.unmeasured.push(format!("{path}: not UTF-8"));
                    None
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                after.unmeasured.push(format!("{path}: {e}"));
                None
            }
        };
        // test code is not the product being refactored
        let is_test = |t: &Option<String>| t.as_deref().is_some_and(|t| defines_tests(path, t));
        if !targets.iter().any(|g| fnmatch(path, g)) && (is_test(&old) || is_test(&new)) {
            continue;
        }
        if let Some(t) = &old {
            before.add(path, t);
        }
        if let Some(t) = &new {
            after.add(path, t);
        }
    }
    if !after.unmeasured.is_empty() || !before.unmeasured.is_empty() {
        return json!({"state": "unverified",
                      "detail": "some changed files could not be measured; unmeasured code never counts as simpler",
                      "before": before.json(), "after": after.json()});
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
