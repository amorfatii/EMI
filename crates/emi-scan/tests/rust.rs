//! Rust scope assembly on a two-crate workspace.

use std::path::PathBuf;

use emi_scan::{Options, Probe, scan};

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Two crates: `core-lib` re-exports `Thing`; `app`'s `a` and `b` depend
/// on each other; `app/tests` is an integration test.
fn workspace() -> Tree {
    let root = Tree(std::env::temp_dir().join(format!("emi-rust-scan-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let files = [
        (
            "crates/core-lib/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\n",
        ),
        (
            "crates/core-lib/src/lib.rs",
            "pub mod model;\npub use model::Thing;\n",
        ),
        (
            "crates/core-lib/src/model.rs",
            "pub struct Thing;\npub fn make() -> Thing { Thing }\n",
        ),
        (
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\ncore-lib = { path = \"../core-lib\" }\n",
        ),
        (
            "crates/app/src/main.rs",
            "mod a;\nmod b;\nfn main() { a::run(); }\n",
        ),
        (
            "crates/app/src/a.rs",
            "use core_lib::Thing;\npub fn run() -> Thing { crate::b::helper(); core_lib::model::make() }\npub fn back() {}\n",
        ),
        (
            "crates/app/src/b.rs",
            "pub fn helper() { super::a::back(); }\n",
        ),
        (
            "crates/app/tests/it.rs",
            "#[test]\nfn it() { assert!(true); }\n",
        ),
    ];
    for (rel, text) in files {
        let p = root.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap_or(&root.0)).unwrap_or_default();
        std::fs::write(p, text).unwrap_or_default();
    }
    root
}

#[test]
fn workspace_crates_modules_tests_and_cycles() {
    let root = workspace();
    let m =
        scan(&root.0.join("crates"), &Options::new(Probe::Rust)).unwrap_or_else(|e| panic!("{e}"));
    let inventory: Vec<(String, String)> = m
        .inventory
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.file, e.state))
        .collect();
    assert!(
        inventory.contains(&("app/tests/it.rs".into(), "test-only".into())),
        "{inventory:?}"
    );
    let run = m
        .functions
        .iter()
        .find(|f| f.function == "run")
        .cloned()
        .unwrap_or_else(|| panic!("no run"));
    let deps = run
        .coupling
        .map(|c| c.resolved_dependencies)
        .unwrap_or_default();
    assert_eq!(
        deps,
        ["app::b", "core_lib::model"],
        "cross-crate path through a re-export"
    );
    assert_eq!(
        m.static_dependency_cycles,
        vec![vec!["app::a".to_owned(), "app::b".to_owned()]]
    );
    let a = m
        .components
        .iter()
        .find(|c| c.id == "app/src/a.rs")
        .cloned()
        .unwrap_or_else(|| panic!("no a.rs"));
    assert_eq!(
        a.burden.coupling,
        Some(emi_ir::Evidence::Exact(f64::from(a.sloc))),
        "cycle members carry full C"
    );
}
