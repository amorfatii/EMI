//! `build_change`: measure a scope at two pinned revisions and assemble the
//! review-ready change document.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use emi_ir::Measurement;
use emi_ledger::contracts::{PROFILE, py_eq};
use emi_ledger::pyfmt::digest;
use emi_scan::{OnSyntaxError, Options, Probe, ScanError};
use serde_json::{Map, Value, json};

use crate::snapshot::{self, Inventory, Result};

/// What to measure and how; mirrors `build_change`'s keyword arguments.
#[derive(Clone, Debug)]
pub struct Request {
    pub repo: PathBuf,
    pub source: String,
    pub before: String,
    pub after: String,
    pub probe: Probe,
    pub merge_base: bool,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub tolerate_syntax_errors: bool,
    pub use_gitignore: bool,
    pub detect_tests: bool,
}

impl Request {
    pub fn new(
        repo: impl Into<PathBuf>,
        source: &str,
        before: &str,
        after: &str,
        probe: Probe,
    ) -> Self {
        Request {
            repo: repo.into(),
            source: source.to_owned(),
            before: before.to_owned(),
            after: after.to_owned(),
            probe,
            merge_base: false,
            exclude: Vec::new(),
            include: Vec::new(),
            tolerate_syntax_errors: false,
            use_gitignore: false,
            detect_tests: true,
        }
    }

    fn options(&self, dest: &Path) -> Options {
        let mut options = Options::new(self.probe);
        options.exclude_globs.clone_from(&self.exclude);
        options.include_globs.clone_from(&self.include);
        options.detect_tests = self.detect_tests;
        options.use_gitignore = self.use_gitignore;
        if self.tolerate_syntax_errors {
            options.on_syntax_error = OnSyntaxError::Record;
        }
        if self.probe == Probe::Js {
            options.config_root = Some(dest.to_path_buf());
        }
        options
    }
}

/// One measured revision; `None` measurement is a valid empty scope.
struct Side {
    inventory: Inventory,
    measurement: Option<Value>,
}

fn to_value(m: &Measurement) -> Result<Value> {
    serde_json::to_value(m).map_err(|e| e.to_string())
}

fn measure(req: &Request, sha: &str, dest: &Path, source: &str) -> Result<Side> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let inventory = snapshot::materialize(&req.repo, sha, dest)?;
    let scope = if source.is_empty() {
        dest.to_path_buf()
    } else {
        dest.join(source)
    };
    snapshot::assert_contained(dest, &scope)?;
    let measurement = match emi_scan::scan(&scope, &req.options(dest)) {
        Ok(m) => Some(to_value(&m)?),
        Err(ScanError::NoSources {
            empty_side: true, ..
        }) => None,
        Err(e) => return Err(e.to_string()),
    };
    Ok(Side {
        inventory,
        measurement,
    })
}

/// The empty side of a first push or full removal, built from the live
/// side's contract so the options stay comparable.
fn empty_like(live: &Value) -> Value {
    json!({"measurement_profile": live["measurement_profile"], "components": [],
           "source_sha256": {}, "errors": [], "regions": [],
           "measurement_options": live.get("measurement_options").cloned().unwrap_or_else(|| json!({}))})
}

fn list<'a>(m: &'a Value, key: &str) -> &'a [Value] {
    m.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn by_id(m: &Value) -> BTreeMap<String, &Value> {
    list(m, "components")
        .iter()
        .map(|c| (c["id"].as_str().unwrap_or("").to_owned(), c))
        .collect()
}

/// Hash-equal files whose measured sloc and burden also agree: only these
/// are asserted unchanged (source bytes alone do not fix clone membership
/// or graph position).
fn unchanged(before: &Value, after: &Value) -> Vec<String> {
    let (b, a) = (by_id(before), by_id(after));
    let hash = |m: &Value, k: &str| {
        m.get("source_sha256")
            .and_then(|h| h.get(k))
            .filter(|v| !v.is_null())
            .cloned()
    };
    b.iter()
        .filter(|(k, bc)| {
            a.get(*k).is_some_and(|ac| {
                hash(before, k).is_some()
                    && hash(before, k) == hash(after, k)
                    && py_eq(&bc["sloc"], &ac["sloc"])
                    && py_eq(&bc["burden"], &ac["burden"])
            })
        })
        .map(|(k, _)| k.clone())
        .collect()
}

type RegionLists<'a> = HashMap<&'a str, Vec<&'a Value>>;

fn regions_by_file(m: &Value) -> RegionLists<'_> {
    let mut grouped: RegionLists = HashMap::new();
    for r in list(m, "regions") {
        grouped
            .entry(r["file"].as_str().unwrap_or(""))
            .or_default()
            .push(r);
    }
    grouped
}

/// Region ids that repeat within a file on either side (fail closed to
/// file level).
fn duplicate_ids(
    idents: &BTreeSet<String>,
    sides: [&RegionLists; 2],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut dups = BTreeMap::new();
    for ident in idents {
        let mut set = BTreeSet::new();
        for side in sides {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for r in side.get(ident.as_str()).into_iter().flatten() {
                *counts.entry(r["id"].as_str().unwrap_or("")).or_default() += 1;
            }
            set.extend(
                counts
                    .into_iter()
                    .filter(|(_, n)| *n > 1)
                    .map(|(id, _)| id.to_owned()),
            );
        }
        dups.insert(ident.clone(), set);
    }
    dups
}

fn attach(
    m: &Value,
    regions: &RegionLists,
    common: &BTreeSet<String>,
    dups: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Value> {
    let mut out = Vec::new();
    for comp in list(m, "components") {
        let id = comp["id"].as_str().unwrap_or("");
        let mut comp = comp.clone();
        if common.contains(id) {
            let recs: Vec<Value> = regions
                .get(id)
                .into_iter()
                .flatten()
                .filter(|r| !dups[id].contains(r["id"].as_str().unwrap_or("")))
                .map(|r| {
                    json!({"id": r["id"], "sloc": r["sloc"], "sha": r["sha"], "in_cycle": r["in_cycle"],
                           "E": r["E"], "C": r["C"]})
                })
                .collect();
            if !recs.is_empty() {
                comp["regions"] = Value::Array(recs);
            }
        }
        out.push(comp);
    }
    out
}

fn ids(m: &Value) -> BTreeSet<String> {
    by_id(m).into_keys().collect()
}

fn snapshot_id(commit: &str, source: &str, m: &Value, tree: &str) -> String {
    digest(
        "snapshot",
        &json!({"commit": commit, "source": source, "source_sha256": m["source_sha256"],
                "measurement_options": m.get("measurement_options"), "tree": tree}),
    )
}

struct Pinned {
    before: String,
    after: String,
}

fn pin(req: &Request) -> Result<Pinned> {
    let before = snapshot::resolve_ref(&req.repo, &req.before)?;
    let after = snapshot::resolve_ref(&req.repo, &req.after)?;
    let before = if req.merge_base {
        snapshot::merge_base(&req.repo, &before, &after)?
    } else {
        before
    };
    Ok(Pinned { before, after })
}

fn measure_both(req: &Request, pinned: &Pinned, source: &str) -> Result<[Side; 2]> {
    let tmp = TempDir::new("emi-diff-")?;
    let before = measure(req, &pinned.before, &tmp.path.join("ref0"), source)?;
    let after = measure(req, &pinned.after, &tmp.path.join("ref1"), source)?;
    Ok([before, after])
}

/// Components with their region records attached (only for files present
/// on both sides, minus ids repeated within a file), plus diagnostics.
fn with_regions(before_m: &Value, after_m: &Value) -> (Vec<Value>, Vec<Value>, Value) {
    let (before_ids, after_ids) = (ids(before_m), ids(after_m));
    let common: BTreeSet<String> = before_ids.intersection(&after_ids).cloned().collect();
    let all: BTreeSet<String> = before_ids.union(&after_ids).cloned().collect();
    let (rb, ra) = (regions_by_file(before_m), regions_by_file(after_m));
    let dups = duplicate_ids(&all, [&rb, &ra]);
    let before_list = attach(before_m, &rb, &common, &dups);
    let after_list = attach(after_m, &ra, &common, &dups);
    let duplicate: Vec<String> = dups
        .iter()
        .flat_map(|(f, set)| set.iter().map(move |r| format!("{f}:{r}")))
        .collect();
    let diagnostics = json!({
        "components_with_regions": before_list.iter().filter(|c| c.get("regions").is_some()).count(),
        "duplicate_region_ids": duplicate});
    (before_list, after_list, diagnostics)
}

/// Both measurements, synthesizing an empty side for a first push or a
/// full removal; both sides empty is not a review.
fn pair(before: Option<Value>, after: Option<Value>) -> Result<(Value, Value)> {
    match (before, after) {
        (None, None) => Err("empty review: no measurable source files on either side".into()),
        (None, Some(a)) => Ok((empty_like(&a), a)),
        (Some(b), None) => {
            let e = empty_like(&b);
            Ok((b, e))
        }
        (Some(b), Some(a)) => Ok((b, a)),
    }
}

fn contract_id(before_m: &Value, after_m: &Value) -> String {
    let options = |m: &Value| m.get("measurement_options").cloned().unwrap_or(Value::Null);
    digest(
        "measurement-contract",
        &json!({"profile": PROFILE,
                "measurement_profile_before": before_m["measurement_profile"],
                "measurement_profile_after": after_m["measurement_profile"],
                "measurement_options_before": options(before_m),
                "measurement_options_after": options(after_m)}),
    )
}

/// `build_change`.
pub fn build_change(req: &Request) -> Result<Value> {
    let source = snapshot::normalize_source(&req.source)?;
    let pinned = pin(req)?;
    let [before_side, after_side] = measure_both(req, &pinned, &source)?;
    let (before_m, after_m) = pair(before_side.measurement, after_side.measurement)?;
    let all: BTreeSet<String> = ids(&before_m).union(&ids(&after_m)).cloned().collect();
    let (before_list, after_list, diagnostics) = with_regions(&before_m, &after_m);
    let errors = |m: &Value| Value::Array(list(m, "errors").to_vec());
    let complete = list(&before_m, "errors").is_empty() && list(&after_m, "errors").is_empty();
    let options = |m: &Value| m.get("measurement_options").cloned().unwrap_or(Value::Null);
    let contract_id = contract_id(&before_m, &after_m);
    let mut change = Map::new();
    let mut put = |k: &str, v: Value| {
        change.insert(k.to_owned(), v);
    };
    put("profile", json!(PROFILE));
    put(
        "measurement_profile_before",
        before_m["measurement_profile"].clone(),
    );
    put(
        "measurement_profile_after",
        after_m["measurement_profile"].clone(),
    );
    put("measurement_options_before", options(&before_m));
    put("measurement_options_after", options(&after_m));
    put("identity_verified", json!(true));
    put(
        "scope",
        json!({"complete": complete, "expected_components": all,
                        "unchanged_components": unchanged(&before_m, &after_m)}),
    );
    put("history_days", Value::Null);
    put("region_diagnostics", diagnostics);
    put(
        "collection_errors",
        json!({"before": errors(&before_m), "after": errors(&after_m)}),
    );
    put("before", Value::Array(before_list));
    put("after", Value::Array(after_list));
    put("before_sha", json!(pinned.before));
    put("after_sha", json!(pinned.after));
    put("before_ref", json!(req.before));
    put("after_ref", json!(req.after));
    put("source_sha256_before", before_m["source_sha256"].clone());
    put("source_sha256_after", after_m["source_sha256"].clone());
    put("measurement_contract_id", json!(contract_id));
    put(
        "snapshot_id_before",
        json!(snapshot_id(
            &pinned.before,
            &source,
            &before_m,
            &before_side.inventory.tree
        )),
    );
    put(
        "snapshot_id_after",
        json!(snapshot_id(
            &pinned.after,
            &source,
            &after_m,
            &after_side.inventory.tree
        )),
    );
    let change = Value::Object(change);
    emi_ledger::contracts::validate_change(&change)?;
    Ok(change)
}

/// A temporary directory removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self> {
        let base = std::env::temp_dir();
        for attempt in 0u32..100 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let path = base.join(format!("{prefix}{}-{nanos}-{attempt}", std::process::id()));
            if std::fs::create_dir(&path).is_ok() {
                return Ok(TempDir { path });
            }
        }
        Err("cannot create a temporary directory".into())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
