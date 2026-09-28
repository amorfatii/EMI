//! The tools. Every tool measures what it reports itself (raw git blobs or
//! the working tree): agents pass locations and refs, never measurements.

use std::path::PathBuf;

use emi_diff::Request;
use emi_scan::{Probe, Profile};
use serde_json::{Map, Value, json};

type Out = Result<Value, String>;

fn common_schema(extra: Value) -> Value {
    let mut props = json!({
        "repo": {"type": "string", "description": "Path to the git repository (default: current directory)."},
        "source": {"type": "string", "description": "Product scope subdirectory, repository-relative (default: \"src\"; \".\" for the root)."},
        "probe": {"type": "string", "enum": ["python", "js", "rust"], "description": "Language adapter (default: python)."},
        "profile": {"type": "string", "enum": ["precise", "parity", "graded"], "description": "Measurement contract (default: graded, which weighs function erosion by cognitive complexity and Halstead volume)."}
    });
    if let (Some(p), Value::Object(e)) = (props.as_object_mut(), extra) {
        p.extend(e);
    }
    json!({"type": "object", "properties": props})
}

pub fn list() -> Vec<Value> {
    vec![
        json!({"name": "emi_gate",
               "description": "Gate a change: measure the working tree (or the `after` ref) against `before` and return block/warn/pass with margins per dimension, the components responsible, and a next_step (split, shrink, justify, inspect-unknowns, proceed). Blocks only on proven burden growth beyond the allowance.",
               "inputSchema": common_schema(json!({
                   "before": {"type": "string", "description": "Base ref (default: HEAD)."},
                   "after": {"type": "string", "description": "Changed ref; omit to measure the working tree."},
                   "merge_base": {"type": "boolean", "description": "Diff against merge-base(before, after)."},
                   "allow_growth": {"type": "number", "description": "Burden units of proven growth tolerated (default 10)."},
                   "allow_headroom": {"type": "number", "description": "Possible growth behind missing evidence tolerated before a warn."},
                   "max_evidence": {"type": "integer", "description": "Cap on evidence entries (default 5)."}}))}),
        json!({"name": "emi_explain",
               "description": "Explain one file's measured burden: every region (function) with SLOC, complexity, erosion, redundancy lines, coupling dependencies and unresolved calls, plus clone groups and findings touching the file. `refactoring` lists each function's graded erosion, what drives its cognitive complexity (nesting, boolean chains, else branches), and line-anchored steps that lower it, with the cognitive complexity after them. Pass `line` to get only the region containing it.",
               "inputSchema": common_schema(json!({
                   "file": {"type": "string", "description": "Scope-relative file path."},
                   "line": {"type": "integer", "description": "Optional 1-based line."}}))}),
        json!({"name": "emi_hotspots",
               "description": "Rank files by measured burden × change frequency over a history window, with change coupling (files that change together), authorship concentration and AI-attributed share.",
               "inputSchema": common_schema(json!({
                   "days": {"type": "integer", "description": "History window in days (default 365; 0 = all)."},
                   "top": {"type": "integer", "description": "Rows to return (default 10)."}}))}),
        json!({"name": "emi_accept_rewrite",
               "description": "Anti-gaming check for a refactor: accept only when matched density improves AND measured burden is reduced, with no regressions and robustness across weightings. Dilution (added clean code, nothing removed) returns needs-review.",
               "inputSchema": common_schema(json!({
                   "before": {"type": "string", "description": "Base ref (default: HEAD)."},
                   "after": {"type": "string", "description": "Refactored ref; omit for the working tree."}}))}),
        json!({"name": "emi_task_verify",
               "description": "Verify your work against a supervised task the controller froze: runs the task's frozen checks in an isolated snapshot, checks the change stays inside the task's scope envelope, applies the guardrails (weakened or skipped tests, prohibited files, new suppressions, growth; each violation with an id, file and line) and the structural gate, and returns accepted / rejected / needs_review / incomplete with per-requirement states. Consumes one retry from the task budget. You cannot change the task's checks or criteria.",
               "inputSchema": {"type": "object", "required": ["authority", "task"], "properties": {
                   "authority": {"type": "string", "description": "Controller authority directory."},
                   "task": {"type": "string", "description": "Task id."},
                   "candidate": {"type": "string", "description": "Commit/ref to verify; omit for the working tree."}}}}),
        json!({"name": "emi_task_status",
               "description": "The task's frozen objective, requirements and budget, plus the latest decision for a candidate (no checks run, no budget spent).",
               "inputSchema": {"type": "object", "required": ["authority", "task"], "properties": {
                   "authority": {"type": "string"}, "task": {"type": "string"},
                   "candidate": {"type": "string", "description": "Commit/ref; omit for the working tree."}}}}),
        json!({"name": "emi_guard",
               "description": "Check your change against verifiable guardrails before finishing: edits outside the permitted scope, deleted/skipped/weakened tests (fewer test cases or assertions, skip or xfail markers), changed test or coverage config, lockfile/CI/secret/vendored edits, newly added lint or type suppressions, and growth beyond budget. Each violation has a stable id, file and line, and a tier: block (fix it) or review (justify it). Deterministic; no scores.",
               "inputSchema": {"type": "object", "properties": {
                   "repo": {"type": "string", "description": "Path to the git repository (default: current directory)."},
                   "base": {"type": "string", "description": "Baseline ref (default: HEAD)."},
                   "head": {"type": "string", "description": "Candidate ref; omit to check the working tree."},
                   "policy": {"type": "object", "description": "Optional guardrail policy: permitted_paths, excluded_paths, protected_paths, allow_classes, tiers, max_net_sloc, max_files."}}}}),
        json!({"name": "emi_scan",
               "description": "Summarize the scope at a ref or the working tree: files, SLOC, burden totals per dimension, unknown-coupling share, cycles, and the most burdened files.",
               "inputSchema": common_schema(json!({
                   "at": {"type": "string", "description": "Ref to measure; omit for the working tree."},
                   "top": {"type": "integer", "description": "Files to list (default 10)."}}))}),
    ]
}

pub fn call(name: &str, args: &Value) -> Option<Out> {
    Some(match name {
        "emi_gate" => gate(args),
        "emi_explain" => explain(args),
        "emi_hotspots" => hotspots(args),
        "emi_accept_rewrite" => accept(args),
        "emi_scan" => summary(args),
        "emi_task_verify" => task_verify(args),
        "emi_task_status" => task_status(args),
        "emi_guard" => guard(args),
        _ => return None,
    })
}

fn guard(args: &Value) -> Out {
    let repo = PathBuf::from(text(args, "repo", "."));
    let policy = match args.get("policy") {
        Some(p) if !p.is_null() => emi_guard::Policy::from_json(p)?,
        _ => emi_guard::Policy::default(),
    };
    let base = emi_diff::snapshot::resolve_ref(&repo, text(args, "base", "HEAD"))?;
    let head = match args.get("head").and_then(Value::as_str) {
        Some(h) => Some(emi_diff::snapshot::resolve_ref(&repo, h)?),
        None => None,
    };
    Ok(emi_guard::guard(&repo, &base, head.as_deref(), &policy)?.to_json())
}

fn text<'a>(args: &'a Value, key: &str, default: &'a str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn request(args: &Value, before: &str) -> Result<Request, String> {
    let probe = Probe::from_name(text(args, "probe", "python")).ok_or_else(|| {
        format!(
            "probe must be python, js or rust, got {}",
            text(args, "probe", "python")
        )
    })?;
    let after = args.get("after").and_then(Value::as_str);
    let mut req = Request::new(
        PathBuf::from(text(args, "repo", ".")),
        text(args, "source", "src"),
        before,
        after.unwrap_or(""),
        probe,
    );
    req.worktree = after.is_none();
    req.merge_base = args
        .get("merge_base")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    req.tolerate_syntax_errors = true;
    let profile = text(args, "profile", "graded");
    req.profile = Profile::from_name(profile)
        .ok_or_else(|| format!("profile must be parity, precise or graded, got {profile}"))?;
    Ok(req)
}

fn review(args: &Value) -> Out {
    let req = request(args, text(args, "before", "HEAD"))?;
    let change = emi_diff::build_change(&req)?;
    emi_ledger::review(&change)
}

fn gate(args: &Value) -> Out {
    let report = review(args)?;
    let policy = emi_ledger::gate::Policy {
        fail_on_warn: false,
        allowance: args
            .get("allow_growth")
            .and_then(Value::as_f64)
            .unwrap_or(10.0),
        headroom: args.get("allow_headroom").and_then(Value::as_f64),
        max_evidence: args
            .get("max_evidence")
            .and_then(Value::as_i64)
            .unwrap_or(5),
        compact: true,
    };
    let gated = emi_ledger::gate::apply(report, &policy)?;
    let mut out = gated.output;
    out["scope_complete"] = gated.report["scope"]["complete"].clone();
    if let Some(errors) = gated.report.get("collection_errors") {
        out["collection_errors"] = errors.clone();
    }
    Ok(out)
}

fn accept(args: &Value) -> Out {
    let report = review(args)?;
    let mut out = emi_ledger::accept_rewrite(&report)?;
    out["burden_change"] = json!({
        "E": report["burden_change"]["E"]["delta"], "V": report["burden_change"]["V"]["delta"],
        "C": report["burden_change"]["C"]["delta"]});
    out["matched"] = report["matched"]["classification"].clone();
    Ok(out)
}

/// The measurement at `at` (a ref) or of the working tree.
fn measurement(args: &Value, at: Option<&str>) -> Out {
    let mut req = request(args, at.unwrap_or("HEAD"))?;
    req.worktree = at.is_none();
    let target = at.unwrap_or(emi_diff::change::WORKTREE);
    let (_, m) = emi_diff::change::measure_ref(&req, target)?;
    m.ok_or_else(|| "no measurable source files in the scope".to_owned())
}

fn burden_total(components: &[Value], dim: &str) -> [f64; 2] {
    components.iter().fold([0.0, 0.0], |acc, c| {
        let sloc = c["sloc"].as_f64().unwrap_or(0.0);
        let [lo, hi] = match &c["burden"][dim] {
            Value::Null => [0.0, sloc],
            Value::Object(o) => {
                let k = o.get("known_mass").and_then(Value::as_f64).unwrap_or(0.0);
                [
                    k,
                    k + o.get("unknown_sloc").and_then(Value::as_f64).unwrap_or(0.0),
                ]
            }
            v => [v.as_f64().unwrap_or(0.0); 2],
        };
        [acc[0] + lo, acc[1] + hi]
    })
}

fn summary(args: &Value) -> Out {
    let m = measurement(args, args.get("at").and_then(Value::as_str))?;
    let components = m["components"].as_array().cloned().unwrap_or_default();
    let sloc: f64 = components.iter().filter_map(|c| c["sloc"].as_f64()).sum();
    let unknown_c = burden_total(&components, "C");
    let top = usize::try_from(args.get("top").and_then(Value::as_u64).unwrap_or(10)).unwrap_or(10);
    let mut ranked: Vec<(f64, &Value)> = components
        .iter()
        .map(|c| {
            let one = std::slice::from_ref(c);
            let mass: f64 = ["E", "V", "C"]
                .iter()
                .map(|d| burden_total(one, d)[1])
                .sum();
            (mass, c)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(json!({
        "measurement_profile": m["measurement_profile"],
        "files": components.len(),
        "sloc": sloc,
        "burden": {"E": burden_total(&components, "E"), "V": burden_total(&components, "V"), "C": unknown_c},
        "unknown_coupling_share": if sloc > 0.0 { (unknown_c[1] - unknown_c[0]) / sloc } else { 0.0 },
        "cycles": m["static_dependency_cycles"],
        "errors": m["errors"].as_array().map_or(0, Vec::len),
        "most_burdened": ranked.iter().take(top).map(|(mass, c)| json!({"file": c["id"], "sloc": c["sloc"], "burden_upper": mass})).collect::<Vec<_>>(),
    }))
}

fn explain(args: &Value) -> Out {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .ok_or("file is required")?;
    let m = measurement(args, None)?;
    let line = args.get("line").and_then(Value::as_u64);
    let in_file = |v: &&Value| v["file"].as_str() == Some(file);
    let regions: Vec<&Value> = m["regions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(in_file)
        .collect();
    let all: Vec<&Value> = m["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(in_file)
        .collect();
    let selected: Vec<&Value> = match line {
        None => all,
        Some(l) => region_at(&all, l),
    };
    let functions: Vec<Value> = selected
        .into_iter()
        .map(|f| {
            let mut row = Map::new();
            for key in [
                "function",
                "line",
                "sloc",
                "cc",
                "erosion_mass",
                "resolved_dependencies",
                "coupling_unknown",
                "unresolved_calls",
                "in_static_cycle",
            ] {
                if let Some(v) = f.get(key) {
                    row.insert(key.into(), v.clone());
                }
            }
            Value::Object(row)
        })
        .collect();
    let clones: Vec<&Value> = ["clone_groups", "clone_groups_normalized"]
        .iter()
        .flat_map(|k| m[*k].as_array().into_iter().flatten())
        .filter(|g| {
            g.as_array()
                .is_some_and(|occ| occ.iter().any(|o| o["file"].as_str() == Some(file)))
        })
        .collect();
    let findings: Vec<&Value> = m["verbosity_findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(in_file)
        .collect();
    let component = m["components"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["id"].as_str() == Some(file))
        .cloned();
    if component.is_none() && functions.is_empty() {
        return Err(format!(
            "{file} is not a measured file of the scope (check source and probe)"
        ));
    }
    let refactoring = refactoring(args, file, line);
    Ok(
        json!({"file": file, "component": component, "regions": regions.len(), "functions": functions,
              "clone_groups": clones, "findings": findings, "refactoring": refactoring}),
    )
}

/// Refactoring guidance for `file` in the working tree (functions around
/// `line` only, when given), highest graded erosion first.
fn refactoring(args: &Value, file: &str, line: Option<u64>) -> Vec<Value> {
    let path = PathBuf::from(text(args, "repo", "."))
        .join(text(args, "source", "src"))
        .join(file);
    let (Some(lang), Ok(src)) = (emi_panel::Lang::of_path(file), std::fs::read(&path)) else {
        return Vec::new();
    };
    emi_panel::explain::explain(lang, &src)
        .iter()
        .filter(|e| {
            line.is_none_or(|l| {
                (u64::from(e.function.start)..=u64::from(e.function.end)).contains(&l)
            })
        })
        .map(|e| emi_panel::explain::record(file, e))
        .collect()
}

/// The region owning `line`: by its eligible lines when recorded (JS),
/// else the innermost function starting at or before it (Python).
fn region_at<'a>(functions: &[&'a Value], line: u64) -> Vec<&'a Value> {
    let owns = |f: &&&Value| {
        f.get("lines")
            .and_then(Value::as_array)
            .is_some_and(|ls| ls.iter().any(|x| x.as_u64() == Some(line)))
    };
    if let Some(f) = functions.iter().find(owns) {
        return vec![*f];
    }
    functions
        .iter()
        .filter(|f| f["line"].as_u64().is_some_and(|s| s <= line))
        .max_by_key(|f| f["line"].as_u64().unwrap_or(0))
        .map(|f| vec![*f])
        .unwrap_or_default()
}

fn hotspots(args: &Value) -> Out {
    let req = request(args, "HEAD")?;
    let (sha, m) = emi_diff::change::measure_ref(&req, "HEAD")?;
    let m = m.ok_or("no measurable source files at HEAD")?;
    let days = args.get("days").and_then(Value::as_u64).unwrap_or(365);
    let query = emi_history::Query {
        repo: req.repo.clone(),
        source: emi_diff::snapshot::normalize_source(&req.source)?,
        head: sha.clone(),
        days: u32::try_from(days).ok().filter(|d| *d > 0),
        ai_pattern: emi_history::DEFAULT_AI_PATTERN.into(),
    };
    let commits = emi_history::mine(&query)?;
    let histories = emi_history::file_histories(&commits);
    let now = commits.first().map_or(0, |c| c.time);
    let top = usize::try_from(args.get("top").and_then(Value::as_u64).unwrap_or(10)).unwrap_or(10);
    let ranked = emi_history::hotspots::rank(&m, &histories, now);
    let coupling = emi_history::change_coupling(&commits, 5, 30.0, 30);
    Ok(json!({"head": sha, "commits": commits.len(),
              "hotspots": ranked.into_iter().take(top).collect::<Vec<_>>(),
              "change_coupling": coupling.into_iter().take(top).collect::<Vec<_>>()}))
}

fn task_store(args: &Value) -> Result<(emi_controller::Store, Value), String> {
    let authority = PathBuf::from(
        args.get("authority")
            .and_then(Value::as_str)
            .ok_or("authority is required")?,
    );
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .ok_or("task is required")?;
    let probe = emi_controller::Store {
        dir: authority.join(task),
    };
    let contract = probe.contract()?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    Ok((
        emi_controller::Store::open(&authority, task, &repo)?,
        contract,
    ))
}

fn candidate(args: &Value) -> emi_controller::Candidate {
    args.get("candidate")
        .and_then(Value::as_str)
        .map_or(emi_controller::Candidate::Worktree, |c| {
            emi_controller::Candidate::Ref(c.to_owned())
        })
}

fn task_verify(args: &Value) -> Out {
    let (store, _) = task_store(args)?;
    emi_controller::verify(&store, &candidate(args))
}

fn task_status(args: &Value) -> Out {
    let (store, contract) = task_store(args)?;
    let repo = PathBuf::from(contract["repo"]["path"].as_str().unwrap_or("."));
    let id = match candidate(args) {
        emi_controller::Candidate::Worktree => emi_controller::verify::WORKTREE.to_owned(),
        emi_controller::Candidate::Ref(r) => emi_diff::snapshot::resolve_ref(&repo, &r)?,
    };
    let latest = store.latest_decision(&id, contract["contract_id"].as_str().unwrap_or(""));
    let state = store.state()?;
    Ok(
        json!({"task_id": contract["task_id"], "objective": contract["objective"], "revision": contract["revision"],
              "requirements": contract["requirements"], "envelope": contract["envelope"],
              "budgets": contract["budgets"], "retries_used": state["cumulative"]["retries"],
              "candidate_id": id, "latest_decision": latest}),
    )
}
