//! Preserve intent: the person says the change must keep the code's
//! behaviour (a refactoring), and the law that follows needs no writing:
//! for every function the change modifies, the baseline and the candidate
//! must answer the same. The inputs are the ones the baseline's tests pass
//! the function, plus inputs drawn from its annotations; each input runs
//! against both versions in separate processes under the same pins. An
//! input where they answer differently, or where the candidate no longer
//! finishes in time, is a counterexample sent back to the proposer.
//!
//! This is evidence on those inputs, never a proof of equivalence: the
//! receipt says how many inputs were compared and which functions could not
//! be (instance methods, async or nested functions, signatures that
//! changed).

use std::path::Path;
use std::time::Duration;

use emi_python::drift::{self, InView};
use emi_python::sandbox::Runner;
use serde_json::{Value, json};

use crate::Config;
use crate::Error;
use crate::baseline::Baseline;
use crate::candidate::Candidate;

const FILE: &str = "intent.json";

/// What the person declared about the change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Intent {
    /// Keep the behaviour of the functions the change modifies.
    pub preserve: bool,
    /// Only those under these paths (repository relative); empty for all.
    pub paths: Vec<String>,
}

impl Intent {
    pub fn to_value(&self) -> Value {
        if self.preserve {
            json!({"preserve": self.paths})
        } else {
            Value::Null
        }
    }

    fn covers(&self, file: &str) -> bool {
        self.paths.is_empty()
            || self
                .paths
                .iter()
                .any(|p| file == p || file.starts_with(&format!("{}/", p.trim_end_matches('/'))))
    }
}

/// The intent recorded for a repository on this machine.
pub fn read(repo_dir: &Path) -> Result<Intent, Error> {
    let path = repo_dir.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Intent::default()),
        Err(e) => return Err(Error::Usage(format!("{}: {e}", path.display()))),
    };
    let v = emi_kernel::strict::loads(&text)
        .map_err(|e| Error::Usage(format!("{}: {e}", path.display())))?;
    let paths = v["preserve"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .ok_or_else(|| Error::Usage(format!("{} is not an intent record", path.display())))?;
    Ok(Intent {
        preserve: true,
        paths,
    })
}

/// Record `intent` for the repository (none clears it).
pub fn write(repo_dir: &Path, intent: &Intent) -> Result<(), Error> {
    let path = repo_dir.join(FILE);
    if !intent.preserve {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::Usage(format!("{}: {e}", path.display())))
            }
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(repo_dir)
        .and_then(|()| {
            std::fs::write(
                &path,
                emi_kernel::json::dumps_pretty(&intent.to_value()) + "\n",
            )
        })
        .map_err(|e| Error::Usage(format!("{}: {e}", path.display())))
}

/// One input on which the candidate answered differently.
#[derive(Clone, Debug, PartialEq)]
pub struct Divergence {
    pub file: String,
    pub function: String,
    pub input: Value,
    pub before: Value,
    pub now: Value,
}

/// The receipt's `preserve` object.
#[derive(Clone, Debug, Default)]
pub struct Preserved {
    pub divergences: Vec<Divergence>,
    /// Divergences beyond those listed.
    pub more: u64,
    /// Per function: inputs compared and whether the baseline answered
    /// each the same twice (observed determinism).
    pub functions: Vec<Value>,
    /// Functions the comparison could not check, with why.
    pub not_checked: Vec<String>,
    pub reasons: Vec<String>,
    pub counts: Value,
}

impl Preserved {
    pub fn count(&self) -> usize {
        self.divergences.len() + usize::try_from(self.more).unwrap_or(usize::MAX)
    }

    /// Exclude functions whose behavioural change is already proven by a
    /// proving test (`status == "exercised"` in `change`), when the intent
    /// applies to the whole change rather than an explicit path list.
    pub fn exclude_proven(&mut self, change: &crate::change::ChangeEvidence, intent: &Intent) {
        if !intent.paths.is_empty() {
            return;
        }
        let crate::change::ChangeEvidence::Measured(v) = change else {
            return;
        };
        if v["proving_tests"].as_array().is_none_or(Vec::is_empty) {
            return;
        }
        let proven: std::collections::BTreeSet<(&str, &str)> = v["functions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|f| f["status"] == "exercised")
            .filter_map(|f| Some((f["file"].as_str()?, f["function"].as_str()?)))
            .collect();
        if proven.is_empty() {
            return;
        }
        self.divergences
            .retain(|d| !proven.contains(&(d.file.as_str(), d.function.as_str())));
        self.functions.retain(|f| {
            let file = f["file"].as_str().unwrap_or("");
            let func = f["function"].as_str().unwrap_or("");
            !proven.contains(&(file, func))
        });
    }

    /// Whether `(file, function)` was compared on at least one input,
    /// answered deterministically, and had no divergence.
    pub fn verified(&self, file: &str, function: &str) -> bool {
        if self.more > 0
            || self
                .divergences
                .iter()
                .any(|d| d.file == file && d.function == function)
        {
            return false;
        }
        self.functions.iter().any(|f| {
            f["file"].as_str() == Some(file)
                && f["function"].as_str() == Some(function)
                && f["status"].as_str() == Some("compared")
                && f["compared"].as_u64().unwrap_or(0) > 0
                && f["deterministic"].as_bool() == Some(true)
        })
    }

    pub fn to_value(&self) -> Value {
        json!({
            "divergences": self.divergences.iter().map(|d| json!({
                "file": d.file, "function": d.function, "input": d.input,
                "before": d.before, "now": d.now})).collect::<Vec<_>>(),
            "more_divergences": self.more,
            "functions": self.functions,
            "not_checked": self.not_checked,
            "reasons": self.reasons,
            "counts": self.counts,
        })
    }
}

fn shown(v: &Value) -> String {
    match (
        v.get("returned"),
        v.get("self"),
        v.get("mutated"),
        v.get("raised"),
        v.get("timeout"),
    ) {
        (Some(r), Some(s), Some(m), _, _) => {
            format!("returned {r} (leaving self = {s}, mutated = {m})")
        }
        (Some(r), Some(s), None, _, _) => format!("returned {r} (leaving self = {s})"),
        (Some(r), None, Some(m), _, _) => format!("returned {r} (leaving mutated = {m})"),
        (Some(r), None, None, _, _) => format!("returned {r}"),
        (_, _, _, Some(e), _) => format!("raised {}", e.as_str().unwrap_or("?")),
        (_, _, _, _, Some(t)) => format!("did not finish within {t} s"),
        _ => v.to_string(),
    }
}

/// One repair line per listed divergence.
pub fn repair_lines(p: &Preserved) -> Vec<String> {
    p.divergences
        .iter()
        .map(|d| {
            format!(
                "{}::{} answers differently from the baseline for the input {}: it {} and now {}. \
                 The person asked for its behaviour to be kept.",
                d.file,
                d.function,
                d.input,
                shown(&d.before),
                shown(&d.now)
            )
        })
        .collect()
}

/// Compare the functions the change modifies on the baseline and the
/// candidate.
pub fn measure(
    repo: &Path,
    view: &Path,
    (config, baseline, cand): (&Config, &Baseline, &Candidate),
    intent: &Intent,
    cache: Option<&emi_python::proving::ListingCache>,
) -> Preserved {
    let mut env = config.pins.env();
    env.push((
        "PYTHONPATH".to_owned(),
        emi_kernel::pins::Pins::python_path(&config.python_path),
    ));
    let runner = |rev| Runner {
        repo,
        objects: Some(view),
        rev,
        link: &config.link,
        env: &env,
        timeout: Duration::from_secs(1800),
        network: false,
    };
    let (base, head) = (
        InView {
            runner: runner(&baseline.sha),
            objects: view,
        },
        InView {
            runner: runner(&cand.rev),
            objects: view,
        },
    );
    let commands = [config.tests.command.clone()];
    let req = drift::Request {
        repo: view,
        base: baseline.sha.clone(),
        head: cand.rev.clone(),
        commands: &commands,
        call_timeout_s: f64::from(config.preserve.call_timeout_s),
        base_side: &base,
        head_side: &head,
        generated_examples: config.preserve.generated_examples,
        module_roots: &config.python_path,
        paths: &intent.paths,
        cache,
    };
    match drift::measure(&req) {
        Ok(report) => read_report(&report, intent),
        Err(why) => not_compared(&why),
    }
}

/// The evidence when the behaviour could not be compared at all.
pub fn not_compared(why: &str) -> Preserved {
    Preserved {
        reasons: vec![format!("behaviour could not be compared: {why}")],
        ..Preserved::default()
    }
}

/// The parts of a drift report the receipt uses, for the functions the
/// intent covers.
fn read_report(report: &Value, intent: &Intent) -> Preserved {
    let mut p = Preserved {
        reasons: report["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        counts: report["counts"].clone(),
        ..Preserved::default()
    };
    for f in report["functions"].as_array().into_iter().flatten() {
        let (file, name) = (
            f["file"].as_str().unwrap_or(""),
            f["name"].as_str().unwrap_or(""),
        );
        if !intent.covers(file) {
            continue;
        }
        if f["status"] != "compared" {
            p.not_checked.push(format!(
                "{file}::{name}: {}",
                f["status"].as_str().unwrap_or("not compared")
            ));
        }
        for d in f["drifts"].as_array().into_iter().flatten() {
            p.divergences.push(Divergence {
                file: file.to_owned(),
                function: name.to_owned(),
                input: d["input"].clone(),
                before: d["old"].clone(),
                now: d["new"].clone(),
            });
        }
        p.more += f["more_drifts"].as_u64().unwrap_or(0);
        p.functions.push(json!({
            "file": file, "function": name, "status": f["status"],
            "inputs": f["inputs"], "compared": f["compared"],
            "deterministic": f["compared"].as_u64().unwrap_or(0) > 0
                && f["unstable"].as_u64() == Some(0),
        }));
    }
    for s in report["skipped"].as_array().into_iter().flatten() {
        let file = s["file"].as_str().unwrap_or("");
        if intent.covers(file) {
            p.not_checked.push(format!(
                "{file}::{}: {}",
                s["name"].as_str().unwrap_or(""),
                s["reason"].as_str().unwrap_or("skipped")
            ));
        }
    }
    p
}

/// What the receipt does not establish about the kept behaviour.
pub fn limits(p: &Preserved) -> Vec<String> {
    let compared: u64 = p
        .functions
        .iter()
        .filter_map(|f| f["compared"].as_u64())
        .sum();
    let mut out = vec![format!(
        "Behaviour was compared with the baseline's on {compared} input(s) ({} generated from \
         annotations); other inputs are not covered, so this is not a proof of equivalence.",
        p.counts["generated"].as_u64().unwrap_or(0)
    )];
    out.extend(p.not_checked.iter().map(|n| format!("Not compared: {n}.")));
    out.extend(p.reasons.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_becomes_divergences_for_the_paths_the_intent_covers() {
        let report = json!({
            "reasons": [],
            "counts": {"generated": 3},
            "functions": [
                {"file": "src/a.py", "name": "f", "status": "compared", "inputs": 4,
                 "compared": 4, "unstable": 0, "more_drifts": 0,
                 "drifts": [{"input": {"args": [1], "kwargs": {}},
                             "old": {"returned": 1}, "new": {"raised": "ValueError"}}]},
                {"file": "other/b.py", "name": "g", "status": "compared", "inputs": 1,
                 "compared": 1, "unstable": 0, "more_drifts": 0,
                 "drifts": [{"input": 1, "old": {"returned": 1}, "new": {"returned": 2}}]}
            ],
            "skipped": [{"file": "src/a.py", "name": "C.m", "reason": "instance_method"}]
        });
        let intent = Intent {
            preserve: true,
            paths: vec!["src".into()],
        };
        let p = read_report(&report, &intent);
        assert_eq!(p.count(), 1);
        assert_eq!(p.functions[0]["deterministic"], true);
        assert_eq!(p.not_checked, ["src/a.py::C.m: instance_method"]);
        let line = &repair_lines(&p)[0];
        assert!(
            line.contains("it returned 1 and now raised ValueError"),
            "{line}"
        );
        assert!(limits(&p)[0].contains("on 4 input(s) (3 generated"));
    }

    #[test]
    fn the_intent_is_recorded_and_cleared() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read(dir.path()).ok(), Some(Intent::default()));
        let kept = Intent {
            preserve: true,
            paths: vec!["src/pkg".into()],
        };
        assert!(write(dir.path(), &kept).is_ok());
        assert_eq!(read(dir.path()).ok(), Some(kept));
        assert!(write(dir.path(), &Intent::default()).is_ok());
        assert_eq!(read(dir.path()).ok(), Some(Intent::default()));
    }
}
