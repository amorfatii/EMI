//! `emi task init|gate`: cumulative task gating (the oracle's legacy,
//! advisory path). `init` pins a baseline commit and the scope/policy;
//! every `gate` reviews baseline -> candidate under that pinned policy, so
//! a retry cannot reset its allowance by committing in pieces.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use emi_diff::{Request, build_change, snapshot};
use emi_ledger::pyfmt::{digest, dumps_pretty};
use serde_json::{Map, Value, json};

use crate::{Language, ProfileArg, gatecli, specgen};

const RECORD_VERSION: &str = "emi-task-0.1";

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Pin a task baseline record (advisory structural gating).
    Init(InitArgs),
    /// Gate baseline -> AFTER under the recorded policy.
    Gate(GateArgs),
    /// Freeze a supervised task contract under a controller authority.
    Create(CreateArgs),
    /// Verify a candidate: run frozen checks, gates, decide (exit 0 accepted, 3 otherwise).
    Verify(VerifyArgs),
    /// Latest decision for a candidate, without running anything.
    Status(VerifyArgs),
    /// Complete the task with an accepted commit (exit 0) or deny (exit 3).
    Complete(VerifyArgs),
    /// Record a contract revision (actor and reason required).
    Revise(ReviseArgs),
    /// Approve the review items of a commit's needs_review decision, so it
    /// can complete (actor, reason and every item required).
    Approve(ApproveArgs),
    /// Pre-push check: exit 0 only for a completed commit.
    HookCheck(VerifyArgs),
    /// Install a pre-push hook that runs hook-check.
    InstallHook(HookArgs),
    /// Print a refactoring task spec: the target must get simpler
    /// (cognitive complexity) while every existing test keeps passing.
    RefactorSpec(specgen::RefactorSpecArgs),
    /// Print a task spec for a change to the targets (trusted tests and
    /// runner configuration, envelope, held-out tests), with warnings from
    /// a saved profile where the targets' tests are weak or missing.
    Draft(specgen::DraftArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Controller-owned directory outside the repository.
    #[arg(long)]
    authority: PathBuf,
    /// Task spec JSON (task_id, repo, baseline, objective, requirements, checks, envelope, ...).
    #[arg(long)]
    spec: PathBuf,
}

#[derive(Args)]
pub struct VerifyArgs {
    #[arg(long)]
    authority: PathBuf,
    #[arg(long)]
    task: String,
    /// Candidate commit/ref (default HEAD).
    #[arg(long, default_value = "HEAD")]
    candidate: String,
    /// Verify the uncommitted working tree instead (verify/status only).
    #[arg(long)]
    worktree: bool,
    /// Repository (default: the contract's).
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Args)]
pub struct ReviseArgs {
    #[arg(long)]
    authority: PathBuf,
    #[arg(long)]
    task: String,
    /// JSON object of contract fields to replace.
    #[arg(long)]
    changes: String,
    #[arg(long)]
    actor: String,
    #[arg(long)]
    reason: String,
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Args)]
pub struct ApproveArgs {
    #[arg(long)]
    authority: PathBuf,
    #[arg(long)]
    task: String,
    /// The verified commit (default HEAD).
    #[arg(long, default_value = "HEAD")]
    candidate: String,
    /// A review item of the decision (its `review_items`); repeat for each.
    #[arg(long = "item", required = true)]
    items: Vec<String>,
    #[arg(long)]
    actor: String,
    #[arg(long)]
    reason: String,
    #[arg(long)]
    repo: Option<PathBuf>,
}

#[derive(Args)]
pub struct HookArgs {
    #[arg(long)]
    authority: PathBuf,
    #[arg(long)]
    task: String,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(Args)]
pub struct InitArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "src")]
    source: String,
    /// Task baseline ref (resolved to a SHA and pinned).
    #[arg(long)]
    baseline: String,
    #[arg(long, value_enum, default_value_t = Language::Python)]
    probe: Language,
    #[arg(long, value_enum, default_value_t = ProfileArg::Parity)]
    profile: ProfileArg,
    #[arg(long, default_value_t = 10.0, value_name = "UNITS")]
    allow_growth: f64,
    #[arg(long, value_name = "UNITS")]
    allow_headroom: Option<f64>,
    #[arg(long = "exclude")]
    exclude: Vec<String>,
    #[arg(long = "include")]
    include: Vec<String>,
    #[arg(long)]
    tolerate_syntax_errors: bool,
    #[arg(long)]
    use_gitignore: bool,
    #[arg(long)]
    no_test_detection: bool,
    /// Overwrite an existing task record.
    #[arg(long)]
    force: bool,
    /// Where to write the task record.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub struct GateArgs {
    /// Task record from `emi task init`.
    #[arg(long)]
    task: PathBuf,
    /// Candidate ref.
    #[arg(long)]
    after: String,
    /// Accepted for compatibility; the engine needs no probe cache.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    fail_on_warn: bool,
    #[arg(
        long,
        default_value_t = 5,
        value_name = "N",
        allow_negative_numbers = true
    )]
    max_evidence: i64,
    #[arg(long)]
    compact: bool,
    #[arg(long, value_name = "DIR")]
    log_trajectory: Option<PathBuf>,
    #[arg(long)]
    trajectory_strict: bool,
}

fn check_knob(value: f64, label: &str) -> anyhow::Result<f64> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        anyhow::bail!("{label} is out of range or has an invalid unit")
    }
}

fn init(a: &InitArgs) -> anyhow::Result<u8> {
    if !a.force && a.output.exists() {
        anyhow::bail!(
            "refusing to overwrite existing task record {} (pass --force to replace)",
            a.output.display()
        );
    }
    let sha = snapshot::resolve_ref(&a.repo, &a.baseline).map_err(anyhow::Error::msg)?;
    let allowance = check_knob(a.allow_growth, "allowance")?;
    let headroom = match a.allow_headroom {
        None => allowance,
        Some(h) => check_knob(h, "headroom")?,
    };
    let repo = std::fs::canonicalize(&a.repo)?
        .to_string_lossy()
        .into_owned();
    let probe = emi_scan::Probe::from(a.probe).name();
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    let mut record = json!({
        "version": RECORD_VERSION, "repo": repo, "source": a.source, "probe": probe,
        "baseline_ref": a.baseline, "baseline_sha": sha, "allowance": allowance, "headroom": headroom,
        "exclude": a.exclude, "include": a.include, "tolerate_syntax_errors": a.tolerate_syntax_errors,
        "use_gitignore": a.use_gitignore, "detect_tests": !a.no_test_detection,
        "measurement_contract_id": digest("measurement-contract", &json!({
            "probe": probe, "exclude": sorted(&a.exclude), "include": sorted(&a.include),
            "tolerate_syntax_errors": a.tolerate_syntax_errors, "use_gitignore": a.use_gitignore,
            "detect_tests": !a.no_test_detection})),
        "gate_policy_id": digest("gate-policy", &json!({
            "profile": emi_ledger::contracts::PROFILE, "allowance": allowance, "headroom": headroom,
            "version": "emi-gate-0.1"})),
    });
    if matches!(a.profile, ProfileArg::Precise) {
        // Parity records carry no profile key (they are the oracle's).
        record["measurement"] = json!("precise");
    }
    record["created"] = json!(emi_diff::trajectory::utc_now_iso());
    std::fs::write(&a.output, dumps_pretty(&record) + "\n")?;
    Ok(0)
}

fn load(path: &std::path::Path) -> anyhow::Result<Map<String, Value>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read task record {}: {e}", path.display()))?;
    let value = emi_ledger::strict::loads(&text)
        .map_err(|e| anyhow::anyhow!("cannot parse task record {} as JSON: {e}", path.display()))?;
    let Some(record) = value
        .as_object()
        .filter(|r| r.get("version").and_then(Value::as_str) == Some(RECORD_VERSION))
    else {
        anyhow::bail!("{} is not an {RECORD_VERSION} task record", path.display());
    };
    for key in [
        "repo",
        "source",
        "probe",
        "baseline_sha",
        "allowance",
        "headroom",
    ] {
        if record.get(key).is_none_or(Value::is_null) {
            anyhow::bail!("task record {} is missing '{key}'", path.display());
        }
    }
    Ok(record.clone())
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

fn gate(a: &GateArgs) -> anyhow::Result<u8> {
    let r = load(&a.task)?;
    let text = |k: &str| r[k].as_str().unwrap_or("").to_owned();
    let flag = |k: &str, default: bool| r.get(k).and_then(Value::as_bool).unwrap_or(default);
    let gate_args = gatecli::GateArgs {
        guardrail: true,
        fail_on_warn: a.fail_on_warn,
        allow_growth: r["allowance"].as_f64().unwrap_or(10.0),
        allow_headroom: r["headroom"].as_f64(),
        max_evidence: a.max_evidence,
        compact: a.compact,
        log_trajectory: a.log_trajectory.clone(),
        trajectory_strict: a.trajectory_strict,
    };
    gate_args.validate()?;
    let repo = PathBuf::from(text("repo"));
    let after = snapshot::resolve_ref(&repo, &a.after).map_err(anyhow::Error::msg)?;
    let probe = emi_scan::Probe::from_name(&text("probe")).unwrap_or(emi_scan::Probe::Python);
    let mut req = Request::new(&repo, &text("source"), &text("baseline_sha"), &after, probe);
    req.exclude = strings(r.get("exclude"));
    req.include = strings(r.get("include"));
    req.tolerate_syntax_errors = flag("tolerate_syntax_errors", false);
    req.use_gitignore = flag("use_gitignore", false);
    req.detect_tests = flag("detect_tests", true);
    if r.get("measurement").and_then(Value::as_str) == Some("precise") {
        req.profile = emi_scan::Profile::Precise;
    }
    let fail = |e: String| anyhow::anyhow!("emi task gate: {e}");
    let change = build_change(&req).map_err(fail)?;
    let report = emi_ledger::review(&change).map_err(fail)?;
    let mut record = Map::new();
    record.insert("repo".into(), json!(text("repo")));
    record.insert("source".into(), json!(text("source")));
    record.insert("before".into(), json!(text("baseline_sha")));
    record.insert("after".into(), json!(after));
    record.insert("before_ref".into(), json!(text("baseline_sha")));
    record.insert("after_ref".into(), json!(a.after));
    record.insert("probe".into(), json!(text("probe")));
    let (out, code) = gatecli::apply(report, &gate_args, record)?;
    let rendered = dumps_pretty(&out);
    match &a.output {
        Some(p) => std::fs::write(p, rendered)?,
        None => println!("{rendered}"),
    }
    Ok(code)
}

const EXIT_DENIED: u8 = 3;

fn print(v: &Value) {
    println!("{}", dumps_pretty(v));
}

/// The contract's repository (or an override) and the task's store.
fn store(
    authority: &std::path::Path,
    task: &str,
    repo: Option<&PathBuf>,
) -> anyhow::Result<emi_controller::Store> {
    let repo = match repo {
        Some(r) => r.clone(),
        None => {
            // Read the repo path from the stored state's current contract.
            let probe = emi_controller::Store {
                dir: authority.join(task),
            };
            let c = probe.contract().map_err(anyhow::Error::msg)?;
            PathBuf::from(c["repo"]["path"].as_str().unwrap_or("."))
        }
    };
    emi_controller::Store::open(authority, task, &repo).map_err(anyhow::Error::msg)
}

fn candidate(a: &VerifyArgs) -> emi_controller::Candidate {
    if a.worktree {
        emi_controller::Candidate::Worktree
    } else {
        emi_controller::Candidate::Ref(a.candidate.clone())
    }
}

fn supervised(cmd: &TaskCommand) -> anyhow::Result<u8> {
    let err = anyhow::Error::msg;
    match cmd {
        TaskCommand::Create(a) => {
            let text = std::fs::read_to_string(&a.spec)?;
            let spec = emi_ledger::strict::loads(&text).map_err(err)?;
            print(&emi_controller::create(&a.authority, &spec).map_err(err)?);
            Ok(0)
        }
        TaskCommand::Verify(a) => {
            let d = emi_controller::verify(
                &store(&a.authority, &a.task, a.repo.as_ref())?,
                &candidate(a),
            )
            .map_err(err)?;
            print(&d);
            Ok(if d["decision"] == "accepted" {
                0
            } else {
                EXIT_DENIED
            })
        }
        TaskCommand::Status(a) => {
            let s = store(&a.authority, &a.task, a.repo.as_ref())?;
            let c = s.contract().map_err(err)?;
            let repo = PathBuf::from(c["repo"]["path"].as_str().unwrap_or("."));
            let id = if a.worktree {
                emi_controller::verify::WORKTREE.to_owned()
            } else {
                snapshot::resolve_ref(&repo, &a.candidate).map_err(err)?
            };
            let d = s.latest_decision(&id, c["contract_id"].as_str().unwrap_or(""));
            print(&d.unwrap_or_else(|| json!({"candidate_id": id, "decision": null, "reasons": ["not verified under the current contract"]})));
            Ok(0)
        }
        TaskCommand::Complete(a) => {
            let (done, record) = emi_controller::complete(
                &store(&a.authority, &a.task, a.repo.as_ref())?,
                &a.candidate,
            )
            .map_err(err)?;
            print(&record);
            Ok(if done { 0 } else { EXIT_DENIED })
        }
        TaskCommand::HookCheck(a) => {
            let (ok, msg) = emi_controller::hook_check(
                &store(&a.authority, &a.task, a.repo.as_ref())?,
                &a.candidate,
            )
            .map_err(err)?;
            eprintln!("emi: {msg}");
            Ok(if ok { 0 } else { EXIT_DENIED })
        }
        TaskCommand::Revise(a) => {
            let changes = emi_ledger::strict::loads(&a.changes).map_err(err)?;
            print(
                &store(&a.authority, &a.task, a.repo.as_ref())?
                    .revise(&changes, &a.actor, &a.reason)
                    .map_err(err)?,
            );
            Ok(0)
        }
        TaskCommand::Approve(a) => {
            print(
                &emi_controller::approve(
                    &store(&a.authority, &a.task, a.repo.as_ref())?,
                    &a.candidate,
                    &a.actor,
                    &a.reason,
                    &a.items,
                )
                .map_err(err)?,
            );
            Ok(0)
        }
        TaskCommand::InstallHook(a) => install_hook(a),
        TaskCommand::Init(_)
        | TaskCommand::Gate(_)
        | TaskCommand::RefactorSpec(_)
        | TaskCommand::Draft(_) => Ok(0),
    }
}

fn install_hook(a: &HookArgs) -> anyhow::Result<u8> {
    let git_dir = std::process::Command::new("git")
        .arg("-C")
        .arg(&a.repo)
        .args(["rev-parse", "--git-dir"])
        .output()?;
    let git_dir = a.repo.join(String::from_utf8_lossy(&git_dir.stdout).trim());
    let hook = git_dir.join("hooks").join("pre-push");
    if hook.exists() {
        anyhow::bail!(
            "{} exists; add `emi task hook-check` to it yourself",
            hook.display()
        );
    }
    let exe = std::env::current_exe()?;
    let authority = std::fs::canonicalize(&a.authority)?;
    let script = format!(
        "#!/bin/sh\n# Installed by emi: pushes must carry a completed supervised task.\nexec '{}' task hook-check --authority '{}' --task '{}' --candidate HEAD\n",
        exe.display(),
        authority.display(),
        a.task
    );
    std::fs::create_dir_all(hook.parent().unwrap_or(&git_dir))?;
    std::fs::write(&hook, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("installed {}", hook.display());
    Ok(0)
}

pub fn run(cmd: &TaskCommand) -> anyhow::Result<u8> {
    match cmd {
        TaskCommand::Init(a) => init(a),
        TaskCommand::Gate(a) => gate(a),
        TaskCommand::RefactorSpec(a) => specgen::refactor_spec(a),
        TaskCommand::Draft(a) => specgen::draft(a),
        other => supervised(other),
    }
}
