//! The authority store: `<authority>/<task_id>/` holds the frozen contract
//! revisions, `state.json` (cumulative budget, history) and the
//! append-only `evidence.jsonl`. It must live outside the candidate's
//! repository, so the agent under supervision cannot rewrite it through
//! its own change.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::contract::{self, Result};

pub struct Store {
    pub dir: PathBuf,
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Write via a temporary file and rename, so readers never see a torn file.
pub fn atomic_write(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let text = emi_ledger::pyfmt::dumps_pretty(value) + "\n";
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    emi_ledger::strict::loads(&text).map_err(|e| format!("{}: {e}", path.display()))
}

impl Store {
    /// The task's directory, refusing an authority inside the repository.
    pub fn open(authority: &Path, task_id: &str, repo: &Path) -> Result<Store> {
        if !contract::valid_task_id(task_id) {
            return Err(format!("invalid task_id {task_id:?}"));
        }
        std::fs::create_dir_all(authority).map_err(|e| format!("{}: {e}", authority.display()))?;
        let authority = canonical(authority);
        if authority.starts_with(canonical(repo)) {
            return Err(format!(
                "authority directory {} is inside the repository; it must be controller-owned and outside the candidate tree",
                authority.display()
            ));
        }
        Ok(Store {
            dir: authority.join(task_id),
        })
    }

    pub fn exists(&self) -> bool {
        self.dir.join("state.json").exists()
    }

    pub fn create(&self, contract: &Value) -> Result<()> {
        if self.exists() {
            return Err(format!(
                "task already exists at {}; revise it instead",
                self.dir.display()
            ));
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        self.write_contract(contract)?;
        let state = json!({
            "task_id": contract["task_id"], "current_revision": contract["revision"],
            "current_contract_id": contract["contract_id"],
            "cumulative": {"retries": 0, "time_s": 0.0},
            "history": [{"revision": contract["revision"], "contract_id": contract["contract_id"],
                         "actor": "controller", "reason": "initial freeze"}]});
        atomic_write(&self.dir.join("state.json"), &state)
    }

    fn contract_path(&self, revision: u64, id: &str) -> PathBuf {
        self.dir.join(format!(
            "contract-r{revision}-{}.json",
            &id[..16.min(id.len())]
        ))
    }

    fn write_contract(&self, contract: &Value) -> Result<()> {
        let id = contract["contract_id"].as_str().unwrap_or("");
        let rev = contract["revision"].as_u64().unwrap_or(0);
        atomic_write(&self.contract_path(rev, id), contract)
    }

    pub fn state(&self) -> Result<Value> {
        read(&self.dir.join("state.json"))
    }

    pub fn save_state(&self, state: &Value) -> Result<()> {
        atomic_write(&self.dir.join("state.json"), state)
    }

    /// The current contract, integrity-checked against its id and state.
    pub fn contract(&self) -> Result<Value> {
        let state = self.state()?;
        let id = state["current_contract_id"].as_str().unwrap_or("");
        let rev = state["current_revision"].as_u64().unwrap_or(0);
        let c = read(&self.contract_path(rev, id))?;
        contract::verify_integrity(&c)?;
        if c["contract_id"].as_str() != Some(id) {
            return Err("state names a different contract than the stored one".into());
        }
        Ok(c)
    }

    /// Record a revision: a new contract id, prior acceptances void.
    pub fn revise(&self, changes: &Value, actor: &str, reason: &str) -> Result<Value> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err("a revision needs an actor and a reason".into());
        }
        let old = self.contract()?;
        let mut core = old.clone();
        let Some(obj) = changes.as_object() else {
            return Err("changes must be an object".into());
        };
        for (k, v) in obj {
            if matches!(
                k.as_str(),
                "task_id"
                    | "schema_version"
                    | "contract_id"
                    | "revision"
                    | "predecessor"
                    | "repo"
                    | "baseline"
            ) {
                return Err(format!("{k} cannot be revised"));
            }
            core[k] = v.clone();
        }
        core["revision"] = json!(old["revision"].as_u64().unwrap_or(1) + 1);
        core["predecessor"] = old["contract_id"].clone();
        let new = contract::finalize(core);
        self.write_contract(&new)?;
        let mut state = self.state()?;
        state["current_revision"] = new["revision"].clone();
        state["current_contract_id"] = new["contract_id"].clone();
        if let Some(h) = state["history"].as_array_mut() {
            h.push(json!({"revision": new["revision"], "contract_id": new["contract_id"],
                          "actor": actor, "reason": reason, "changed": obj.keys().collect::<Vec<_>>()}));
        }
        self.save_state(&state)?;
        Ok(new)
    }

    /// Append one evidence row (`kind`: check_run, decision, completion).
    pub fn append(&self, kind: &str, row: &Value) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("evidence.jsonl"))
            .map_err(|e| e.to_string())?;
        let line = json!({"kind": kind, "at": emi_diff::trajectory::utc_now_iso(), "record": row});
        writeln!(file, "{line}").map_err(|e| e.to_string())
    }

    pub fn rows(&self, kind: &str) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("evidence.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r["kind"] == kind)
            .map(|r| r["record"].clone())
            .collect()
    }

    /// The latest decision about `candidate` under the current contract.
    pub fn latest_decision(&self, candidate: &str, contract_id: &str) -> Option<Value> {
        self.rows("decision")
            .into_iter()
            .rev()
            .find(|d| d["candidate_id"] == candidate && d["contract_id"] == contract_id)
    }
}
