//! Second pass of the Python scan: scope-wide catalog, import graph, static
//! cycles, and per-region coupling evidence (C).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use emi_ir::{CouplingRecord, Evidence, UnresolvedCall, burden};
use emi_lang_python::coupling::{self, Project};
use emi_lang_python::{FileCoupling, FileMeasure};
use rayon::prelude::*;

use crate::ScanError;

/// A parsed product (non-test) file awaiting coupling.
pub struct Product {
    pub rel: String,
    pub measure: FileMeasure,
    pub source: String,
}

impl Product {
    fn module(&self) -> String {
        coupling::module_name(&self.rel)
    }

    fn is_package(&self) -> bool {
        self.rel.rsplit('/').next() == Some("__init__.py")
    }
}

pub struct Coupled {
    pub files: Vec<FileCoupling>,
    pub cycles: Vec<Vec<String>>,
    pub cycle_modules: HashSet<String>,
}

/// Build the catalog from every product module, couple each file, and find
/// cycles in the import graph.
pub fn couple_all(products: &mut [Product]) -> Result<Coupled, ScanError> {
    let mut project = Project::default();
    for p in products.iter_mut() {
        let module = p.module();
        project.add(&module, std::mem::take(&mut p.measure.catalog));
    }
    let files = products
        .par_iter()
        .map(|p| {
            emi_lang_python::couple(&p.source, &p.module(), p.is_package(), &project).map_err(|e| {
                ScanError::Syntax {
                    file: p.rel.clone(),
                    message: e.message,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (p, fc) in products.iter().zip(&files) {
        graph.insert(p.module(), fc.import_deps.clone());
    }
    let cycles = coupling::strongly_connected(&graph);
    let cycle_modules = cycles.iter().flatten().cloned().collect();
    Ok(Coupled {
        files,
        cycles,
        cycle_modules,
    })
}

fn unresolved_calls(rc: &coupling::RegionCoupling) -> Vec<UnresolvedCall> {
    rc.unresolved
        .iter()
        .map(|u| UnresolvedCall {
            line: u.line,
            column: u.column.map(|c| u32::try_from(c).unwrap_or(u32::MAX)),
            reason: u.reason.to_owned(),
        })
        .collect()
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Attach coupling evidence to a file's records; returns the component's C.
pub fn apply(measure: &mut FileMeasure, fc: &FileCoupling, in_cycle: bool) -> Evidence {
    let (mut known, mut unknown_sloc) = (0.0, 0u32);
    for ((func, region), rc) in measure
        .functions
        .iter_mut()
        .zip(measure.regions.iter_mut())
        .zip(&fc.regions)
    {
        let sloc = region.sloc;
        let evidence = if in_cycle {
            known += f64::from(sloc);
            Evidence::Exact(f64::from(sloc))
        } else if !rc.unresolved.is_empty() {
            unknown_sloc += sloc;
            Evidence::Partial {
                known_mass: 0.0,
                unknown_sloc: sloc,
            }
        } else {
            let mass = burden::coupling_mass(sloc, count(rc.deps.len()), 0);
            known += mass;
            Evidence::Exact(mass)
        };
        region.in_cycle = Some(in_cycle);
        region.coupling = Some(evidence);
        func.coupling = Some(CouplingRecord {
            resolved_fanout: count(rc.deps.len()),
            resolved_dependencies: rc.deps.iter().cloned().collect(),
            local_callees: rc.local_callees.clone(),
            coupling_unknown: !rc.unresolved.is_empty() && !in_cycle,
            unresolved_calls: unresolved_calls(rc),
            in_static_cycle: in_cycle,
            local_callers: Vec::new(),
        });
    }
    fill_local_callers(&mut measure.functions);
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
pub fn fill_local_callers(functions: &mut [emi_ir::FunctionRecord]) {
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
            .map(|(caller, _)| caller.clone())
            .collect();
        callers.sort();
        if let Some(c) = f.coupling.as_mut() {
            c.local_callers = callers;
        }
    }
}
