//! Package coupling for Go.
//!
//! A package is a directory; its import path is the `go.mod` module path
//! plus the directory. A region depends on the first-party packages it
//! names: `pkg.Name` selectors and qualified types through an import
//! (default name, alias), and unqualified exported names of dot imports.
//! Blank imports (`import _ "p"`) are dependencies of the importing file's
//! module region: they run `p`'s initialization.
//! Method calls through values are dispatch. Go forbids import cycles, so
//! static cycles never occur.

use std::collections::{BTreeSet, HashMap, HashSet};

use emi_lang_common::{children, owner_index, row, text};
use tree_sitter::Node;

#[derive(Debug, Default)]
pub struct Scope {
    /// First-party package import paths.
    pub packages: HashSet<String>,
    /// Exported top-level names per package (for dot imports).
    pub exported: HashMap<String, HashSet<String>>,
}

/// Package import path of a file: `module` + directory relative to the
/// `go.mod` directory.
pub fn package_path(module: &str, rel_dir: &str) -> String {
    match (module, rel_dir.is_empty() || rel_dir == ".") {
        // The standard library's import paths carry no module prefix.
        ("std", true) => "std".to_owned(),
        ("std", false) => rel_dir.to_owned(),
        (_, true) => module.to_owned(),
        (_, false) => format!("{module}/{rel_dir}"),
    }
}

/// The `module` line of a `go.mod`.
pub fn module_of(go_mod: &str) -> Option<String> {
    go_mod
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("module "))
        .map(|m| m.trim().trim_matches('"').to_owned())
}

/// Default local name of an import path (`.../v2` -> previous element).
fn default_name(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let last = parts.next().unwrap_or(path);
    let versioned =
        last.len() > 1 && last.starts_with('v') && last[1..].chars().all(|c| c.is_ascii_digit());
    let name = if versioned {
        parts.next().unwrap_or(last)
    } else {
        last
    };
    name.replace(['-', '.'], "_")
}

/// Exported (capitalized) top-level names of a file.
pub fn exported(source: &str) -> HashSet<String> {
    let Some(tree) = crate::parse(source) else {
        return HashSet::new();
    };
    let src = source.as_bytes();
    let mut out = HashSet::new();
    let mut stack = children(tree.root_node());
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_declaration" | "type_spec" | "const_spec" | "var_spec" | "type_alias" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let t = text(name, src);
                    if t.chars().next().is_some_and(char::is_uppercase) {
                        out.insert(t.to_owned());
                    }
                }
            }
            "type_declaration" | "const_declaration" | "var_declaration" => {
                stack.extend(children(n))
            }
            _ => {}
        }
    }
    out
}

fn string_value(node: Node, src: &[u8]) -> String {
    text(node, src).trim_matches(['"', '`']).to_owned()
}

/// Imports of a file.
#[derive(Default)]
struct Imports {
    /// `alias -> import path`.
    named: HashMap<String, String>,
    /// Dot-imported paths.
    dots: Vec<String>,
    /// Blank (`_`) imports: side-effect dependencies, by the row they sit on.
    blank: Vec<(u32, String)>,
}

fn imports(root: Node, src: &[u8]) -> Imports {
    let mut out = Imports::default();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_spec" {
            let path = n
                .child_by_field_name("path")
                .map(|p| string_value(p, src))
                .unwrap_or_default();
            match n.child_by_field_name("name") {
                Some(a) if a.kind() == "dot" => out.dots.push(path),
                Some(a) if a.kind() == "blank_identifier" => {
                    out.blank.push((row(n.start_position().row), path))
                }
                Some(a) => {
                    out.named.insert(text(a, src).to_owned(), path);
                }
                None => {
                    out.named.insert(default_name(&path), path);
                }
            }
            continue;
        }
        stack.extend(children(n));
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct RegionDeps {
    pub deps: BTreeSet<String>,
    pub local_callees: Vec<String>,
}

pub struct FileCoupling {
    pub graph: BTreeSet<String>,
    pub regions: Vec<RegionDeps>,
}

/// Coupling of a file in package `package`.
pub fn couple(package: &str, source: &str, scope: &Scope) -> Option<FileCoupling> {
    let tree = crate::parse(source)?;
    let src = source.as_bytes();
    let Imports { named, dots, blank } = imports(tree.root_node(), src);
    let (regions, _) = emi_lang_common::regions(&tree, &crate::SPEC, src, &BTreeSet::new());
    let owner = owner_index(&regions);
    let fn_names: HashSet<String> = regions.iter().map(|r| r.name.clone()).collect();
    let first_party = |p: &String| scope.packages.contains(p) && p != package;
    let mut per_region = vec![RegionDeps::default(); regions.len()];
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        let Some(&i) = owner.get(&row(n.start_position().row)) else {
            stack.extend(children(n));
            continue;
        };
        match n.kind() {
            "import_declaration" | "comment" => continue,
            "selector_expression" | "qualified_type" => {
                let operand = n
                    .child_by_field_name("operand")
                    .or_else(|| n.child_by_field_name("package"));
                if let Some(path) = operand
                    .and_then(|o| named.get(text(o, src)))
                    .filter(|p| first_party(p))
                {
                    per_region[i].deps.insert(path.clone());
                }
            }
            "identifier" | "type_identifier" => {
                let name = text(n, src);
                if let Some(path) = dots.iter().find(|d| {
                    first_party(d) && scope.exported.get(*d).is_some_and(|e| e.contains(name))
                }) {
                    per_region[i].deps.insert(path.clone());
                } else if fn_names.contains(name)
                    && n.parent()
                        .and_then(|p| p.child_by_field_name("name"))
                        .is_none_or(|d| d.id() != n.id())
                {
                    per_region[i].local_callees.push(name.to_owned());
                }
            }
            _ => {}
        }
        stack.extend(children(n));
    }
    for (r, path) in blank {
        if let Some(&i) = owner.get(&r).filter(|_| first_party(&path)) {
            per_region[i].deps.insert(path);
        }
    }
    for r in &mut per_region {
        r.local_callees.sort();
        r.local_callees.dedup();
    }
    let graph = per_region
        .iter()
        .flat_map(|r| r.deps.iter().cloned())
        .collect();
    Some(FileCoupling {
        graph,
        regions: per_region,
    })
}
