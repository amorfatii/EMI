//! Rust scope assembly (profile `emi-rust-0.1`, native).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use emi_ir::clones::Grouper;
use emi_ir::graph::strongly_connected;
use emi_ir::{Burden, Component, Evidence, FileError, InventoryEntry, Measurement, pytext};
use emi_lang_rust::coupling::{self, Scope};
use rayon::prelude::*;

use crate::coupling::{RegionEvidence, apply};
use crate::walk::{self, ScopeFile};
use crate::{OnSyntaxError, Options, ScanError, truncate};

pub const DEFAULT_EXCLUDES: [&str; 8] = [
    "target",
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "vendor",
    ".venv",
    "venv",
];

/// Crate name (underscored) and directory of the nearest `Cargo.toml`
/// with a `[package]` at or above `dir`, not above `limit`.
fn crate_of(dir: &Path, limit: &Path) -> Option<(String, PathBuf)> {
    let mut current = Some(dir);
    while let Some(d) = current {
        if let Ok(toml) = std::fs::read_to_string(d.join("Cargo.toml")) {
            let mut in_package = false;
            for line in toml.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_package = line == "[package]";
                } else if in_package
                    && let Some(v) = line
                        .strip_prefix("name")
                        .map(str::trim_start)
                        .and_then(|l| l.strip_prefix('='))
                {
                    let name = v.trim().trim_matches('"').replace('-', "_");
                    return Some((name, d.to_path_buf()));
                }
            }
        }
        if d == limit {
            break;
        }
        current = d.parent();
    }
    None
}

fn is_test_path(rel_in_crate: &str) -> bool {
    let first = rel_in_crate.split('/').next().unwrap_or("");
    matches!(first, "tests" | "benches" | "examples")
}

struct Unit {
    rel: String,
    module: String,
    krate: String,
    source: String,
    measure: emi_lang_rust::FileMeasure,
}

enum Outcome {
    Product(Unit),
    Test,
    Failed(FileError),
}

fn classify(f: &ScopeFile, root: &Path, limit: &Path) -> (String, String, bool) {
    let dir = f.path.parent().unwrap_or(root);
    match crate_of(dir, limit) {
        Some((krate, crate_dir)) => {
            let rel = f.path.strip_prefix(&crate_dir).map_or_else(
                |_| f.rel.clone(),
                |p| p.to_string_lossy().replace('\\', "/"),
            );
            let test = is_test_path(&rel);
            let module = coupling::module_path(&krate, &rel).unwrap_or_else(|| {
                format!(
                    "{krate}::{}",
                    rel.trim_end_matches(".rs").replace('/', "::")
                )
            });
            (krate, module, test)
        }
        None => {
            let krate = root
                .file_name()
                .map_or_else(|| "root".into(), |n| n.to_string_lossy().replace('-', "_"));
            let module = format!(
                "{krate}::{}",
                f.rel.trim_end_matches(".rs").replace('/', "::")
            );
            (krate, module, false)
        }
    }
}

fn read(f: &ScopeFile, root: &Path, limit: &Path) -> Outcome {
    let (krate, module, test) = classify(f, root, limit);
    if test {
        return Outcome::Test;
    }
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
    match emi_lang_rust::measure(&f.rel, &source) {
        Some(measure) => Outcome::Product(Unit {
            rel: f.rel.clone(),
            module,
            krate,
            source,
            measure,
        }),
        None => Outcome::Failed(error(
            &f.rel,
            "parse",
            "ParseError",
            "parser unavailable".into(),
        )),
    }
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

fn entry(file: &str, state: &str) -> InventoryEntry {
    InventoryEntry {
        file: file.to_owned(),
        state: state.to_owned(),
        reason: None,
    }
}

fn scope_of(units: &[Unit]) -> Scope {
    let mut scope = Scope::default();
    for u in units {
        scope.modules.insert(u.module.clone());
        scope.crates.insert(u.krate.clone());
        scope
            .items
            .insert(u.module.clone(), coupling::catalog(&u.source));
    }
    let reexports: Vec<_> = units
        .par_iter()
        .map(|u| {
            (
                u.module.clone(),
                coupling::reexports(&u.module, &u.krate, &u.source, &scope),
            )
        })
        .collect();
    scope.reexports = reexports.into_iter().collect();
    scope
}

fn measurement_options(
    options: &Options,
    excludes: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::json!({
        "adapter": emi_lang_rust::PROFILE, "engine": crate::ENGINE,
        "on_syntax_error": crate::syntax_policy(options),
        "exclude_dirs": crate::sorted(excludes),
        "exclude_globs": crate::sorted(&options.exclude_globs),
        "include_globs": crate::sorted(&options.include_globs),
    }) {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// Read and measure every selected file.
fn read_all(
    root: &Path,
    options: &Options,
    out: &mut Measurement,
    inventory: &mut Vec<InventoryEntry>,
) -> Result<Vec<Unit>, ScanError> {
    let excludes: Vec<String> = options
        .exclude_dirs
        .clone()
        .unwrap_or_else(|| DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect());
    out.measurement_options = Some(measurement_options(options, &excludes));
    let (candidates, _, walk_errors) = walk::walk(root, &excludes, |n| n.ends_with(".rs"));
    for e in walk_errors {
        out.errors
            .push(error(&e.rel, "read", "OSError", truncate(&e.message)));
        inventory.push(entry(&e.rel, "unreadable"));
    }
    let selected: Vec<ScopeFile> = candidates
        .into_iter()
        .filter(|f| walk::globs_keep(&f.rel, &options.exclude_globs, &options.include_globs))
        .collect();
    let limit = root.parent().unwrap_or(root).to_path_buf();
    let outcomes: Vec<Outcome> = selected.par_iter().map(|f| read(f, root, &limit)).collect();
    let raise = options.on_syntax_error == OnSyntaxError::Raise;
    let mut units = Vec::new();
    for (f, o) in selected.iter().zip(outcomes) {
        match o {
            Outcome::Test => inventory.push(entry(&f.rel, "test-only")),
            Outcome::Failed(e) if raise => {
                return Err(ScanError::Read {
                    file: e.file,
                    message: e.message.unwrap_or_default(),
                });
            }
            Outcome::Failed(e) => {
                inventory.push(entry(&f.rel, "failed"));
                out.errors.push(e);
            }
            Outcome::Product(u) => {
                if !u.measure.error_lines.is_empty() {
                    let lines: Vec<u32> = u.measure.error_lines.iter().copied().collect();
                    if raise {
                        return Err(ScanError::Syntax {
                            file: u.rel,
                            message: format!("rows {lines:?}"),
                        });
                    }
                    out.errors.push(FileError {
                        error_lines: Some(lines),
                        ..error(
                            &u.rel,
                            "parse",
                            "SyntaxError",
                            "best-effort measurement; regions on error lines are unknown".into(),
                        )
                    });
                }
                out.source_sha256
                    .insert(u.rel.clone(), pytext::sha256_hex(u.source.as_bytes()));
                units.push(u);
            }
        }
    }
    Ok(units)
}

pub fn scan(root: &Path, options: &Options) -> Result<Measurement, ScanError> {
    let mut out = Measurement {
        measurement_profile: emi_lang_rust::PROFILE.to_owned(),
        ..Measurement::default()
    };
    let mut inventory = Vec::new();
    let units = read_all(root, options, &mut out, &mut inventory)?;
    if units.is_empty() && out.errors.is_empty() {
        return Err(ScanError::empty("no Rust source files"));
    }
    let scope = scope_of(&units);
    let couplings: Vec<Option<coupling::FileCoupling>> = units
        .par_iter()
        .map(|u| coupling::couple(&u.module, &u.krate, &u.source, &scope))
        .collect();
    let graph: BTreeMap<String, BTreeSet<String>> = units
        .iter()
        .zip(&couplings)
        .map(|(u, c)| {
            (
                u.module.clone(),
                c.as_ref().map(|c| c.graph.clone()).unwrap_or_default(),
            )
        })
        .collect();
    out.static_dependency_cycles = strongly_connected(&graph);
    let in_cycle: std::collections::HashSet<&String> =
        out.static_dependency_cycles.iter().flatten().collect();
    let in_cycle: std::collections::HashSet<String> = in_cycle.into_iter().cloned().collect();
    assemble(&mut out, units, couplings, &in_cycle, &mut inventory);
    inventory.sort_by(|a, b| a.file.cmp(&b.file));
    out.inventory = Some(inventory);
    out.errors
        .sort_by(|a, b| (&a.file, &a.error).cmp(&(&b.file, &b.error)));
    let complete = out.errors.is_empty();
    out.status = Some(
        if out.components.is_empty() {
            "no-eligible"
        } else {
            "ok"
        }
        .into(),
    );
    out.scope_complete = Some(complete);
    Ok(out)
}

fn assemble(
    out: &mut Measurement,
    units: Vec<Unit>,
    couplings: Vec<Option<coupling::FileCoupling>>,
    in_cycle: &std::collections::HashSet<String>,
    inventory: &mut Vec<InventoryEntry>,
) {
    let (mut exact, mut normalized) = (Grouper::default(), Grouper::default());
    let mut rule_lines: HashMap<String, BTreeSet<u32>> = HashMap::new();
    for (u, fc) in units.into_iter().zip(couplings) {
        let mut m = u.measure;
        for c in &m.clones {
            exact.add(c.exact, &u.rel, &c.body_lines);
            normalized.add(c.normalized, &u.rel, &c.body_lines);
        }
        out.verbosity_findings.append(&mut m.findings);
        rule_lines.insert(u.rel.clone(), std::mem::take(&mut m.rule_lines));
        if m.sloc == 0 {
            inventory.push(entry(&u.rel, "zero-eligible"));
            continue;
        }
        inventory.push(entry(&u.rel, "measured"));
        let evidence: Vec<RegionEvidence> = fc
            .map(|fc| fc.regions)
            .unwrap_or_default()
            .into_iter()
            .map(|r| RegionEvidence {
                deps: r.deps.into_iter().collect(),
                unresolved: Vec::new(),
                local_callees: r.local_callees,
            })
            .collect();
        let c = apply(
            &mut m.functions,
            &mut m.regions,
            &evidence,
            in_cycle.contains(&u.module),
        );
        let erosion = if m.error_lines.is_empty() {
            Evidence::Exact(m.erosion)
        } else {
            let unknown: u32 = m
                .regions
                .iter()
                .filter(|r| matches!(r.erosion, Evidence::Partial { .. }))
                .map(|r| r.sloc)
                .sum();
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
