//! Coupling evidence (C) attached to measured records, shared by adapters.

use std::collections::{BTreeMap, BTreeSet};

use emi_ir::{CouplingRecord, Evidence, FunctionRecord, RegionRecord, UnresolvedCall, burden};

/// One region's coupling facts in adapter-neutral form.
pub struct RegionEvidence {
    pub deps: Vec<String>,
    pub unresolved: Vec<UnresolvedCall>,
    pub local_callees: Vec<String>,
}

/// The module graph restated between files: every `(file, module, imported
/// modules)` becomes edges to the files of those modules. Imports of
/// modules outside the scope have no file and no edge.
pub fn file_edges<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str, &'a BTreeSet<String>)> + Clone,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut files_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (rel, module, _) in files.clone() {
        files_of.entry(module).or_default().push(rel);
    }
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (rel, _, deps) in files {
        let edges = out.entry(rel.to_owned()).or_default();
        for dep in deps {
            let targets = files_of.get(dep.as_str()).into_iter().flatten();
            edges.extend(targets.filter(|t| **t != rel).map(|t| (*t).to_owned()));
        }
    }
    out
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Attach coupling evidence to a file's records; returns the component's C.
/// Cycle members count every line; unresolved calls make a region unknown;
/// otherwise mass follows direct fan-out.
pub fn apply(
    functions: &mut [FunctionRecord],
    regions: &mut [RegionRecord],
    evidence: &[RegionEvidence],
    in_cycle: bool,
) -> Evidence {
    let (mut known, mut unknown_sloc) = (0.0, 0u32);
    for ((func, region), ev) in functions.iter_mut().zip(regions.iter_mut()).zip(evidence) {
        let sloc = region.sloc;
        let c = if in_cycle {
            known += f64::from(sloc);
            Evidence::Exact(f64::from(sloc))
        } else if !ev.unresolved.is_empty() {
            unknown_sloc += sloc;
            Evidence::Partial {
                known_mass: 0.0,
                unknown_sloc: sloc,
            }
        } else {
            let mass = burden::coupling_mass(sloc, count(ev.deps.len()), 0);
            known += mass;
            Evidence::Exact(mass)
        };
        region.in_cycle = Some(in_cycle);
        region.coupling = Some(c);
        func.coupling = Some(CouplingRecord {
            resolved_fanout: count(ev.deps.len()),
            resolved_dependencies: ev.deps.clone(),
            local_callees: ev.local_callees.clone(),
            coupling_unknown: !ev.unresolved.is_empty() && !in_cycle,
            unresolved_calls: ev.unresolved.clone(),
            in_static_cycle: in_cycle,
            local_callers: Vec::new(),
        });
    }
    fill_local_callers(functions);
    if unknown_sloc > 0 {
        Evidence::Partial {
            known_mass: known,
            unknown_sloc,
        }
    } else {
        Evidence::Exact(known)
    }
}

/// Fan-in within one file by bare name (a documented approximation).
fn fill_local_callers(functions: &mut [FunctionRecord]) {
    let callees: Vec<(String, Vec<String>)> = functions
        .iter()
        .map(|f| {
            (
                f.function.clone(),
                f.coupling
                    .as_ref()
                    .map(|c| c.local_callees.clone())
                    .unwrap_or_default(),
            )
        })
        .collect();
    for f in functions.iter_mut() {
        let mut callers: Vec<String> = callees
            .iter()
            .filter(|(_, called)| called.contains(&f.function))
            .map(|(c, _)| c.clone())
            .collect();
        callers.sort();
        if let Some(c) = f.coupling.as_mut() {
            c.local_callers = callers;
        }
    }
}
