//! Rust adapter semantics.

use std::collections::HashSet;

use emi_lang_rust::coupling::{Scope, couple, module_path, reexports};
use emi_lang_rust::measure;

fn find<'a>(m: &'a emi_lang_rust::FileMeasure, name: &str) -> &'a emi_ir::FunctionRecord {
    m.functions
        .iter()
        .find(|f| f.function == name)
        .unwrap_or_else(|| panic!("no region {name}"))
}

#[test]
fn complexity_counts_rust_branches() {
    let src = r#"
fn f(x: Option<u32>, y: bool) -> Result<u32, E> {
    let v = g()?;
    if let Some(a) = x {
        if a > 1 && y || v == 0 { return Ok(a); } else if a == 0 { return Ok(0); }
    }
    for i in 0..3 { while y { break; } }
    loop { break; }
    match x { Some(1) => 1, Some(_) => 2, None => 3 };
    let c = |z: u32| if z > 0 { z } else { 0 };
    Ok(c(v))
}
"#;
    let m = measure("a.rs", src).unwrap_or_else(|| panic!("parse"));
    // 1 + ? + if-let + if + && + || + else-if + for + while + 2 extra arms + closure if = 12
    assert_eq!(find(&m, "f").cc, Some(12));
}

#[test]
fn test_code_is_not_product() {
    let src = "pub fn lib() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t() { assert_eq!(lib(), 1); }\n}\n\n#[test]\nfn loose() {}\n";
    let m = measure("a.rs", src).unwrap_or_else(|| panic!("parse"));
    assert_eq!(m.sloc, 1);
    assert!(
        m.functions
            .iter()
            .all(|f| f.function != "t" && f.function != "loose")
    );
}

#[test]
fn regions_are_named_by_item() {
    let src = "struct S { a: u8 }\nimpl S {\n    fn m(&self) -> u8 { self.a }\n}\nimpl std::fmt::Display for S {\n    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, \"s\") }\n}\nmod inner {\n    pub fn deep() {}\n}\nconst K: u8 = 1;\n";
    let m = measure("a.rs", src).unwrap_or_else(|| panic!("parse"));
    let ids: HashSet<&str> = m.regions.iter().map(|r| r.id.as_str()).collect();
    for id in [
        "S",
        "impl S",
        "S.m",
        "impl Display for S",
        "<S as Display>.fmt",
        "inner",
        "inner.deep",
        "K",
    ] {
        assert!(ids.contains(id), "{id} missing from {ids:?}");
    }
    assert!(
        m.erosion.abs() < 1e-9,
        "declarations and short functions carry no erosion"
    );
}

#[test]
fn module_paths_follow_cargo_layout() {
    assert_eq!(module_path("k", "src/lib.rs").as_deref(), Some("k"));
    assert_eq!(module_path("k", "src/main.rs").as_deref(), Some("k"));
    assert_eq!(module_path("k", "src/a/mod.rs").as_deref(), Some("k::a"));
    assert_eq!(module_path("k", "src/a/b.rs").as_deref(), Some("k::a::b"));
    assert_eq!(module_path("k", "tests/it.rs"), None);
}

fn scope() -> Scope {
    let mut s = Scope::default();
    for m in [
        "app",
        "app::db",
        "app::db::pool",
        "app::util",
        "core",
        "core::model",
    ] {
        s.modules.insert(m.into());
    }
    s.crates.extend(["app".to_owned(), "core".to_owned()]);
    s.items
        .insert("app::util".into(), ["helper".to_owned()].into());
    s.items
        .insert("core::model".into(), ["Thing".to_owned()].into());
    s.items.insert("core".into(), HashSet::new());
    let lib = "pub use model::Thing;\n";
    let r = reexports("core", "core", lib, &s);
    s.reexports.insert("core".into(), r);
    s
}

#[test]
fn paths_resolve_through_uses_globs_macros_and_reexports() {
    let s = scope();
    let src = r#"
use crate::db::{self, pool as p};
use crate::util::*;
use core::Thing;
use std::collections::HashMap;

pub fn a() { db::open(); }
pub fn b() { p::get(); }
pub fn c() { helper(); }
pub fn d(x: Thing) -> HashMap<u8, u8> { HashMap::new() }
pub fn e() { println!("{}", super::util::helper()); }
pub fn f(v: Vec<u8>) { v.iter().count(); local(); }
fn local() {}
"#;
    let fc = couple("app::api", "app", src, &s).unwrap_or_else(|| panic!("parse"));
    let m = measure("api.rs", src).unwrap_or_else(|| panic!("parse"));
    let deps = |name: &str| {
        let i = m
            .functions
            .iter()
            .position(|f| f.function == name)
            .unwrap_or_else(|| panic!("{name}"));
        fc.regions[i].deps.iter().cloned().collect::<Vec<_>>()
    };
    assert_eq!(deps("a"), ["app::db"], "use a::{{self}}");
    assert_eq!(deps("b"), ["app::db::pool"], "renamed group member");
    assert_eq!(deps("c"), ["app::util"], "glob import of a known item");
    assert_eq!(
        deps("d"),
        ["core::model"],
        "re-export followed to the defining module; std is external"
    );
    assert_eq!(
        deps("e"),
        ["app::util"],
        "super:: path inside a macro token tree"
    );
    assert!(deps("f").is_empty(), "method calls are dispatch");
    let i = m
        .functions
        .iter()
        .position(|f| f.function == "f")
        .unwrap_or(0);
    assert_eq!(fc.regions[i].local_callees, ["local"]);
}
