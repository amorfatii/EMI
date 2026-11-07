//! The Python processes behind drift, all through a [`Side`] (one fresh
//! tree per process group, the single executor underneath): classify the
//! targets, run the tests on the base side with the input recorder loaded,
//! and replay the recorded inputs. The driver and the recorder are written
//! into the tree they run in, so nothing outside the scratch tree is
//! touched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use emi_exec::Status;
use emi_kernel::json::{canonical, dumps_line};
use serde_json::{Value, json};

use super::{Side, Tree};
use crate::sandbox::tail;

const DRIVER: &str = include_str!("driver.py");
const RECORDER: &str = include_str!("recorder.py");
/// Inside a copy; a dot directory, so test discovery skips it.
const WORK: &str = ".emi-drift";
/// Distinct inputs kept per function across all test processes.
pub const MAX_INPUTS: usize = 50;
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a process gave no usable result, in words for the report.
fn failure(what: &str, o: &emi_exec::Outcome) -> String {
    let why = match o.status {
        Status::NotLaunched => format!(
            "could not start: {}",
            o.launch_error.as_deref().unwrap_or("unknown launch error")
        ),
        Status::TimedOut => format!("timed out after {} ms", o.duration_ms),
        Status::Exited => {
            let code = o
                .exit_code
                .map_or_else(|| format!("signal {:?}", o.signal), |c| c.to_string());
            match o.stderr.trim() {
                "" => format!("exited with {code}"),
                err => format!("exited with {code}: {}", tail(err, 400)),
            }
        }
    };
    format!("{what} {why}")
}

fn work_dir(tree: &Path) -> Result<PathBuf, String> {
    let dir = tree.join(WORK);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// How the driver runs: the interpreter, the environment, the bound.
struct Invocation<'a> {
    python: &'a str,
    env: &'a [(String, String)],
    timeout: Duration,
}

/// Run the driver in `tree` and read its result file.
fn driver<S: Side>(
    side: &S,
    tree: &S::Tree,
    how: &Invocation,
    mode: &str,
    request: &Value,
) -> Result<Value, String> {
    let work = work_dir(tree.dir())?;
    let (script, req, res) = (
        work.join("driver.py"),
        work.join(format!("{mode}-request.json")),
        work.join(format!("{mode}-result.json")),
    );
    write(&script, DRIVER)?;
    write(&req, &dumps_line(request))?;
    let argv: Vec<String> = [how.python, &script.to_string_lossy(), mode]
        .into_iter()
        .map(str::to_owned)
        .chain([req, res.clone()].map(|p| p.to_string_lossy().into_owned()))
        .collect();
    let o = side.run(tree, &argv, how.env, how.timeout);
    if o.status != Status::Exited || o.exit_code != Some(0) {
        return Err(failure(&format!("the {mode} step"), &o));
    }
    let text = std::fs::read_to_string(&res)
        .map_err(|e| format!("the {mode} step wrote no result: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("the {mode} step's result is not JSON: {e}"))
}

/// What each target is, by (file, name, `def` line on the base side):
/// `function`, `static`, `class`, `instance`, `nested`, `async` or
/// `not_found`, with its qualified name.
pub fn classify<S: Side>(
    side: &S,
    tree: &S::Tree,
    python: &str,
    targets: &[(String, String, u32)],
) -> Result<Vec<(String, Option<String>)>, String> {
    let request = json!({"targets": targets.iter().map(|(file, name, line)| {
        json!({"file": file, "name": name, "line": line})
    }).collect::<Vec<_>>()});
    let how = Invocation {
        python,
        env: side.env(),
        timeout: CLASSIFY_TIMEOUT,
    };
    let out = driver(side, tree, &how, "classify", &request)?;
    let items = out["targets"]
        .as_array()
        .filter(|a| a.len() == targets.len())
        .ok_or("the classify step returned a different number of targets")?;
    items
        .iter()
        .map(|t| {
            let kind = t["kind"]
                .as_str()
                .ok_or("a classified target has no kind")?;
            Ok((kind.to_owned(), t["qualname"].as_str().map(str::to_owned)))
        })
        .collect()
}

/// One replayable target, as the recorder and the driver know it.
pub struct Target {
    pub id: usize,
    pub file: String,
    pub qualname: String,
    pub kind: String,
}

/// What the test run recorded for one target, merged over its processes.
#[derive(Default)]
pub struct Recorded {
    /// Module name and import root the tests used; `None` when the tests
    /// never imported the file.
    pub module: Option<(String, Option<String>)>,
    /// The recorder found the module but not the function in it.
    pub missing: bool,
    /// Distinct inputs, ordered by their canonical encoding.
    pub inputs: Vec<Value>,
    pub unrecordable: u64,
}

/// The harvest: inputs per target id and how the test run went.
pub struct Harvest {
    pub recorded: BTreeMap<usize, Recorded>,
    /// The interpreter the tests ran under.
    pub executable: Option<String>,
    pub sys_path: Vec<String>,
    /// Set when the run did not complete normally (the inputs may be partial).
    pub incomplete: Option<String>,
}

fn harvest_env(base: &[(String, String)], tree: &Path, work: &Path) -> Vec<(String, String)> {
    let recorder = work.join("recorder").to_string_lossy().into_owned();
    let mut env: Vec<(String, String)> = base
        .iter()
        .filter(|(k, _)| k != "PYTHONPATH")
        .cloned()
        .collect();
    // the recorder goes first; a PYTHONPATH the caller set still applies
    let path = base
        .iter()
        .rfind(|(k, _)| k == "PYTHONPATH")
        .map_or(recorder.clone(), |(_, v)| format!("{recorder}:{v}"));
    env.push(("PYTHONPATH".into(), path));
    let at = |p: &str| work.join(p).to_string_lossy().into_owned();
    env.push(("EMI_DRIFT_ROOT".into(), tree.to_string_lossy().into()));
    env.push(("EMI_DRIFT_TARGETS".into(), at("targets.json")));
    env.push(("EMI_DRIFT_OUT".into(), at("records")));
    env
}

/// Run the test commands in turn on `tree` (of the base side) with the
/// recorder loaded. `Err` when nothing usable was recorded: a command did
/// not start, the commands timed out before any process wrote its
/// records, or none started Python with the recorder.
pub fn harvest<S: Side>(
    side: &S,
    tree: &S::Tree,
    commands: &[Vec<String>],
    targets: &[Target],
) -> Result<Harvest, String> {
    let work = work_dir(tree.dir())?;
    let records = work.join("records");
    for dir in [work.join("recorder"), records.clone()] {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    write(&work.join("recorder/sitecustomize.py"), RECORDER)?;
    let specs: Vec<Value> = targets
        .iter()
        .map(|t| json!({"id": t.id, "file": t.file, "qualname": t.qualname, "kind": t.kind}))
        .collect();
    write(&work.join("targets.json"), &dumps_line(&Value::from(specs)))?;
    let env = harvest_env(side.env(), tree.dir(), &work);
    let mut outcomes = Vec::new();
    for command in commands {
        let o = side.run(tree, command, &env, side.timeout());
        if o.status == Status::NotLaunched {
            return Err(failure("the test command", &o));
        }
        outcomes.push(o);
    }
    // the first run that did not finish normally says why, else the first
    let Some(o) = outcomes
        .iter()
        .find(|o| o.status != Status::Exited || o.exit_code != Some(0))
        .or(outcomes.first())
    else {
        return Err("no test command to run".into());
    };
    let files = read_records(&records)?;
    if files.is_empty() {
        return Err(match o.status {
            Status::Exited => format!(
                "the test command exited with {:?}, but no Python process loaded the input \
                 recorder (the command must run Python with site enabled and PYTHONPATH \
                 honoured)",
                o.exit_code
            ),
            _ => failure("the test command", o),
        });
    }
    let incomplete = (o.status != Status::Exited || o.exit_code != Some(0))
        .then(|| failure("the test command on the base commit", o));
    merge(&files, incomplete)
}

fn read_records(dir: &Path) -> Result<Vec<Value>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            serde_json::from_str(&text)
                .map_err(|e| format!("the recorder wrote invalid JSON in {}: {e}", p.display()))
        })
        .collect()
}

fn canon(v: &Value) -> String {
    let mut s = String::new();
    canonical(v, &mut s);
    s
}

fn merge_one(
    into: &mut Recorded,
    seen: &mut BTreeMap<String, Value>,
    rec: &Value,
) -> Result<(), String> {
    if let Some(module) = rec["module"].as_str() {
        let this = (module.to_owned(), rec["root"].as_str().map(str::to_owned));
        // several processes may import one file under different names;
        // the smallest keeps the choice independent of process order
        if into.module.as_ref().is_none_or(|m| this < *m) {
            into.module = Some(this);
        }
    }
    into.missing |= rec["missing"] == true;
    into.unrecordable += rec["unrecordable"]
        .as_u64()
        .ok_or("a recorder file lacks the unrecordable count")?;
    let inputs = rec["inputs"]
        .as_array()
        .ok_or("a recorder file lacks the recorded inputs")?;
    for input in inputs {
        seen.entry(canon(input)).or_insert_with(|| input.clone());
    }
    Ok(())
}

fn merge(files: &[Value], incomplete: Option<String>) -> Result<Harvest, String> {
    let mut recorded: BTreeMap<usize, Recorded> = BTreeMap::new();
    let mut seen: BTreeMap<usize, BTreeMap<String, Value>> = BTreeMap::new();
    let mut executables: Vec<&str> = Vec::new();
    let mut sys_path: Vec<String> = Vec::new();
    for f in files {
        executables.extend(f["executable"].as_str());
        for p in f["sys_path"].as_array().into_iter().flatten() {
            if let Some(p) = p.as_str().filter(|p| !sys_path.iter().any(|q| q == p)) {
                sys_path.push(p.to_owned());
            }
        }
        let targets = f["targets"]
            .as_object()
            .ok_or("a recorder file lacks its targets")?;
        for (id, rec) in targets {
            let id: usize = id
                .parse()
                .map_err(|_| format!("a recorder file names an unknown target {id:?}"))?;
            merge_one(
                recorded.entry(id).or_default(),
                seen.entry(id).or_default(),
                rec,
            )?;
        }
    }
    for (id, inputs) in seen {
        if let Some(r) = recorded.get_mut(&id) {
            r.inputs = inputs.into_values().take(MAX_INPUTS).collect();
        }
    }
    executables.sort_unstable();
    Ok(Harvest {
        recorded,
        executable: executables.first().map(|s| (*s).to_owned()),
        sys_path,
        incomplete,
    })
}

/// The interpreter for a replay in `tree`: the harvest's, moved into
/// `tree` when it lived inside the harvested tree (a linked virtualenv).
pub fn interpreter(harvest: &Harvest, base: &Path, tree: &Path) -> String {
    let Some(exe) = harvest.executable.as_deref() else {
        return "python3".into();
    };
    let roots = [Some(base.to_path_buf()), base.canonicalize().ok()];
    roots
        .iter()
        .flatten()
        .find_map(|r| Path::new(exe).strip_prefix(r).ok())
        .map_or_else(
            || exe.to_owned(),
            |rel| tree.join(rel).to_string_lossy().into_owned(),
        )
}

/// One replay request entry.
pub struct Replay<'a> {
    pub target: &'a Target,
    pub module: &'a str,
    pub root: Option<&'a str>,
    pub inputs: &'a [Value],
}

/// Settings for one side's replay.
pub struct ReplaySettings<'a> {
    pub python: &'a str,
    pub sys_path: &'a [String],
    pub call_timeout_s: f64,
    /// Calls per input: two on the base side detect unstable functions.
    pub repeat: u32,
}

/// Per target id: `{"outcomes": [[outcome; repeat]; inputs]}` or
/// `{"import_error": "..."}`.
pub fn replay<S: Side>(
    side: &S,
    tree: &S::Tree,
    items: &[Replay],
    s: &ReplaySettings,
) -> Result<BTreeMap<usize, Value>, String> {
    let timeout = side.timeout();
    // the driver stops calling in time to write what it has
    let own = timeout.as_secs_f64() - (timeout.as_secs_f64() / 10.0).clamp(1.0, 30.0);
    let request = json!({
        "targets": items.iter().map(|r| json!({
            "id": r.target.id, "module": r.module, "root": r.root,
            "qualname": r.target.qualname, "kind": r.target.kind, "inputs": r.inputs,
        })).collect::<Vec<_>>(),
        "sys_path": s.sys_path,
        "call_timeout": s.call_timeout_s,
        "total_timeout": own.max(1.0),
        "repeat": s.repeat,
    });
    let mut env = side.env().to_vec();
    // dicts and sets of strings must come out in the same order on each side
    env.push(("PYTHONHASHSEED".into(), "0".into()));
    let how = Invocation {
        python: s.python,
        env: &env,
        timeout,
    };
    let out = driver(side, tree, &how, "replay", &request)?;
    let results = out["results"]
        .as_object()
        .ok_or("the replay step's result lacks `results`")?;
    items
        .iter()
        .map(|r| {
            results
                .get(&r.target.id.to_string())
                .map(|v| (r.target.id, v.clone()))
                .ok_or_else(|| format!("the replay step skipped {}", r.target.qualname))
        })
        .collect()
}

/// What generation gave each target: its inputs, or why there are none.
pub struct Generated {
    pub inputs: BTreeMap<usize, Vec<Value>>,
    /// Per target id: why nothing was generated for it.
    pub refused: BTreeMap<usize, String>,
}

/// Inputs drawn from each target's annotations (see `driver.py`), in a
/// fresh tree of the base side.
pub fn generate<S: Side>(
    side: &S,
    tree: &S::Tree,
    items: &[Replay],
    (python, sys_path, examples): (&str, &[String], u32),
) -> Result<Generated, String> {
    let request = json!({
        "targets": items.iter().map(|r| json!({
            "id": r.target.id, "module": r.module, "root": r.root,
            "qualname": r.target.qualname, "kind": r.target.kind,
        })).collect::<Vec<_>>(),
        "sys_path": sys_path,
        "examples": examples,
    });
    let mut env = side.env().to_vec();
    env.push(("PYTHONHASHSEED".into(), "0".into()));
    let how = Invocation {
        python,
        env: &env,
        timeout: side.timeout(),
    };
    let out = driver(side, tree, &how, "generate", &request)?;
    let results = out["results"]
        .as_object()
        .ok_or("the generate step's result lacks `results`")?;
    let mut generated = Generated {
        inputs: BTreeMap::new(),
        refused: BTreeMap::new(),
    };
    for r in items {
        let v = &results[&r.target.id.to_string()];
        match (v["inputs"].as_array(), v["error"].as_str()) {
            (Some(inputs), _) => {
                generated.inputs.insert(r.target.id, inputs.clone());
            }
            (None, Some(why)) => {
                generated.refused.insert(r.target.id, why.to_owned());
            }
            (None, None) => {
                generated
                    .refused
                    .insert(r.target.id, "the generate step skipped it".to_owned());
            }
        }
    }
    Ok(generated)
}
