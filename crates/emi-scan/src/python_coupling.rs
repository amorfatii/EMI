//! Second pass of the Python scan: scope-wide catalog, import graph, static
//! cycles, and per-region coupling evidence (C).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use emi_ir::UnresolvedCall;

use crate::coupling::RegionEvidence;
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

/// Adapter-neutral evidence of a Python file's regions.
pub fn evidence(fc: &FileCoupling) -> Vec<RegionEvidence> {
    fc.regions
        .iter()
        .map(|rc| RegionEvidence {
            deps: rc.deps.iter().cloned().collect(),
            unresolved: rc
                .unresolved
                .iter()
                .map(|u| UnresolvedCall {
                    line: u.line,
                    column: u.column.map(|c| u32::try_from(c).unwrap_or(u32::MAX)),
                    reason: u.reason.to_owned(),
                })
                .collect(),
            local_callees: rc.local_callees.clone(),
        })
        .collect()
}
