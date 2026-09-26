//! Rust scope (profile `emi-rust-0.1`): crates from `Cargo.toml`, modules
//! from the Cargo layout, `tests`/`benches`/`examples` as test code.

use std::path::Path;

use emi_lang_rust::coupling::{self, Scope};
use rayon::prelude::*;

use crate::native::{Adapter, RegionDeps, Role, Unit, find_up};
use crate::walk::ScopeFile;

pub struct Rust;

/// `[package] name` of a `Cargo.toml`, underscored.
fn package_name(toml: &str) -> Option<String> {
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
            return Some(v.trim().trim_matches('"').replace('-', "_"));
        }
    }
    None
}

/// The crate of a directory: the nearest `Cargo.toml` with a package.
fn crate_of(dir: &Path, limit: &Path) -> Option<(String, std::path::PathBuf)> {
    let mut current = Some(dir.to_path_buf());
    while let Some(d) = current {
        let (found, toml) = find_up(&d, limit, "Cargo.toml")?;
        if let Some(name) = package_name(&toml) {
            return Some((name, found));
        }
        if found == limit {
            return None;
        }
        current = found.parent().map(Path::to_path_buf);
    }
    None
}

impl Adapter for Rust {
    type Scope = Scope;
    const PROFILE: &'static str = emi_lang_rust::PROFILE;
    const SUFFIX: &'static str = ".rs";
    const EXCLUDES: &'static [&'static str] = &[
        "target",
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "vendor",
        ".venv",
        "venv",
    ];
    const EMPTY: &'static str = "no Rust source files";

    fn role(&self, f: &ScopeFile, root: &Path, limit: &Path, _source: &str) -> Role {
        let dir = f.path.parent().unwrap_or(root);
        let Some((krate, crate_dir)) = crate_of(dir, limit) else {
            let krate = root
                .file_name()
                .map_or_else(|| "root".into(), |n| n.to_string_lossy().replace('-', "_"));
            let module = format!(
                "{krate}::{}",
                f.rel.trim_end_matches(".rs").replace('/', "::")
            );
            return Role::Product {
                module,
                context: krate,
            };
        };
        let rel = f.path.strip_prefix(&crate_dir).map_or_else(
            |_| f.rel.clone(),
            |p| p.to_string_lossy().replace('\\', "/"),
        );
        if matches!(
            rel.split('/').next(),
            Some("tests" | "benches" | "examples")
        ) {
            return Role::Test;
        }
        let module = coupling::module_path(&krate, &rel).unwrap_or_else(|| {
            format!(
                "{krate}::{}",
                rel.trim_end_matches(".rs").replace('/', "::")
            )
        });
        Role::Product {
            module,
            context: krate,
        }
    }

    fn measure(&self, rel: &str, source: &str) -> Option<emi_lang_common::FileMeasure> {
        emi_lang_rust::measure(rel, source)
    }

    fn scope(&self, units: &[Unit]) -> Scope {
        let mut scope = Scope::default();
        for u in units {
            scope.modules.insert(u.module.clone());
            scope.crates.insert(u.context.clone());
            scope
                .items
                .insert(u.module.clone(), coupling::catalog(&u.source));
        }
        let reexports: Vec<_> = units
            .par_iter()
            .map(|u| {
                (
                    u.module.clone(),
                    coupling::reexports(&u.module, &u.context, &u.source, &scope),
                )
            })
            .collect();
        scope.reexports = reexports.into_iter().collect();
        scope
    }

    fn couple(&self, u: &Unit, scope: &Scope) -> Option<Vec<RegionDeps>> {
        let fc = coupling::couple(&u.module, &u.context, &u.source, scope)?;
        Some(
            fc.regions
                .into_iter()
                .map(|r| (r.deps, r.local_callees))
                .collect(),
        )
    }
}
