//! `emi replay`: compute a receipt again from nothing cached and say
//! whether it is the same decision on the same evidence. A replay that
//! differs means some input the pins do not hold (the clock, the network,
//! another process, a test that depends on its own history) reached the
//! outcome, and it names the parts of the receipt that differ.

use std::path::Path;

use serde_json::{Value, json};

use crate::{Error, Given, Request, Setting, candidate, receipt, state};

/// The parts of a receipt a replay does not reproduce: where it ran, and
/// the id derived from the rest.
const NOT_COMPARED: &[&str] = &["meta", "decision_id"];

/// What the replay found.
pub struct Replayed {
    pub same: bool,
    pub original: String,
    pub replayed: Value,
    /// The parts that differ, each with both values.
    pub differences: Vec<Value>,
}

impl Replayed {
    pub fn to_value(&self) -> Value {
        json!({"same": self.same, "decision_id": self.original,
               "replayed_decision_id": self.replayed["decision_id"],
               "differences": self.differences})
    }
}

fn field<'a>(v: &'a Value, path: &[&str]) -> Result<&'a str, Error> {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Usage(format!("the receipt has no {}", path.join("."))))
}

/// The setting `original` was computed in.
fn setting_of(original: &Value) -> Result<Setting, Error> {
    match field(original, &["assurance", "setting"])? {
        "cooperative" => Ok(Setting::Cooperative),
        "enforced" => Ok(Setting::Enforced),
        other => Err(Error::Usage(format!(
            "the receipt's setting {other} is unknown"
        ))),
    }
}

/// A fresh state directory holding only the person's approvals: no
/// recorded baseline run is reused.
fn fresh_state(real_root: &Path, repo: &Path, into: &Path) -> Result<(), Error> {
    let from = state::repo_dir(real_root, repo).join("approvals.jsonl");
    if from.exists() {
        let to = state::repo_dir(into, repo);
        std::fs::create_dir_all(&to)
            .and_then(|()| std::fs::copy(&from, to.join("approvals.jsonl")).map(|_| ()))
            .map_err(|e| Error::Usage(format!("cannot copy the approvals: {e}")))?;
    }
    Ok(())
}

/// Replay `original` in `repo`. `measure` supplies the change evidence, as
/// for `emi done`.
pub fn replay(
    repo: &Path,
    state_root: &Path,
    original: &Value,
    measure: impl FnOnce(&crate::ChangeContext) -> crate::ChangeEvidence,
) -> Result<Replayed, Error> {
    if field(original, &["schema"])? != receipt::SCHEMA {
        return Err(Error::Usage(format!(
            "only {} receipts can be replayed",
            receipt::SCHEMA
        )));
    }
    let id = field(original, &["decision_id"])?.to_owned();
    let baseline = field(original, &["baseline", "sha"])?;
    let candidate_id = field(original, &["candidate", "id"])?;
    let content_id = field(original, &["candidate", "content_id"])?;
    let spec = if candidate_id == crate::WORKTREE {
        let now = crate::content_id(repo, &candidate::Spec::Worktree)?;
        if now != content_id {
            return Err(Error::Usage(
                "the working tree changed since the receipt was made, so it cannot be replayed; \
                 commit the candidate and check it with --candidate"
                    .to_owned(),
            ));
        }
        candidate::Spec::Worktree
    } else {
        candidate::Spec::Ref(candidate_id.to_owned())
    };
    let scratch = emi_tree::snapshot::TempDir::new("emi-replay-").map_err(Error::Usage)?;
    let repo = crate::repo_root(repo)?;
    fresh_state(state_root, &repo, scratch.path())?;
    // the baseline is chosen the way the original chose it: a merge base is
    // found again, and a replay after the default branch moved says so
    let given = match field(original, &["baseline", "source"])? {
        "session" => Some((baseline, Given::Session)),
        "explicit" => Some((baseline, Given::Explicit)),
        _ => None,
    };
    let req = Request {
        repo: &repo,
        state_root: scratch.path(),
        baseline: given,
        candidate: spec,
        setting: setting_of(original)?,
        approve: &[],
        // the intent the original was checked under, never this machine's
        preserve: original["inputs"]["intent"]["preserve"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }),
    };
    let replayed = crate::done(&req, measure)?.receipt;
    let keys: std::collections::BTreeSet<&String> = [original, &replayed]
        .iter()
        .filter_map(|v| v.as_object())
        .flat_map(|m| m.keys())
        .filter(|k| !NOT_COMPARED.contains(&k.as_str()))
        .collect();
    let differences: Vec<Value> = keys
        .into_iter()
        .filter(|k| original.get(k.as_str()) != replayed.get(k.as_str()))
        .map(|k| json!({"part": k, "original": original[k], "replayed": replayed[k]}))
        .collect();
    Ok(Replayed {
        same: replayed["decision_id"] == id.as_str(),
        original: id,
        replayed,
        differences,
    })
}
