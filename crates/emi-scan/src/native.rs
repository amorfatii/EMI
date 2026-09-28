//! Scope assembly shared by the native adapters (Rust, Go, ...): walk,
//! classify, measure in parallel, build the scope-wide knowledge, couple,
//! find module cycles, group clones, and assemble components.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use emi_ir::clones::Grouper;
use emi_ir::graph::strongly_connected;
use emi_ir::{Burden, Component, Evidence, FileError, InventoryEntry, Measurement, pytext};
use emi_lang_common::FileMeasure;
use rayon::prelude::*;

use crate::coupling::{RegionEvidence, apply};
use crate::walk::{self, ScopeFile};
use crate::{OnSyntaxError, Options, ScanError, truncate};

/// How a file takes part in the scope.
pub enum Role {
    /// Product source in module `module` (unit of coupling and cycles).
    Product { module: String, context: String },
    /// Test code (never product).
    Test,
    /// Excluded with a reason (generated code).
    Excluded(&'static str),
}

/// A measured product file.
pub struct Unit {
    pub rel: String,
    pub module: String,
    /// Adapter context (the crate name for Rust).
    pub context: String,
    pub source: String,
    pub measure: FileMeasure,
}

/// Per-region coupling an adapter reports: dependencies and local callees.
pub type RegionDeps = (BTreeSet<String>, Vec<String>);

pub trait Adapter: Sync {
    type Scope: Sync;
    const PROFILE: &'static str;
    const SUFFIX: &'static str;
    const EXCLUDES: &'static [&'static str];
    const EMPTY: &'static str;
    /// Role of a file, given the scope root and the walk limit.
    fn role(&self, file: &ScopeFile, root: &Path, limit: &Path, source: &str) -> Role;
    fn measure(&self, rel: &str, source: &str) -> Option<FileMeasure>;
    fn scope(&self, units: &[Unit]) -> Self::Scope;
    fn couple(&self, unit: &Unit, scope: &Self::Scope) -> Option<Vec<RegionDeps>>;
}

fn error(file: &str, stage: &str, kind: &str, message: String) -> FileError {
    FileError {
        file: file.to_owned(),
        stage: Some(stage.to_owned()),
        error: kind.to_owned(),
        message: Some(message),
        error_lines: None,
        sha256: None,
    }
}

fn entry(file: &str, state: &str, reason: Option<&str>) -> InventoryEntry {
    InventoryEntry {
        file: file.to_owned(),
        state: state.to_owned(),
        reason: reason.map(str::to_owned),
    }
}

enum Outcome {
    Product(Unit),
    Skipped(&'static str, Option<&'static str>),
    Failed(FileError),
}

fn read<A: Adapter>(a: &A, f: &ScopeFile, root: &Path, limit: &Path, graded: bool) -> Outcome {
    let raw = match std::fs::read(&f.path) {
        Ok(r) => r,
        Err(e) => {
            return Outcome::Failed(error(
                &f.rel,
                "read",
                "OSError",
                truncate(&crate::os_error(&e, &f.path)),
            ));
        }
    };
    let source = match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => {
            let msg = pytext::utf8_error(e.as_bytes(), &e.utf8_error());
            return Outcome::Failed(error(
                &f.rel,
                "decode",
                "UnicodeDecodeError",
                truncate(&msg),
            ));
        }
    };
    let (module, context) = match a.role(f, root, limit, &source) {
        Role::Test => return Outcome::Skipped("test-only", None),
        Role::Excluded(why) => return Outcome::Skipped("excluded", Some(why)),
        Role::Product { module, context } => (module, context),
    };
    match a.measure(&f.rel, &source) {
        Some(mut measure) => {
            if graded {
                measure.erosion = crate::graded::apply(
                    &f.rel,
                    &source,
                    &mut measure.functions,
                    &mut measure.regions,
                );
            }
            Outcome::Product(Unit {
                rel: f.rel.clone(),
                module,
                context,
                source,
                measure,
            })
        }
        None => Outcome::Failed(error(
            &f.rel,
            "parse",
            "ParseError",
            "parser unavailable".into(),
        )),
    }
}

fn options_map<A: Adapter>(
    options: &Options,
    excludes: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::json!({
        "adapter": options.profile.adapter(A::PROFILE, A::PROFILE), "engine": crate::ENGINE,
        "on_syntax_error": crate::syntax_policy(options),
        "exclude_dirs": crate::sorted(excludes),
        "exclude_globs": crate::sorted(&options.exclude_globs),
        "include_globs": crate::sorted(&options.include_globs),
    }) {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

fn record(u: &Unit, raise: bool, out: &mut Measurement) -> Result<(), ScanError> {
    if !u.measure.error_lines.is_empty() {
        let lines: Vec<u32> = u.measure.error_lines.iter().copied().collect();
        if raise {
            return Err(ScanError::Syntax {
                file: u.rel.clone(),
                message: format!("rows {lines:?}"),
            });
        }
        let message = "best-effort measurement; regions on error lines are unknown".into();
        out.errors.push(FileError {
            error_lines: Some(lines),
            ..error(&u.rel, "parse", "SyntaxError", message)
        });
    }
    out.source_sha256
        .insert(u.rel.clone(), pytext::sha256_hex(u.source.as_bytes()));
    Ok(())
}

fn read_all<A: Adapter>(
    a: &A,
    root: &Path,
    options: &Options,
    out: &mut Measurement,
    inventory: &mut Vec<InventoryEntry>,
) -> Result<Vec<Unit>, ScanError> {
    let excludes: Vec<String> = options
        .exclude_dirs
        .clone()
        .unwrap_or_else(|| A::EXCLUDES.iter().map(|s| (*s).to_owned()).collect());
    out.measurement_options = Some(options_map::<A>(options, &excludes));
    let (candidates, _, walk_errors) = walk::walk(root, &excludes, |n| n.ends_with(A::SUFFIX));
    for e in walk_errors {
        out.errors
            .push(error(&e.rel, "read", "OSError", truncate(&e.message)));
        inventory.push(entry(&e.rel, "unreadable", None));
    }
    let selected: Vec<ScopeFile> = candidates
        .into_iter()
        .filter(|f| walk::globs_keep(&f.rel, &options.exclude_globs, &options.include_globs))
        .collect();
    let limit = root.parent().unwrap_or(root).to_path_buf();
    let outcomes: Vec<Outcome> = selected
        .par_iter()
        .map(|f| {
            read(
                a,
                f,
                root,
                &limit,
                options.profile == crate::Profile::Graded,
            )
        })
        .collect();
    let raise = options.on_syntax_error == OnSyntaxError::Raise;
    let mut units = Vec::new();
    for (f, o) in selected.iter().zip(outcomes) {
        match o {
            Outcome::Skipped(state, reason) => inventory.push(entry(&f.rel, state, reason)),
            Outcome::Failed(e) if raise => {
                return Err(ScanError::Read {
                    file: e.file,
                    message: e.message.unwrap_or_default(),
                });
            }
            Outcome::Failed(e) => {
                inventory.push(entry(&f.rel, "failed", None));
                out.errors.push(e);
            }
            Outcome::Product(u) => {
                record(&u, raise, out)?;
                units.push(u);
            }
        }
    }
    Ok(units)
}

pub fn scan<A: Adapter>(a: &A, root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    let mut out = Measurement {
        measurement_profile: options.profile.adapter(A::PROFILE, A::PROFILE),
        ..Measurement::default()
    };
    let mut inventory = Vec::new();
    let units = read_all(a, root, options, &mut out, &mut inventory)?;
    if units.is_empty() && out.errors.is_empty() {
        return Err(ScanError::empty(A::EMPTY));
    }
    let scope = a.scope(&units);
    let couplings: Vec<Option<Vec<RegionDeps>>> =
        units.par_iter().map(|u| a.couple(u, &scope)).collect();
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (u, c) in units.iter().zip(&couplings) {
        let edges = graph.entry(u.module.clone()).or_default();
        for (deps, _) in c.iter().flatten() {
            edges.extend(deps.iter().filter(|d| **d != u.module).cloned());
        }
    }
    out.static_dependency_cycles = strongly_connected(&graph);
    let in_cycle: HashSet<String> = out
        .static_dependency_cycles
        .iter()
        .flatten()
        .cloned()
        .collect();
    assemble(&mut out, units, couplings, &in_cycle, &mut inventory);
    inventory.sort_by(|a, b| a.file.cmp(&b.file));
    out.inventory = Some(inventory);
    out.errors
        .sort_by(|a, b| (&a.file, &a.error).cmp(&(&b.file, &b.error)));
    out.status = Some(
        if out.components.is_empty() {
            "no-eligible"
        } else {
            "ok"
        }
        .into(),
    );
    out.scope_complete = Some(out.errors.is_empty());
    Ok(out)
}

fn assemble(
    out: &mut Measurement,
    units: Vec<Unit>,
    couplings: Vec<Option<Vec<RegionDeps>>>,
    in_cycle: &HashSet<String>,
    inventory: &mut Vec<InventoryEntry>,
) {
    let (mut exact, mut normalized) = (Grouper::default(), Grouper::default());
    let mut rule_lines: HashMap<String, BTreeSet<u32>> = HashMap::new();
    for (u, deps) in units.into_iter().zip(couplings) {
        let mut m = u.measure;
        for c in &m.clones {
            exact.add(c.exact, &u.rel, &c.body_lines);
            normalized.add(c.normalized, &u.rel, &c.body_lines);
        }
        out.verbosity_findings.append(&mut m.findings);
        rule_lines.insert(u.rel.clone(), std::mem::take(&mut m.rule_lines));
        if m.sloc == 0 {
            inventory.push(entry(&u.rel, "zero-eligible", None));
            continue;
        }
        inventory.push(entry(&u.rel, "measured", None));
        let evidence: Vec<RegionEvidence> = deps
            .unwrap_or_default()
            .into_iter()
            .map(|(d, local_callees)| RegionEvidence {
                deps: d.into_iter().collect(),
                unresolved: Vec::new(),
                local_callees,
            })
            .collect();
        let c = apply(
            &mut m.functions,
            &mut m.regions,
            &evidence,
            in_cycle.contains(&u.module),
        );
        let unknown: u32 = m
            .regions
            .iter()
            .filter(|r| matches!(r.erosion, Evidence::Partial { .. }))
            .map(|r| r.sloc)
            .sum();
        let erosion = if unknown == 0 {
            Evidence::Exact(m.erosion)
        } else {
            Evidence::Partial {
                known_mass: m.erosion,
                unknown_sloc: unknown,
            }
        };
        out.components.push(Component {
            id: u.rel.clone(),
            sloc: m.sloc,
            burden: Burden {
                erosion,
                redundancy: None,
                coupling: Some(c),
            },
        });
        out.functions.append(&mut m.functions);
        out.regions.append(&mut m.regions);
    }
    let mut clone_lines = BTreeMap::new();
    out.clone_groups = exact.finish(&mut clone_lines);
    out.clone_groups_normalized = normalized.finish(&mut clone_lines);
    let empty = BTreeSet::new();
    for c in &mut out.components {
        let lines = clone_lines
            .get(&c.id)
            .unwrap_or(&empty)
            .union(rule_lines.get(&c.id).unwrap_or(&empty))
            .count();
        c.burden.redundancy = Some(u32::try_from(lines).unwrap_or(u32::MAX).min(c.sloc));
    }
}

/// Nearest ancestor directory of `dir` (not above `limit`) containing
/// `marker`, with that file's contents.
pub fn find_up(dir: &Path, limit: &Path, marker: &str) -> Option<(std::path::PathBuf, String)> {
    let mut current = Some(dir);
    while let Some(d) = current {
        if let Ok(text) = std::fs::read_to_string(d.join(marker)) {
            return Some((d.to_path_buf(), text));
        }
        if d == limit {
            break;
        }
        current = d.parent();
    }
    None
}
