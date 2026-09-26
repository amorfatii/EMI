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
    /// Precise profile: dotted package path of the scope root when the root
    /// is itself a package (`warehouse/` makes `warehouse.accounts.models`).
    pub prefix: String,
}

impl Product {
    pub fn module(&self) -> String {
        let local = coupling::module_name(&self.rel);
        match (self.prefix.is_empty(), local.is_empty()) {
            (true, _) => local,
            (false, true) => self.prefix.clone(),
            (false, false) => format!("{}.{local}", self.prefix),
        }
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

/// Dotted package path of `root` when it (and possibly its parents) are
/// packages: the names imports use for first-party modules.
pub fn package_prefix(root: &std::path::Path) -> String {
    let mut parts = Vec::new();
    let mut dir = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    while dir.join("__init__.py").is_file() {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            break;
        };
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            break;
        }
        parts.push(name.to_owned());
        if !dir.pop() {
            break;
        }
    }
    parts.reverse();
    parts.join(".")
}

/// Build the catalog from every product module, couple each file, and find
/// cycles in the import graph.
pub fn couple_all(products: &mut [Product], precise: bool) -> Result<Coupled, ScanError> {
    let mut project = Project {
        precise,
        ..Project::default()
    };
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
