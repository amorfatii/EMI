//! Verify one candidate against the frozen contract and reduce everything
//! to a decision: `accepted`, `rejected`, `needs_review` or `incomplete`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use std::time::Duration;

use emi_exec::{Isolation, Spec, Status};
use emi_kernel::json::digest;
use emi_tree::snapshot;
use serde_json::{Value, json};

use crate::contract::Result;
use crate::store::Store;

/// What a contract without a test inventory cannot rule out.
pub const NO_INVENTORY: &str = "no test inventory: each check passes on its exit code alone, so code \
     that ends the test process with status 0 before the tests run would pass; configure \
     verification.inventory";

/// What is being verified.
#[derive(Clone, Debug)]
pub enum Candidate {
    Ref(String),
    /// Uncommitted work (never completable, only verifiable).
    Worktree,
}

pub const WORKTREE: &str = "worktree";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("emi-verify-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    emi_git::run(repo, args)
}

/// `(path, kind)` of every change from the baseline, renames split into
/// delete + add.
pub fn changes(
    repo: &Path,
    baseline: &str,
    candidate: &str,
) -> Result<Vec<(String, &'static str)>> {
    let raw = git(
        repo,
        &[
            "diff",
            "--no-textconv",
            "--no-ext-diff",
            "--name-status",
            "-z",
            "-M",
            baseline,
            candidate,
        ],
    )?;
    let tokens: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let status = tokens[i].clone();
        let letter = status.chars().next().unwrap_or(' ');
        match letter {
            'R' | 'C' if i + 2 < tokens.len() => {
                if letter == 'R' {
                    out.push((tokens[i + 1].clone(), "delete"));
                }
                out.push((tokens[i + 2].clone(), "add"));
                i += 3;
            }
            'A' | 'M' | 'D' | 'T' if i + 1 < tokens.len() => {
                let kind = match letter {
                    'A' => "add",
                    'D' => "delete",
                    _ => "modify",
                };
                out.push((tokens[i + 1].clone(), kind));
                i += 2;
            }
            _ => i += 1,
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn globs(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

/// Materialize `candidate` into `dest` and link the ignored dependency
/// directories (node_modules, .venv) the checks need. Returns the linked
/// sources, which a sandbox mounts back read-only inside the hidden
/// repository.
fn materialize(
    objects: &Path,
    repo: &Path,
    candidate: &str,
    dest: &Path,
    contract: &Value,
) -> Result<Vec<PathBuf>> {
    snapshot::materialize(objects, candidate, dest)?;
    snapshot::link_dependencies(repo, dest, &globs(&contract["execution"]["link"]))
}

/// Write `bytes` to `workspace/rel` without following links the candidate
/// may have planted: every directory on the way must be a real directory,
/// and whatever is at the destination is replaced, not written through.
fn write_beneath(workspace: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let target = workspace.join(rel);
    let mut dir = workspace.to_path_buf();
    let parts: Vec<_> = Path::new(rel).components().collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(format!("{rel}: {} is not a plain directory", dir.display()));
            }
            Ok(_) => {}
            Err(_) => std::fs::create_dir(&dir).map_err(|e| format!("{rel}: {e}"))?,
        }
    }
    if std::fs::symlink_metadata(&target).is_ok() {
        std::fs::remove_file(&target).map_err(|e| format!("{rel}: {e}"))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .and_then(|mut f| std::io::Write::write_all(&mut f, bytes))
        .map_err(|e| format!("{rel}: {e}"))
}

/// Trusted paths the candidate modified or deleted (restored before runs).
fn trusted_changes(trusted: &[String], changed: &[(String, &'static str)]) -> Vec<String> {
    changed
        .iter()
        .filter(|(p, kind)| {
            matches!(*kind, "modify" | "delete")
                && trusted.iter().any(|g| emi_kernel::glob::fnmatch(p, g))
        })
        .map(|(p, _)| p.clone())
        .collect()
}

/// Restore the baseline version of every trusted file the candidate
/// modified or deleted; files it added stay. Returns the restored paths.
fn restore_trusted(
    repo: &Path,
    baseline: &str,
    workspace: &Path,
    trusted: &[String],
    changed: &[(String, &'static str)],
) -> Result<Vec<String>> {
    let mut restored = Vec::new();
    for (path, kind) in changed {
        let is_trusted = trusted.iter().any(|g| emi_kernel::glob::fnmatch(path, g));
        if !is_trusted || !matches!(*kind, "modify" | "delete") {
            continue;
        }
        let bytes = git(repo, &["cat-file", "blob", &format!("{baseline}:{path}")])?;
        write_beneath(workspace, path, &bytes).map_err(|e| format!("restore {e}"))?;
        restored.push(path.clone());
    }
    Ok(restored)
}

/// Write the contract's held-out files into the workspace (they win over
/// any file of the same name the candidate wrote).
fn inject_held_out(workspace: &Path, held_out: &Value) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (dest, file) in held_out.as_object().into_iter().flatten() {
        write_beneath(
            workspace,
            dest,
            file["content"].as_str().unwrap_or("").as_bytes(),
        )
        .map_err(|e| format!("held-out {e}"))?;
        written.push(dest.clone());
    }
    Ok(written)
}

/// Everything needed to build a run workspace.
struct RunSetup<'a> {
    /// The clean view every git read goes through.
    objects: &'a Path,
    /// The real repository: linked dependencies come from it.
    repo: &'a Path,
    candidate: &'a str,
    baseline: &'a str,
    contract: &'a Value,
    changed: &'a [(String, &'static str)],
    /// The baseline's files matching the trusted globs.
    trusted_files: Vec<String>,
    /// Host paths a run must not read: the authority and the repository.
    hidden: Vec<PathBuf>,
}

/// What a run's sandbox mounts besides its workspace.
#[derive(Clone)]
struct Mounts {
    read_only: Vec<PathBuf>,
    hidden: Vec<PathBuf>,
}

impl RunSetup<'_> {
    /// A fresh workspace at `dest`: the candidate, trusted files restored,
    /// held-out files injected.
    fn prepare(&self, dest: &Path) -> Result<Mounts> {
        let linked = materialize(self.objects, self.repo, self.candidate, dest, self.contract)?;
        let v = &self.contract["verification"];
        restore_trusted(
            self.objects,
            self.baseline,
            dest,
            &globs(&v["trusted"]),
            self.changed,
        )?;
        let held_out = inject_held_out(dest, &v["held_out"])?;
        let mut read_only: Vec<PathBuf> = self
            .trusted_files
            .iter()
            .chain(&held_out)
            .map(|p| dest.join(p))
            // bubblewrap cannot bind onto a symbolic link, and refuses to
            // start: a trusted file that is one is restored but left
            // writable (its target, when trusted too, is still mounted)
            .filter(|p| {
                p.symlink_metadata()
                    .is_ok_and(|m| !m.file_type().is_symlink())
            })
            .collect();
        read_only.extend(linked);
        Ok(Mounts {
            read_only,
            hidden: self.hidden.clone(),
        })
    }

    /// The baseline, for its test inventory (nothing restored or injected).
    fn prepare_baseline(&self, dest: &Path) -> Result<Mounts> {
        let linked = materialize(self.objects, self.repo, self.baseline, dest, self.contract)?;
        Ok(Mounts {
            read_only: linked,
            hidden: self.hidden.clone(),
        })
    }
}

/// The sandboxed run of `argv` in `workspace` under the contract's
/// execution policy.
fn exec_spec(
    argv: &Value,
    workspace: &Path,
    execution: &Value,
    timeout_s: f64,
    mounts: Mounts,
) -> std::result::Result<Spec, String> {
    // contracts frozen before timeouts were bounded may hold values no
    // duration can represent: the run cannot happen, and says why
    let timeout = Duration::try_from_secs_f64(timeout_s)
        .map_err(|_| format!("timeout_s {timeout_s} is not a usable number of seconds"))?;
    Ok(spec_within(argv, workspace, execution, timeout, mounts))
}

/// [`exec_spec`] with a bound that is already a duration.
fn spec_within(
    argv: &Value,
    workspace: &Path,
    execution: &Value,
    timeout: Duration,
    mounts: Mounts,
) -> Spec {
    let argv: Vec<&str> = argv
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut spec = Spec::new(&argv, workspace);
    spec.timeout = timeout;
    spec.isolation = isolation(execution);
    spec.read_only = mounts.read_only;
    spec.hidden = mounts.hidden;
    if let Some(env) = execution["env"].as_object() {
        spec.env.extend(
            env.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))),
        );
    }
    spec
}

/// Files of `rev` matching `globs`. A tree that cannot be listed is an
/// error: an empty list would read as "no trusted files".
fn files_matching(repo: &Path, rev: &str, globs: &[String]) -> Result<Vec<String>> {
    if globs.is_empty() {
        return Ok(Vec::new());
    }
    let raw = git(repo, &["ls-tree", "-r", "-z", "--name-only", rev])
        .map_err(|e| format!("cannot list the files of {rev}: {e}"))?;
    Ok(raw
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| globs.iter().any(|g| emi_kernel::glob::fnmatch(p, g)))
        .collect())
}

/// Whether the contract accepts only sandboxed runs (anything but an
/// explicit `"isolation": "none"`).
fn sandbox_required(execution: &Value) -> bool {
    execution["isolation"].as_str() != Some("none")
}

/// Test ids the inventory command lists in `workspace`, or why it could not.
fn inventory_ids(
    inv: &Value,
    workspace: &Path,
    execution: &Value,
    mounts: Mounts,
) -> std::result::Result<std::collections::BTreeSet<String>, String> {
    let timeout = inv["timeout_s"].as_f64().unwrap_or(300.0);
    let mut spec = exec_spec(&inv["command"], workspace, execution, timeout, mounts)?;
    // one line per test: a large suite's listing outgrows the default limit
    spec.output_limit = EVIDENCE_OUTPUT_LIMIT;
    let o = emi_exec::run(&spec);
    if sandbox_required(execution) && o.receipt.isolation != "bwrap" {
        return Err("the inventory could not run sandboxed (bubblewrap unavailable)".into());
    }
    // a test file that cannot be imported makes pytest's listing exit
    // nonzero while still naming every test it could collect; a contract
    // that compares the listing with the baseline's may accept that, since
    // a file lost on the candidate shows up as missing tests
    let tolerated = inv["collection_errors"] == "tolerate" && matches!(o.exit_code, Some(1 | 2));
    if o.status != Status::Exited || !(o.exit_code == Some(0) || tolerated) {
        return Err(format!(
            "inventory command did not succeed ({:?}, exit {:?}): {}",
            o.status,
            o.exit_code,
            tail(&o.stderr, 400)
        ));
    }
    // the output is kept only up to a limit: a list cut off there looks
    // like a shorter suite, and two lists cut at the same place look equal
    if o.stdout_bytes > spec.output_limit as u64 {
        return Err(format!(
            "the inventory printed {} bytes and only the first {} were kept, so its list of \
             tests is partial",
            o.stdout_bytes, spec.output_limit
        ));
    }
    let ids = parse_inventory(&o.stdout, inv["format"] == "pytest")?;
    Ok(ids.iter().map(|id| portable(id, workspace)).collect())
}

/// A test id without the path of the workspace it ran in: a test
/// parametrized by a path under its own checkout would otherwise have a
/// different name in every workspace, and look missing or new.
fn portable(id: &str, workspace: &Path) -> String {
    id.replace(workspace.to_string_lossy().as_ref(), "<workspace>")
}

/// The test ids in an inventory's output. In the pytest format only
/// `path::name` lines are ids (the rest is the runner's summary); output
/// with lines but no id is a format the controller does not read, not an
/// empty suite.
fn parse_inventory(
    stdout: &str,
    pytest: bool,
) -> std::result::Result<std::collections::BTreeSet<String>, String> {
    if pytest && let Some(exact) = reporter_inventory(stdout) {
        return exact;
    }
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    // a pytest id starts its line and its file path holds no space: a
    // warning that quotes a test (`  Test: tests/a.py::t, argvalues ...`)
    // is not one, and counting it would make the suite look larger than
    // any run of it
    let is_id = |l: &&&str| {
        !pytest
            || l.split_once("::")
                .is_some_and(|(path, _)| !path.is_empty() && !path.contains(char::is_whitespace))
    };
    let ids: std::collections::BTreeSet<String> = lines
        .iter()
        .filter(is_id)
        .map(|l| l.trim().to_owned())
        .collect();
    if ids.is_empty() && !lines.is_empty() {
        return Err(format!(
            "the inventory printed {} line(s) but no line of its output is a test id \
             (path::name), so it is not the pytest format listing; first line: {:?}",
            lines.len(),
            lines[0]
        ));
    }
    Ok(ids)
}

/// The exact listing a reporter inside the runner printed: `EMI-ITEM <id>`
/// per test, closed by `EMI-ITEMS <count>`. `None` when the output holds no
/// such listing; an error when it is cut short or does not add up.
fn reporter_inventory(
    stdout: &str,
) -> Option<std::result::Result<std::collections::BTreeSet<String>, String>> {
    let mut ids = std::collections::BTreeSet::new();
    let (mut rows, mut closed) = (0usize, None);
    for line in stdout.lines() {
        if let Some(count) = line.strip_prefix("EMI-ITEMS ") {
            closed = count.trim().parse::<usize>().ok();
        } else if let Some(id) = line.strip_prefix("EMI-ITEM ") {
            rows += 1;
            ids.insert(id.to_owned());
        }
    }
    match (rows, closed) {
        (0, None) => None,
        (n, Some(count)) if n == count => Some(Ok(ids)),
        _ => Some(Err(
            "the inventory's listing is cut short or does not add up to its own count".to_owned(),
        )),
    }
}

/// Test-runner configuration the candidate added: restoring only undoes
/// changes to files the baseline had, and a new conftest.py's hooks run in
/// every check.
fn added_runner_config(changed: &[(String, &'static str)]) -> Vec<String> {
    changed
        .iter()
        .filter(|(p, kind)| *kind == "add" && crate::runner_config::matches(p))
        .map(|(p, _)| p.clone())
        .collect()
}

/// The verification gate: trusted files restored, held-out files injected
/// (both before the checks run), and the test inventory compared. Without
/// a verification section only added runner configuration is looked for,
/// since it changes what every check means whatever the contract asks.
fn verification_gate(
    setup: &RunSetup,
    restored: Vec<String>,
    held_out: Vec<String>,
    candidate_ids: Option<std::result::Result<std::collections::BTreeSet<String>, String>>,
    scratch: &Scratch,
) -> Value {
    let contract = setup.contract;
    let spec = &contract["verification"];
    let added_config = added_runner_config(setup.changed);
    if spec.is_null() {
        return if added_config.is_empty() {
            json!({"state": "not_required"})
        } else {
            json!({"state": "needs_review", "added_runner_config": added_config})
        };
    }
    let policy = spec["runner_config"].as_str().unwrap_or("reject");
    // test files the contract does not trust: an edit to one is verified
    // as the candidate left it, so a narrow `trusted` list must not turn a
    // weakened test into a silent pass
    let untrusted: Vec<&String> = setup
        .changed
        .iter()
        .filter(|(p, kind)| {
            matches!(*kind, "modify" | "delete")
                && emi_kernel::paths::is_test(p)
                && !restored.contains(p)
        })
        .map(|(p, _)| p)
        .collect();
    let mut state = if !added_config.is_empty() && policy == "reject" {
        "fail"
    } else if restored.is_empty() && added_config.is_empty() && untrusted.is_empty() {
        "pass"
    } else {
        "needs_review"
    };
    let mut gate = json!({"restored": restored, "held_out": held_out,
                          "added_runner_config": added_config, "runner_config": policy,
                          "untrusted_test_changes": untrusted,
                          "inventory": {"state": "not_required"}});
    if let Some(candidate_ids) = candidate_ids {
        let base_ws = scratch.0.join("baseline");
        let base_ids = setup.prepare_baseline(&base_ws).and_then(|mounts| {
            inventory_ids(&spec["inventory"], &base_ws, &contract["execution"], mounts)
        });
        match (base_ids, candidate_ids) {
            (Ok(base), Ok(cand)) => {
                let missing: Vec<&String> = base.difference(&cand).collect();
                let added: Vec<&String> = cand.difference(&base).collect();
                if !missing.is_empty() {
                    state = "fail";
                }
                // a baseline that lists no test has none to keep: say so
                // instead of reporting a comparison that compared nothing
                let inventory_state = match (missing.is_empty(), base.is_empty()) {
                    (false, _) => "fail",
                    (true, true) => "nothing_to_compare",
                    (true, false) => "pass",
                };
                gate["inventory"] = json!({"state": inventory_state,
                    "baseline": base.len(), "candidate": cand.len(), "missing": missing,
                    "added": added});
            }
            (Err(e), _) | (_, Err(e)) => {
                if state != "fail" {
                    state = "unverified";
                }
                gate["inventory"] = json!({"state": "unverified", "error": e});
            }
        }
    }
    gate["state"] = json!(state);
    gate
}

fn isolation(execution: &Value) -> Isolation {
    let network = execution["network"].as_bool().unwrap_or(false);
    match execution["isolation"].as_str().unwrap_or("best") {
        "none" => Isolation::None,
        "bwrap" => Isolation::Bwrap { network },
        _ => Isolation::Best { network },
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// The record of a check that could not be run at all: an environment
/// error, never a pass or a failure of the candidate.
fn refused_run(id: &str, def: &Value, why: String) -> (String, String, Value) {
    let record = json!({"check_id": id, "check_definition_id": digest("check-definition", def),
                        "status": "error", "attribution": "environment", "launch_error": why});
    ("error".into(), "environment".into(), record)
}

/// Run one frozen check; `(status, attribution, run record)`.
fn run_check(
    id: &str,
    def: &Value,
    workspace: &Path,
    execution: &Value,
    mounts: Mounts,
) -> (String, String, Value) {
    let timeout = def["timeout_s"].as_f64().unwrap_or(120.0);
    run_check_within(id, def, timeout, workspace, execution, mounts, true)
}

/// [`run_check`] bounded by `timeout` seconds instead of the check's own.
fn run_check_within(
    id: &str,
    def: &Value,
    timeout: f64,
    workspace: &Path,
    execution: &Value,
    mounts: Mounts,
    with_evidence: bool,
) -> (String, String, Value) {
    // contracts frozen before the status was required may state none: the
    // check cannot be judged, which is not the same as expecting 0
    let Some(expected) = def["success"]["exit_code"].as_u64() else {
        return refused_run(
            id,
            def,
            "the check states no usable success.exit_code".into(),
        );
    };
    let mut spec = match exec_spec(&def["command"], workspace, execution, timeout, mounts) {
        Ok(spec) => spec,
        Err(e) => return refused_run(id, def, e),
    };
    let declares_evidence = def.get("evidence").is_some_and(|v| !v.is_null());
    if declares_evidence {
        // a report with one line per test outgrows the default limit on a
        // large suite, and a cut report is no evidence at all
        spec.output_limit = EVIDENCE_OUTPUT_LIMIT;
    }
    let o = emi_exec::run(&spec);
    let unsandboxed = sandbox_required(execution) && o.receipt.isolation != "bwrap";
    let (status, attribution) = match o.status {
        // an acceptance needs the declared isolation: without it the run
        // proves nothing about the candidate
        _ if unsandboxed => ("error", "environment"),
        Status::NotLaunched => ("error", "environment"),
        Status::TimedOut => ("timeout", "unresolved"),
        Status::Exited if !o.receipt.verified_clean => ("error", "environment"),
        Status::Exited if o.exit_code.and_then(|c| u64::try_from(c).ok()) == Some(expected) => {
            ("passed", "candidate")
        }
        Status::Exited => ("failed", "candidate"),
    };
    // the verdict rests on the exit status, which output limits cannot cut,
    // but the retained output is the first bytes: say when it is not all
    let limit = spec.output_limit as u64;
    let (status, attribution, mut evidence) = if with_evidence {
        apply_evidence(def, &o, limit, status, attribution)
    } else {
        (status.to_owned(), attribution.to_owned(), Value::Null)
    };
    if evidence.is_object() {
        evidence["outcomes"] = outcomes(&o, limit, workspace);
        evidence["reader"] = json!(crate::evidence::READER);
    }
    let record = json!({
        "check_id": id, "check_definition_id": digest("check-definition", def),
        "status": status, "attribution": attribution, "exit_code": o.exit_code, "signal": o.signal,
        "duration_ms": o.duration_ms, "launch_error": o.launch_error,
        "stdout_tail": tail(&o.stdout, 2000), "stderr_tail": tail(&o.stderr, 2000),
        "stdout_bytes": o.stdout_bytes, "stderr_bytes": o.stderr_bytes,
        "output_truncated": o.stdout_bytes > limit || o.stderr_bytes > limit,
        "evidence": evidence,
        "receipt": o.receipt,
    });
    (status.to_owned(), attribution.to_owned(), record)
}

/// Execution evidence for a check that declares it: the run's own report
/// must show passing tests, not just a passing exit status. Returns the
/// (possibly worsened) status, its attribution, and the evidence record
/// (`null` when the check declares none).
fn apply_evidence(
    def: &Value,
    o: &emi_exec::Outcome,
    limit: u64,
    status: &str,
    attribution: &str,
) -> (String, String, Value) {
    if def.get("evidence").filter(|v| !v.is_null()).is_none() {
        return (status.to_owned(), attribution.to_owned(), Value::Null);
    }
    // without the check's exit there is nothing to add evidence to: the
    // run already failed, errored or timed out on its own terms
    if status != "passed" {
        return (
            status.to_owned(),
            attribution.to_owned(),
            json!({"format": "pytest", "state": "not_required",
                   "reason": "the run did not pass on its exit status"}),
        );
    }
    // the retained output is the first bytes: a cut report may hide
    // failures and skips, so truncated evidence is unresolved, never a pass
    if o.stdout_bytes > limit || o.stderr_bytes > limit {
        return (
            "error".to_owned(),
            "environment".to_owned(),
            json!({"format": "pytest", "state": "unverified",
                   "reason": "the run's output was cut at the kept limit, so its test report is partial"}),
        );
    }
    match crate::evidence::parse(&o.stdout) {
        Ok(ev) => match crate::evidence::judge(&ev) {
            crate::evidence::Verdict::Pass => (
                status.to_owned(),
                attribution.to_owned(),
                evidence_record(&ev, "pass", ""),
            ),
            crate::evidence::Verdict::Fail(why) => (
                "failed".to_owned(),
                "candidate".to_owned(),
                evidence_record(&ev, "fail", &why),
            ),
        },
        Err(why) => (
            "failed".to_owned(),
            "candidate".to_owned(),
            json!({"format": "pytest", "state": "fail", "reason": why}),
        ),
    }
}

/// Output kept from a check that declares execution evidence.
const EVIDENCE_OUTPUT_LIMIT: usize = 32 << 20;

/// Which tests the run itself named as not passing, whatever its exit
/// status: what a caller needs to compare one run with another (a test
/// already skipped at the baseline is not a regression). `null` unless the
/// whole output was kept and it holds one outcome line per test.
fn outcomes(o: &emi_exec::Outcome, limit: u64, workspace: &Path) -> Value {
    if o.stdout_bytes > limit || o.stderr_bytes > limit {
        return Value::Null;
    }
    match crate::evidence::parse(&o.stdout) {
        Ok(ev) if ev.kind == crate::evidence::Kind::Lines => {
            let failed: std::collections::BTreeSet<String> = ev
                .failed
                .iter()
                .chain(&ev.errored)
                .map(|id| portable(id, workspace))
                .collect();
            let skipped: Vec<String> = ev
                .skipped
                .iter()
                .map(|id| portable(id, workspace))
                .filter(|id| !failed.contains(id))
                .collect();
            json!({"passed": ev.passed.len(), "failed": failed, "skipped": skipped})
        }
        _ => Value::Null,
    }
}

/// The recorded evidence: what form it took, the counts it showed, why it
/// did or did not establish passing tests.
fn evidence_record(ev: &crate::evidence::Evidence, state: &str, reason: &str) -> Value {
    let (kind, passed, failed, skipped) = match ev.kind {
        crate::evidence::Kind::Lines => (
            "lines",
            ev.passed.len(),
            ev.failed.len() + ev.errored.len(),
            ev.skipped.len(),
        ),
        crate::evidence::Kind::Summary => (
            "summary",
            ev.summary_passed as usize,
            ev.summary_failed as usize,
            ev.summary_skipped as usize,
        ),
    };
    json!({"format": "pytest", "state": state, "kind": kind,
           "passed": passed, "failed": failed, "skipped": skipped, "reason": reason})
}

/// Requirement ledger: verified when every bound check passed.
fn ledger(contract: &Value, statuses: &BTreeMap<String, String>) -> Vec<Value> {
    contract["requirements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let bound = globs(&r["checks"]);
            let states: Vec<&str> = bound
                .iter()
                .map(|c| statuses.get(c).map_or("not_run", String::as_str))
                .collect();
            let state = if states.iter().all(|s| *s == "passed") {
                "verified"
            } else if states.contains(&"failed") {
                "failed"
            } else {
                "unverified"
            };
            json!({"id": r["id"], "state": state, "checks": bound, "check_states": states})
        })
        .collect()
}

/// The gates a decision reduces over.
struct Gates<'a> {
    verification: &'a Value,
    dependencies: &'a Value,
}

/// Ids of the requirements in `state`.
fn ids_in(ledger: &[Value], state: &str) -> Vec<String> {
    ledger
        .iter()
        .filter(|r| r["state"] == state)
        .map(|r| r["id"].as_str().unwrap_or("").to_owned())
        .collect()
}

/// Ids of the requirements that are neither verified nor failed: a state
/// the ledger does not name leaves its requirement unresolved, never met.
fn unresolved_ids(ledger: &[Value]) -> Vec<String> {
    ledger
        .iter()
        .filter(|r| !matches!(r["state"].as_str(), Some("verified" | "failed")))
        .map(|r| r["id"].as_str().unwrap_or("").to_owned())
        .collect()
}

/// The states each gate reports. A gate in any other state, or in none,
/// has not said it passed.
const GATE_STATES: [(&str, &[&str]); 2] = [
    (
        "verification",
        &["not_required", "pass", "needs_review", "unverified", "fail"],
    ),
    ("dependencies", &["not_required", "pass", "unverified"]),
];

/// One line per gate whose state is not one it reports.
fn unknown_gate_states(g: &Gates) -> Vec<String> {
    let reported = [g.verification, g.dependencies];
    GATE_STATES
        .iter()
        .zip(reported)
        .filter(|((_, known), gate)| !gate["state"].as_str().is_some_and(|s| known.contains(&s)))
        .map(|((name, _), gate)| {
            format!(
                "gate {name} reported state {}, which the controller does not know, so it \
                 cannot count as passed",
                gate["state"]
            )
        })
        .collect()
}

/// One reason for a decision and the structural check it rests on (none
/// for a gap in the evidence).
struct Reason {
    text: String,
    signal: Option<String>,
}

impl Reason {
    fn new(text: String, signal: &str) -> Reason {
        Reason {
            text,
            signal: Some(signal.to_owned()),
        }
    }

    fn gap(text: String) -> Reason {
        Reason { text, signal: None }
    }
}

/// Why the verification gate rejects the candidate: tests missing from
/// the inventory, runner configuration added under the `reject` policy.
fn verification_rejections(v: &Value) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if v["state"] != "fail" {
        return reasons;
    }
    if v["inventory"]["state"] == "fail" {
        reasons.push(Reason::new(
            format!(
                "{} test(s) of the baseline are missing from the candidate's inventory",
                v["inventory"]["missing"].as_array().map_or(0, Vec::len)
            ),
            "verification.inventory",
        ));
    }
    let added = globs(&v["added_runner_config"]);
    if v["runner_config"] == "reject" && !added.is_empty() {
        reasons.push(Reason::new(
            format!(
                "test-runner configuration added (it runs in every check, and restoring cannot undo it): {}",
                added.join(", ")
            ),
            "verification.runner_config",
        ));
    }
    reasons
}

/// Why the candidate is rejected (empty: it is not).
fn rejections(ledger: &[Value], g: &Gates) -> Vec<Reason> {
    let mut reasons = Vec::new();
    let failed = ids_in(ledger, "failed");
    if !failed.is_empty() {
        reasons.push(Reason::new(
            format!("requirements failed their checks: {}", failed.join(", ")),
            "requirements",
        ));
    }
    reasons.extend(verification_rejections(g.verification));
    reasons
}

/// What could not be verified (empty: everything was).
fn gaps(ledger: &[Value], g: &Gates) -> Vec<String> {
    let mut reasons = Vec::new();
    let unverified = unresolved_ids(ledger);
    if !unverified.is_empty() {
        reasons.push(format!(
            "requirements not verified (errors, timeouts or unverified cleanup): {}",
            unverified.join(", ")
        ));
    }
    reasons.extend(unknown_gate_states(g));
    if g.verification["state"] == "unverified" {
        reasons.push("the test inventory could not be taken".into());
    }
    if g.dependencies["state"] == "unverified" {
        reasons.push(format!(
            "linked dependency directories differ from the frozen contract (revise the execution policy to re-pin them): {}",
            globs(&g.dependencies["changed"]).join("; ")
        ));
    }
    reasons
}

/// What in the verification gate a person should look at.
fn reviews(v: &Value) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if v["state"] != "needs_review" {
        return reasons;
    }
    let restored = globs(&v["restored"]);
    if !restored.is_empty() {
        reasons.push(Reason::new(
            format!(
                "trusted files changed (verified against their baseline versions): {}",
                restored.join(", ")
            ),
            "verification.trusted",
        ));
    }
    let added = globs(&v["added_runner_config"]);
    if !added.is_empty() {
        reasons.push(Reason::new(
            format!(
                "test-runner configuration added (it runs in every check, and restoring cannot undo it): {}",
                added.join(", ")
            ),
            "verification.runner_config",
        ));
    }
    let untrusted = globs(&v["untrusted_test_changes"]);
    if !untrusted.is_empty() {
        reasons.push(Reason::new(
            format!(
                "test files outside the trusted set changed (verified as the candidate left them): {}",
                untrusted.join(", ")
            ),
            "verification.untrusted_tests",
        ));
    }
    reasons
}

/// The decision and its reasons. Precedence: rejected, then incomplete,
/// then needs_review, then accepted.
fn reduce(ledger: &[Value], gates: &Gates) -> (&'static str, Vec<Reason>) {
    if ledger.is_empty() {
        // nothing required is nothing verified
        return (
            "incomplete",
            vec![Reason::gap(
                "the contract has no requirements, so nothing was verified".into(),
            )],
        );
    }
    let rejected = rejections(ledger, gates);
    if !rejected.is_empty() {
        return ("rejected", rejected);
    }
    let missing = gaps(ledger, gates);
    if !missing.is_empty() {
        return ("incomplete", missing.into_iter().map(Reason::gap).collect());
    }
    let review = reviews(gates.verification);
    if !review.is_empty() {
        return ("needs_review", review);
    }
    (
        "accepted",
        vec![Reason::gap(
            "every requirement verified by its frozen checks; all gates pass".into(),
        )],
    )
}

/// What a decision rests on: the structural checks behind its reasons, and
/// how the checks ran.
fn assurance(reasons: &[Reason], isolation: &str) -> Value {
    let mut structural: Vec<String> = reasons.iter().filter_map(|r| r.signal.clone()).collect();
    structural.sort();
    structural.dedup();
    let catch_kind = if structural.is_empty() {
        "none"
    } else {
        "structural"
    };
    json!({"structural": structural, "catch_kind": catch_kind, "isolation": isolation})
}

/// What the checks' results are worth: sandboxed runs, or an explicit
/// opt-out of isolation (weaker: the candidate could reach the host).
fn isolation_of(contract: &Value) -> &'static str {
    if sandbox_required(&contract["execution"]) {
        "sandboxed"
    } else {
        "unsandboxed (execution.isolation = none)"
    }
}

/// Run every frozen check in `workspace`, recording each run.
fn run_checks(
    store: &Store,
    setup: &RunSetup,
    scratch: &Path,
    candidate_id: &str,
) -> Result<(BTreeMap<String, String>, Vec<Value>)> {
    let contract = setup.contract;
    let mut statuses = BTreeMap::new();
    let mut runs = Vec::new();
    for (n, (id, def)) in contract["checks"]
        .as_object()
        .into_iter()
        .flatten()
        .enumerate()
    {
        // every check starts from the submitted tree, not from what an
        // earlier check left behind
        let dir = scratch.join(format!("run-{n}"));
        let mounts = setup.prepare(&dir)?;
        let (status, _, mut record) = run_check(id, def, &dir, &contract["execution"], mounts);
        let _ = std::fs::remove_dir_all(&dir);
        record["candidate_id"] = json!(candidate_id);
        record["contract_id"] = contract["contract_id"].clone();
        store.append("check_run", &record)?;
        statuses.insert(id.clone(), status);
        runs.push(record);
    }
    Ok((statuses, runs))
}

fn check_summary(r: &Value) -> Value {
    json!({"check_id": r["check_id"], "status": r["status"], "attribution": r["attribution"],
           "exit_code": r["exit_code"], "duration_ms": r["duration_ms"],
           "verified_clean": r["receipt"]["verified_clean"], "isolation": r["receipt"]["isolation"]})
}

/// What one verification reads: the pinned baseline and one commit holding
/// the candidate (the submitted commit, or the working tree frozen into
/// one), so every gate and every run sees the same content.
struct Subject {
    repo: PathBuf,
    /// A clean view of the repository's objects (no config, attributes,
    /// hooks or replace refs of the agent's repository apply to reads).
    objects: PathBuf,
    /// The authority root (every task's contracts).
    authority: PathBuf,
    baseline: String,
    /// The commit every gate and check reads.
    rev: String,
    /// What the records name: the commit, or `worktree`.
    candidate_id: String,
}

/// The checks' outcomes and the verification gate.
struct Verified {
    statuses: BTreeMap<String, String>,
    runs: Vec<Value>,
    gate: Value,
}

/// Run the inventory and every check, each in a fresh workspace with
/// trusted files restored read-only and held-out files injected.
fn run_verified(
    store: &Store,
    contract: &Value,
    subject: &Subject,
    changed: &[(String, &'static str)],
    scratch: &Scratch,
) -> Result<Verified> {
    let vspec = &contract["verification"];
    let trusted = globs(&vspec["trusted"]);
    let setup = RunSetup {
        objects: &subject.objects,
        repo: &subject.repo,
        candidate: &subject.rev,
        baseline: &subject.baseline,
        contract,
        changed,
        trusted_files: files_matching(&subject.objects, &subject.baseline, &trusted)?,
        hidden: vec![subject.authority.clone(), subject.repo.clone()],
    };
    let restored = trusted_changes(&trusted, changed);
    let held_out: Vec<String> = vspec["held_out"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let candidate_ids = if vspec["inventory"].is_null() {
        None
    } else {
        let dir = scratch.0.join("inventory");
        let mounts = setup.prepare(&dir)?;
        Some(inventory_ids(
            &vspec["inventory"],
            &dir,
            &contract["execution"],
            mounts,
        ))
    };
    let (statuses, runs) = run_checks(store, &setup, &scratch.0, &subject.candidate_id)?;
    let gate = verification_gate(&setup, restored, held_out, candidate_ids, scratch);
    Ok(Verified {
        statuses,
        runs,
        gate,
    })
}

/// Every gate's result and the requirement ledger for one candidate.
struct Evaluation {
    ledger: Vec<Value>,
    gates: Value,
    runs: Vec<Value>,
}

/// Run the checks and compute every gate over `subject`.
fn evaluate(store: &Store, contract: &Value, subject: &Subject) -> Result<Evaluation> {
    let changed = changes(&subject.objects, &subject.baseline, &subject.rev)?;
    let scratch = Scratch::new()?;
    // checked before and after the runs: the checks execute these tools
    let drifted = crate::deps::drift(&subject.repo, contract);
    let verified = run_verified(store, contract, subject, &changed, &scratch)?;
    let dependencies = crate::deps::gate(&subject.repo, contract, drifted);
    Ok(Evaluation {
        ledger: ledger(contract, &verified.statuses),
        gates: json!({"verification": verified.gate, "dependencies": dependencies}),
        runs: verified.runs,
    })
}

/// The commit holding the candidate's content.
fn candidate_rev(
    repo: &Path,
    view: &Path,
    candidate: &Candidate,
    candidate_id: &str,
) -> Result<String> {
    match candidate {
        Candidate::Ref(_) => Ok(candidate_id.to_owned()),
        Candidate::Worktree => {
            let scratch = Scratch::new()?;
            snapshot::commit_worktree(repo, view, &scratch.0)
        }
    }
}

/// Verify a candidate: run every frozen check, compute the gates, reduce,
/// and record the evidence.
pub fn verify(store: &Store, candidate: &Candidate) -> Result<Value> {
    let contract = store.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let baseline = contract["baseline"]["sha"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let candidate_id = match candidate {
        Candidate::Ref(r) => snapshot::resolve_ref(&repo, r)?,
        Candidate::Worktree => WORKTREE.to_owned(),
    };
    let view = emi_git::CleanView::of(&repo)?;
    let rev = candidate_rev(&repo, view.path(), candidate, &candidate_id)?;
    let subject = Subject {
        objects: view.path().to_path_buf(),
        repo,
        authority: store.dir.parent().unwrap_or(&store.dir).to_path_buf(),
        baseline,
        rev,
        candidate_id: candidate_id.clone(),
    };
    let eval = evaluate(store, &contract, &subject)?;
    let g = &eval.gates;
    let gates = Gates {
        verification: &g["verification"],
        dependencies: &g["dependencies"],
    };
    let (decision, reasons) = reduce(&eval.ledger, &gates);
    let mut d = json!({"task_id": contract["task_id"], "contract_id": contract["contract_id"],
                       "candidate_id": candidate_id, "decision": decision,
                       "reasons": reasons.iter().map(|r| r.text.clone()).collect::<Vec<_>>()});
    d["content_id"] = json!(subject.rev);
    d["requirements"] = Value::Array(eval.ledger);
    d["gates"] = eval.gates;
    d["checks"] = Value::Array(eval.runs.iter().map(check_summary).collect());
    d["assurance"] = assurance(&reasons, isolation_of(&contract));
    if contract["verification"]["inventory"].is_null() {
        d["limits"] = json!([NO_INVENTORY]);
    }
    store.append("decision", &d)?;
    Ok(d)
}
