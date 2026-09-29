//! Task specs for `emi task create`, written from the repository instead of
//! by hand: `emi task draft` (any change) and `emi task refactor-spec` (a
//! simplification). Both trust the existing tests and the test-runner
//! configuration, confine the change to the targets, their neighbours and
//! new tests, and run the checks sandboxed without network.

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::{Value, json};

#[derive(Args)]
pub struct RefactorSpecArgs {
    /// Repository (default: current directory).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Files to simplify (repository-relative; repeatable).
    #[arg(long, required = true)]
    target: Vec<String>,
    /// Required fall in the targets' cognitive complexity (0-1].
    #[arg(long, default_value_t = 0.2)]
    min_reduction: f64,
    /// Trusted test globs, restored at verify (default: common test layouts).
    #[arg(long)]
    tests: Vec<String>,
    #[arg(long)]
    task_id: Option<String>,
    /// The test command, after `--` (argv, no shell).
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Args)]
pub struct DraftArgs {
    /// Repository (default: current directory).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Files the change is about (repository-relative; repeatable).
    #[arg(long, required = true)]
    target: Vec<String>,
    /// What the change must achieve, in words the reviewer will check.
    #[arg(long)]
    objective: String,
    /// Trusted test globs, restored at verify (default: common test
    /// layouts). Narrow them when the change legitimately alters what some
    /// existing tests expect.
    #[arg(long)]
    tests: Vec<String>,
    /// A held-out test the agent never sees: DEST=FILE writes FILE's
    /// content to DEST in the workspace at verify (repeatable).
    #[arg(long = "held-out")]
    held_out: Vec<String>,
    /// A saved `emi profile --format json`: warns where the targets' tests
    /// are weak, missing or unknown before the task is frozen.
    #[arg(long)]
    profile: Option<PathBuf>,
    #[arg(long)]
    task_id: Option<String>,
    /// The test command, after `--` (argv, no shell).
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

const TEST_GLOBS: &[&str] = &[
    "tests/*",
    "test/*",
    "*/tests/*",
    "*/test/*",
    "*/__tests__/*",
    "*test_*.py",
    "*_test.py",
    "*_test.go",
    "*.test.*",
    "*.spec.*",
    "conftest.py",
    "*/conftest.py",
];

/// Test-runner configuration: restored like the tests, so a candidate
/// cannot load a plugin or deselect tests through it. Outside the
/// envelope too: the task has no reason to change how tests run.
const RUNNER_CONFIG_GLOBS: &[&str] = &[
    "pytest.ini",
    "pyproject.toml",
    "setup.cfg",
    "tox.ini",
    "noxfile.py",
    "package.json",
    "jest.config.*",
    "vitest.config.*",
    "vite.config.*",
    ".mocharc*",
    "karma.conf.*",
];

/// The first target's file stem, for a default task id.
fn stem(targets: &[String]) -> String {
    targets
        .first()
        .and_then(|t| t.rsplit('/').next())
        .and_then(|name| name.split('.').next())
        .filter(|s| !s.is_empty())
        .unwrap_or("target")
        .to_owned()
}

/// What the candidate may change: the targets, files beside them (new
/// helpers), and tests (new regression tests).
fn permitted(targets: &[String], tests: &[String]) -> Vec<String> {
    let mut out: Vec<String> = targets.to_vec();
    for t in targets {
        out.push(match t.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/*"),
            None => "*".to_owned(),
        });
    }
    out.extend(tests.iter().cloned());
    out.sort();
    out.dedup();
    out
}

/// The parts every generated spec shares.
fn base(repo: &Path, targets: &[String], tests: &[String], command: &[String]) -> Value {
    let tests: Vec<String> = if tests.is_empty() {
        TEST_GLOBS.iter().map(|g| (*g).to_owned()).collect()
    } else {
        tests.to_vec()
    };
    let mut trusted = tests.clone();
    trusted.extend(RUNNER_CONFIG_GLOBS.iter().map(|g| (*g).to_owned()));
    let mut verification = json!({"trusted": trusted});
    if command.iter().any(|c| c.contains("pytest")) {
        let mut inventory = command.to_vec();
        inventory.extend(["--collect-only".to_owned(), "-q".to_owned()]);
        verification["inventory"] = json!({"command": inventory, "format": "pytest"});
    }
    let link: Vec<&str> = [".venv", "venv", "node_modules"]
        .into_iter()
        .filter(|d| repo.join(d).is_dir())
        .collect();
    json!({
        "repo": repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf()),
        "baseline": "HEAD",
        "checks": {"tests": {"command": command, "success": {"exit_code": 0}, "timeout_s": 1800}},
        "envelope": {"permitted_paths": permitted(targets, &tests), "protected_paths": RUNNER_CONFIG_GLOBS},
        "verification": verification,
        "execution": {"isolation": "best", "network": false, "link": link},
        "warning_policy": {"treatment": "review"},
        "budgets": {"max_retries": 10, "max_time_s": 7200},
    })
}

/// `emi task refactor-spec`: a spec for `emi task create`.
pub fn refactor_spec(a: &RefactorSpecArgs) -> anyhow::Result<u8> {
    let mut spec = base(&a.repo, &a.target, &a.tests, &a.command);
    spec["task_id"] = json!(
        a.task_id
            .clone()
            .unwrap_or_else(|| format!("refactor-{}", stem(&a.target)))
    );
    spec["objective"] = json!(format!(
        "Simplify {} without changing behaviour: lower its cognitive complexity by at least {:.0}% \
         (see `emi explain {}`); every existing test must keep passing.",
        a.target.join(", "),
        a.min_reduction * 100.0,
        a.target.join(" ")
    ));
    spec["requirements"] = json!([{"id": "R1", "acceptance": "the existing test suite passes unchanged", "checks": ["tests"]}]);
    spec["refactor"] = json!({"targets": a.target, "min_cognitive_reduction": a.min_reduction});
    println!("{}", serde_json::to_string_pretty(&spec)?);
    Ok(0)
}

fn held_out(pairs: &[String]) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    for pair in pairs {
        let (dest, file) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--held-out needs DEST=FILE, got {pair:?}"))?;
        let file =
            std::fs::canonicalize(file).map_err(|e| anyhow::anyhow!("--held-out {file}: {e}"))?;
        out.insert(dest.to_owned(), json!({"file": file}));
    }
    Ok(Value::Object(out))
}

/// Warnings about the targets' test evidence in a saved profile.
fn advisories(profile: &Value, repo: &Path, targets: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(head) = emi_diff::snapshot::resolve_ref(repo, "HEAD")
        && profile["head"].as_str() != Some(head.as_str())
    {
        out.push(format!(
            "the profile is of {}, but HEAD is {head}; its evidence may not hold",
            profile["head"]
        ));
    }
    let functions = profile["functions"].as_array().cloned().unwrap_or_default();
    for target in targets {
        let listed: Vec<&Value> = functions
            .iter()
            .filter(|f| f["file"] == target.as_str())
            .collect();
        if listed.is_empty() {
            out.push(format!(
                "the profile lists no function of {target} (profile it with --top 0)"
            ));
        }
        for f in listed.iter().filter(|f| f["readiness"] != "strong") {
            out.push(format!(
                "{target}:{} {} is {}: {}",
                f["start"],
                f["function"].as_str().unwrap_or(""),
                f["readiness"].as_str().unwrap_or(""),
                f["advice"].as_str().unwrap_or("")
            ));
        }
    }
    out
}

/// `emi task draft`: a spec for any change to the targets.
pub fn draft(a: &DraftArgs) -> anyhow::Result<u8> {
    if a.objective.trim().is_empty() {
        anyhow::bail!("--objective must say what the change must achieve");
    }
    let mut spec = base(&a.repo, &a.target, &a.tests, &a.command);
    spec["task_id"] = json!(
        a.task_id
            .clone()
            .unwrap_or_else(|| format!("change-{}", stem(&a.target)))
    );
    spec["objective"] = json!(a.objective.trim());
    spec["requirements"] = json!([{"id": "R1",
        "acceptance": "the objective is met and the test suite passes with the trusted tests restored and the held-out tests added",
        "checks": ["tests"]}]);
    if !a.held_out.is_empty() {
        spec["verification"]["held_out"] = held_out(&a.held_out)?;
    }
    if let Some(path) = &a.profile {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("--profile {}: {e}", path.display()))?;
        let profile = emi_ledger::strict::loads(&text).map_err(anyhow::Error::msg)?;
        for warning in advisories(&profile, &a.repo, &a.target) {
            eprintln!("emi: advisory: {warning}");
        }
    }
    println!("{}", serde_json::to_string_pretty(&spec)?);
    Ok(0)
}
