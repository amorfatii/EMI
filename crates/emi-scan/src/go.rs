//! Go scope (profile `emi-go-0.1`): packages from `go.mod` and
//! directories, `_test.go` as test code, generated files excluded.

use std::path::Path;

use emi_lang_go::coupling::{self, Scope};

use crate::native::{Adapter, RegionDeps, Role, Unit, find_up};
use crate::walk::ScopeFile;

pub struct Go;

impl Adapter for Go {
    type Scope = Scope;
    const PROFILE: &'static str = emi_lang_go::PROFILE;
    const SUFFIX: &'static str = ".go";
    const EXCLUDES: &'static [&'static str] = &[
        "vendor",
        "testdata",
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "third_party",
    ];
    const EMPTY: &'static str = "no Go source files";

    fn role(&self, f: &ScopeFile, root: &Path, limit: &Path, source: &str) -> Role {
        if f.rel.ends_with("_test.go") {
            return Role::Test;
        }
        if emi_lang_go::is_generated(source) {
            return Role::Excluded("generated");
        }
        if emi_lang_go::is_build_ignored(source) {
            return Role::Excluded("build-ignored");
        }
        let dir = f.path.parent().unwrap_or(root);
        let (module, mod_dir) = match find_up(dir, limit, "go.mod")
            .and_then(|(d, text)| coupling::module_of(&text).map(|m| (m, d)))
        {
            Some(found) => found,
            None => (
                root.file_name()
                    .map_or_else(|| "root".into(), |n| n.to_string_lossy().into_owned()),
                root.to_path_buf(),
            ),
        };
        let rel_dir = dir.strip_prefix(&mod_dir).map_or_else(
            |_| String::new(),
            |p| p.to_string_lossy().replace('\\', "/"),
        );
        Role::Product {
            module: coupling::package_path(&module, &rel_dir),
            context: module,
        }
    }

    fn measure(&self, rel: &str, source: &str) -> Option<emi_lang_common::FileMeasure> {
        emi_lang_go::measure(rel, source)
    }

    fn scope(&self, units: &[Unit]) -> Scope {
        let mut scope = Scope::default();
        for u in units {
            scope.packages.insert(u.module.clone());
            scope
                .exported
                .entry(u.module.clone())
                .or_default()
                .extend(coupling::exported(&u.source));
        }
        scope
    }

    fn couple(&self, u: &Unit, scope: &Scope) -> Option<Vec<RegionDeps>> {
        let fc = coupling::couple(&u.module, &u.source, scope)?;
        Some(
            fc.regions
                .into_iter()
                .map(|r| (r.deps, r.local_callees))
                .collect(),
        )
    }
}
