//! Static module coupling for Rust.
//!
//! A region depends on the first-party modules its code names: paths
//! through `use` bindings (renames, groups, globs), `crate::`/`self::`/
//! `super::` paths, and paths into other workspace crates — including
//! paths inside macro invocations, reconstructed from token trees. Method
//! calls through values are dynamic dispatch (as in the JS/Python precise
//! profile): the dependency belongs to where the value was obtained.
//! Rust needs no unknowns here: every name is either resolved, local,
//! external (std or a registry crate), or a prelude/builtin.

use std::collections::{BTreeSet, HashMap, HashSet};

use emi_ir::regions::{self, Region};
use tree_sitter::Node;

use crate::{children, row};

/// Scope-wide knowledge.
#[derive(Debug, Default)]
pub struct Scope {
    /// Module paths (`crate::a::b` spelled with the crate name).
    pub modules: HashSet<String>,
    /// First-party crate names (underscored).
    pub crates: HashSet<String>,
    /// Top-level item names per module (for glob imports).
    pub items: HashMap<String, HashSet<String>>,
    /// `pub use` re-exports per module: name -> absolute path, and globs.
    pub reexports: HashMap<String, Reexports>,
}

/// A module's `pub use` re-exports: name -> absolute path, and globs.
pub type Reexports = (HashMap<String, Vec<String>>, Vec<String>);

/// `use` bindings `(alias, path)` and glob prefixes, unresolved.
type Bindings = (Vec<(String, Vec<String>)>, Vec<Vec<String>>);

/// Module path of a file inside a crate (`src/a/b.rs` -> `krate::a::b`).
/// `None` for files outside `src/` (tests, benches, examples, build.rs).
pub fn module_path(krate: &str, rel_in_crate: &str) -> Option<String> {
    let inner = rel_in_crate.strip_prefix("src/")?;
    let stem = inner.strip_suffix(".rs")?;
    let mut parts: Vec<&str> = stem.split('/').collect();
    if matches!(parts.as_slice(), ["lib"] | ["main"]) {
        return Some(krate.to_owned());
    }
    if parts.last() == Some(&"mod") {
        parts.pop();
    }
    Some(
        std::iter::once(krate)
            .chain(parts)
            .collect::<Vec<_>>()
            .join("::"),
    )
}

fn text(node: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

/// Top-level item names of a file (functions, types, traits, consts,
/// statics, macros, inline modules).
pub fn catalog(source: &str) -> HashSet<String> {
    let Some(tree) = crate::parse(source) else {
        return HashSet::new();
    };
    let src = source.as_bytes();
    children(tree.root_node())
        .into_iter()
        .filter(|n| {
            matches!(
                n.kind(),
                "function_item"
                    | "struct_item"
                    | "enum_item"
                    | "trait_item"
                    | "type_item"
                    | "const_item"
                    | "static_item"
                    | "macro_definition"
                    | "mod_item"
                    | "union_item"
            )
        })
        .filter_map(|n| n.child_by_field_name("name").map(|x| text(x, src)))
        .collect()
}

/// Resolution context of one file.
struct Ctx<'a> {
    module: &'a str,
    krate: &'a str,
    scope: &'a Scope,
    /// Local alias -> absolute path segments.
    uses: HashMap<String, Vec<String>>,
    /// Absolute module paths imported with `*`.
    globs: Vec<String>,
}

impl Ctx<'_> {
    /// Make a path absolute (`crate`/`self`/`super`/use bindings).
    fn absolute(&self, segs: &[String]) -> Option<Vec<String>> {
        let (first, rest) = segs.split_first()?;
        let base: Vec<String> = match first.as_str() {
            "crate" => vec![self.krate.to_owned()],
            "self" | "Self" => self.module.split("::").map(str::to_owned).collect(),
            "super" => {
                let mut m: Vec<String> = self.module.split("::").map(str::to_owned).collect();
                m.pop();
                let mut rest = rest;
                while rest.first().is_some_and(|s| s == "super") {
                    m.pop();
                    rest = &rest[1..];
                }
                return Some(m.into_iter().chain(rest.iter().cloned()).collect());
            }
            name => match self.uses.get(name) {
                Some(p) => p.clone(),
                // A child module declared with `mod name;` here.
                None if self
                    .scope
                    .modules
                    .contains(&format!("{}::{name}", self.module)) =>
                {
                    self.module
                        .split("::")
                        .map(str::to_owned)
                        .chain(std::iter::once(name.to_owned()))
                        .collect()
                }
                None if self.scope.crates.contains(name) => vec![name.to_owned()],
                None => return None,
            },
        };
        Some(base.into_iter().chain(rest.iter().cloned()).collect())
    }

    /// First-party module a path lands in: the longest known module
    /// prefix, followed through `pub use` re-exports to the module that
    /// defines the item.
    fn module_of(&self, abs: &[String]) -> Option<String> {
        let mut path = abs.to_vec();
        for _ in 0..8 {
            let n = (1..=path.len())
                .rev()
                .find(|n| self.scope.modules.contains(&path[..*n].join("::")))?;
            let module = path[..n].join("::");
            let Some(name) = path.get(n) else {
                return Some(module);
            };
            if self
                .scope
                .items
                .get(&module)
                .is_some_and(|items| items.contains(name))
            {
                return Some(module);
            }
            let Some((named, globs)) = self.scope.reexports.get(&module) else {
                return Some(module);
            };
            if let Some(target) = named.get(name) {
                path = target
                    .iter()
                    .cloned()
                    .chain(path[n + 1..].iter().cloned())
                    .collect();
                continue;
            }
            let via_glob = globs.iter().find(|g| {
                self.scope
                    .items
                    .get(*g)
                    .is_some_and(|items| items.contains(name))
            });
            return Some(via_glob.cloned().unwrap_or(module));
        }
        None
    }

    /// Dependencies a path contributes.
    fn resolve(&self, segs: &[String]) -> Option<String> {
        if let Some(abs) = self.absolute(segs) {
            return self.module_of(&abs).filter(|m| m != self.module);
        }
        // An unbound single name may come from a first-party glob import.
        let [name] = segs else { return None };
        self.globs
            .iter()
            .find(|g| {
                self.scope
                    .items
                    .get(*g)
                    .is_some_and(|items| items.contains(name))
            })
            .filter(|g| *g != self.module)
            .cloned()
    }
}

/// Flatten a `use` tree into `(alias, absolute-ish segments)` bindings
/// and glob prefixes (segments still relative to `crate`/`self`/...).
fn use_tree(
    node: Node,
    prefix: &[String],
    src: &[u8],
    out: &mut Vec<(String, Vec<String>)>,
    globs: &mut Vec<Vec<String>>,
) {
    let segs = |n: Node| -> Vec<String> {
        text(n, src)
            .split("::")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    };
    match node.kind() {
        "scoped_use_list" => {
            let mut p = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                p.extend(segs(path));
            }
            if let Some(list) = node.child_by_field_name("list") {
                for c in children(list).into_iter().filter(Node::is_named) {
                    use_tree(c, &p, src, out, globs);
                }
            }
        }
        "use_list" => {
            for c in children(node).into_iter().filter(Node::is_named) {
                use_tree(c, prefix, src, out, globs);
            }
        }
        "use_as_clause" => {
            let path: Vec<String> = node
                .child_by_field_name("path")
                .map(segs)
                .unwrap_or_default();
            let alias = node
                .child_by_field_name("alias")
                .map(|a| text(a, src))
                .unwrap_or_default();
            out.push((alias, prefix.iter().cloned().chain(path).collect()));
        }
        "use_wildcard" => {
            let path = children(node)
                .into_iter()
                .find(Node::is_named)
                .map(segs)
                .unwrap_or_default();
            globs.push(prefix.iter().cloned().chain(path).collect());
        }
        _ => {
            let full: Vec<String> = prefix.iter().cloned().chain(segs(node)).collect();
            match full.last().map(String::as_str) {
                Some("self") => {
                    let mut p = full.clone();
                    p.pop();
                    if let Some(last) = p.last().cloned() {
                        out.push((last, p));
                    }
                }
                Some(last) => out.push((last.to_owned(), full.clone())),
                None => {}
            }
        }
    }
}

/// Path segments of a path-like node (generic arguments dropped).
fn path_segments(node: Node, src: &[u8]) -> Vec<String> {
    let t = text(node, src);
    let mut out = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ':' if depth == 0 && chars.get(i + 1) == Some(&':') => {
                out.push(std::mem::take(&mut current).trim().to_owned());
                i += 2;
                continue;
            }
            c if depth == 0 && !c.is_whitespace() => current.push(c),
            _ => {}
        }
        i += 1;
    }
    out.push(current.trim().to_owned());
    out.retain(|s| !s.is_empty());
    out
}

/// Paths spelled inside a macro token tree: `a :: b :: c` runs.
fn token_paths(tt: Node, src: &[u8], out: &mut Vec<(u32, Vec<String>)>) {
    let mut run: Vec<String> = Vec::new();
    let mut run_row = 0;
    let mut expect_segment = true;
    let flush = |run: &mut Vec<String>, r: u32, out: &mut Vec<(u32, Vec<String>)>| {
        if !run.is_empty() {
            out.push((r, std::mem::take(run)));
        }
    };
    for c in children(tt) {
        match c.kind() {
            "identifier" | "crate" | "self" | "super" if expect_segment => {
                if run.is_empty() {
                    run_row = row(c.start_position().row);
                }
                run.push(text(c, src));
                expect_segment = false;
            }
            "::" if !run.is_empty() => expect_segment = true,
            "token_tree" => {
                flush(&mut run, run_row, out);
                expect_segment = true;
                token_paths(c, src, out);
            }
            _ => {
                flush(&mut run, run_row, out);
                expect_segment = true;
                if matches!(c.kind(), "identifier" | "crate" | "self" | "super") {
                    run_row = row(c.start_position().row);
                    run.push(text(c, src));
                    expect_segment = false;
                }
            }
        }
    }
    flush(&mut run, run_row, out);
}

/// Every path reference in the file, by row.
fn references(root: Node, src: &[u8]) -> Vec<(u32, Vec<String>)> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "use_declaration" | "line_comment" | "block_comment" | "attribute_item" => continue,
            // `mod x;` declares ownership, not a dependency.
            "mod_item" if n.child_by_field_name("body").is_none() => continue,
            "scoped_identifier" | "scoped_type_identifier" | "generic_type" => {
                out.push((row(n.start_position().row), path_segments(n, src)));
                continue;
            }
            "identifier" | "type_identifier" => {
                // An item's own name at its definition is not a reference.
                let defines = n
                    .parent()
                    .and_then(|p| p.child_by_field_name("name"))
                    .is_some_and(|d| d.id() == n.id());
                if !defines {
                    out.push((row(n.start_position().row), vec![text(n, src)]));
                }
                continue;
            }
            "token_tree" => {
                token_paths(n, src, &mut out);
                continue;
            }
            "macro_invocation" => {
                if let Some(m) = n.child_by_field_name("macro") {
                    out.push((row(m.start_position().row), path_segments(m, src)));
                }
            }
            _ => {}
        }
        stack.extend(children(n));
    }
    out
}

/// Coupling of one region.
#[derive(Clone, Debug, Default)]
pub struct RegionDeps {
    pub deps: BTreeSet<String>,
    pub local_callees: Vec<String>,
}

pub struct FileCoupling {
    /// First-party modules this file depends on.
    pub graph: BTreeSet<String>,
    /// Per region, in `FileMeasure::regions` order.
    pub regions: Vec<RegionDeps>,
}

fn use_bindings(root: Node, src: &[u8], public_only: bool) -> Bindings {
    let (mut uses, mut globs) = (Vec::new(), Vec::new());
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let public = children(n)
            .iter()
            .any(|c| c.kind() == "visibility_modifier");
        if n.kind() == "use_declaration" && (public || !public_only) {
            if let Some(arg) = n.child_by_field_name("argument") {
                use_tree(arg, &[], src, &mut uses, &mut globs);
            }
            continue;
        }
        stack.extend(children(n));
    }
    (uses, globs)
}

/// A module's `pub use` re-exports, resolved to absolute paths.
pub fn reexports(module: &str, krate: &str, source: &str, scope: &Scope) -> Reexports {
    let Some(tree) = crate::parse(source) else {
        return Default::default();
    };
    let (uses, globs) = use_bindings(tree.root_node(), source.as_bytes(), true);
    let ctx = Ctx {
        module,
        krate,
        scope,
        uses: HashMap::new(),
        globs: Vec::new(),
    };
    let named = uses
        .into_iter()
        .filter_map(|(alias, segs)| ctx.absolute(&segs).map(|abs| (alias, abs)))
        .collect();
    let globs = globs
        .iter()
        .filter_map(|g| ctx.absolute(g))
        .map(|abs| abs.join("::"))
        .collect();
    (named, globs)
}

/// Coupling of a file measured as module `module` of crate `krate`.
pub fn couple(module: &str, krate: &str, source: &str, scope: &Scope) -> Option<FileCoupling> {
    let tree = crate::parse(source)?;
    let src = source.as_bytes();
    let (raw_uses, raw_globs) = use_bindings(tree.root_node(), src, false);
    let mut ctx = Ctx {
        module,
        krate,
        scope,
        uses: HashMap::new(),
        globs: Vec::new(),
    };
    // Resolve use bindings against crate/self/super/crate names (not other
    // use bindings, which is how Rust 2018 paths work).
    let bindings: Vec<(String, Vec<String>)> = raw_uses
        .into_iter()
        .filter_map(|(alias, segs)| ctx.absolute(&segs).map(|abs| (alias, abs)))
        .collect();
    let globs: Vec<String> = raw_globs
        .iter()
        .filter_map(|g| ctx.absolute(g))
        .map(|abs| abs.join("::"))
        .collect();
    ctx.uses = bindings.into_iter().collect();
    ctx.globs = globs;
    let tests = crate::eligible_lines(&tree, src);
    let (cands, nodes) = crate::candidates(&tree, src, &test_rows_of(&tree, src));
    let regions = regions::assign(cands, &tests, &crate::names::module_qname(module));
    let fn_names: HashSet<String> = regions.iter().map(|r| r.name.clone()).collect();
    let mut per_region: Vec<RegionDeps> = vec![RegionDeps::default(); regions.len()];
    let owner = owner_index(&regions);
    for (r, segs) in references(tree.root_node(), src) {
        let Some(&i) = owner.get(&r) else { continue };
        if let Some(dep) = ctx.resolve(&segs) {
            per_region[i].deps.insert(dep);
        } else if segs.len() == 1 && fn_names.contains(&segs[0]) {
            per_region[i].local_callees.push(segs[0].clone());
        }
    }
    let _ = nodes;
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

fn test_rows_of(tree: &tree_sitter::Tree, src: &[u8]) -> BTreeSet<u32> {
    crate::test_rows(tree, src)
}

/// Row -> index of the region owning it.
fn owner_index(regions: &[Region]) -> HashMap<u32, usize> {
    regions
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.lines.iter().map(move |l| (*l, i)))
        .collect()
}
