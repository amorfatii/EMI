//! The authority store: `<authority>/<task_id>/` holds the frozen contract
//! revisions, `state.json` (the current revision and its history) and the
//! append-only `evidence.jsonl`. It must live outside the candidate's
//! repository, so the agent under supervision cannot rewrite it through
//! its own change.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::engine::contract::{self, Result};

pub struct Store {
    pub dir: PathBuf,
    /// The language whose test output the checks print, when one reads
    /// it; the engine itself reads only the forms every driver shares.
    pub driver: Option<&'static dyn emi_driver::Driver>,
}

/// An exclusive lock on a task's store, released when dropped. Appends to
/// the evidence and read-modify-write updates of `state.json` hold it, so
/// concurrent verifications neither interleave rows nor lose updates.
pub struct Lock(#[allow(dead_code)] std::fs::File);

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Write via a temporary file and rename, so readers never see a torn file.
pub fn atomic_write(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let text = emi_kernel::json::dumps_pretty(value) + "\n";
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    emi_kernel::strict::loads(&text).map_err(|e| format!("{}: {e}", path.display()))
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
            driver: None,
        })
    }

    /// The store, reading test output with `driver`.
    #[must_use]
    pub fn reading(mut self, driver: &'static dyn emi_driver::Driver) -> Store {
        self.driver = Some(driver);
        self
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

    fn lock(&self) -> Result<Lock> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("lock"))
            .map_err(|e| format!("lock {}: {e}", self.dir.display()))?;
        #[cfg(unix)]
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|e| format!("lock {}: {e}", self.dir.display()))?;
        Ok(Lock(file))
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
            // validated (and held-out files frozen) like at creation
            core[k] = contract::revised(k, v)?;
        }
        contract::rebind(&mut core)?;
        if obj.contains_key("execution") {
            // revising the execution policy re-pins the linked directories
            // as they are now: the controller's way to accept a new runner
            let repo = Path::new(old["repo"]["path"].as_str().unwrap_or("."));
            match crate::engine::deps::pin(repo, &core["execution"])? {
                Some(d) => core["dependencies"] = d,
                None => {
                    if let Some(m) = core.as_object_mut() {
                        m.remove("dependencies");
                    }
                }
            }
        }
        core["revision"] = json!(old["revision"].as_u64().unwrap_or(1) + 1);
        core["predecessor"] = old["contract_id"].clone();
        let new = contract::finalize(core);
        self.write_contract(&new)?;
        let history = json!({"revision": new["revision"], "contract_id": new["contract_id"],
                             "actor": actor, "reason": reason, "changed": obj.keys().collect::<Vec<_>>()});
        {
            let _lock = self.lock()?;
            let mut state = self.state()?;
            state["current_revision"] = new["revision"].clone();
            state["current_contract_id"] = new["contract_id"].clone();
            if let Some(h) = state["history"].as_array_mut() {
                h.push(history);
            }
            self.save_state(&state)?;
        }
        Ok(new)
    }

    /// Append one evidence row (`kind`: check_run, decision, approval,
    /// completion): one write of the whole line, under the store's lock.
    pub fn append(&self, kind: &str, row: &Value) -> Result<()> {
        let _lock = self.lock()?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("evidence.jsonl"))
            .map_err(|e| e.to_string())?;
        let mut line =
            json!({"kind": kind, "at": emi_exec::clock::utc_now_iso(), "record": row}).to_string();
        line.push('\n');
        file.write_all(line.as_bytes()).map_err(|e| e.to_string())
    }

    /// Every row of `kind`, oldest first. A line that is not a record
    /// means the log is damaged: an error, never a silently shorter log
    /// (a lost row could make an older decision look like the latest).
    pub fn rows(&self, kind: &str) -> Result<Vec<Value>> {
        Ok(self.rows_at(kind)?.into_iter().map(|(r, _)| r).collect())
    }

    /// [`Store::rows`] with the time each row was appended.
    fn rows_at(&self, kind: &str) -> Result<Vec<(Value, String)>> {
        let text = match std::fs::read_to_string(self.dir.join("evidence.jsonl")) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("evidence.jsonl: {e}")),
        };
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let row: Value = serde_json::from_str(line).map_err(|_| {
                format!(
                    "evidence.jsonl line {} is not a record; the evidence log is damaged",
                    n + 1
                )
            })?;
            if row["kind"] == kind {
                let at = row["at"].as_str().unwrap_or_default().to_owned();
                out.push((row["record"].clone(), at));
            }
        }
        Ok(out)
    }

    /// The latest decision about `candidate` under the current contract.
    pub fn latest_decision(&self, candidate: &str, contract_id: &str) -> Result<Option<Value>> {
        Ok(self
            .latest_decision_at(candidate, contract_id)?
            .map(|(d, _)| d))
    }

    /// [`Store::latest_decision`] and when it was recorded.
    pub fn latest_decision_at(
        &self,
        candidate: &str,
        contract_id: &str,
    ) -> Result<Option<(Value, String)>> {
        Ok(self
            .rows_at("decision")?
            .into_iter()
            .rev()
            .find(|(d, _)| d["candidate_id"] == candidate && d["contract_id"] == contract_id))
    }
}
