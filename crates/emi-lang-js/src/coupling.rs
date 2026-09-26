//! Coupling evidence of `js_probe` (profile 0.10): ESM/CommonJS import
//! bindings, re-export and literal require/import graph edges, and per-region
//! resolved dependencies, unresolved calls and local callees.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use tree_sitter::{Node, Tree};

use crate::names::{FUNCTIONS, string_value};
use crate::parse::push_children;
use crate::resolve::{Scope, Target};
use crate::rules::{children, named_children, text, walk};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Module(String),
    Symbol(String, String),
    External,
    Local,
    Unknown,
}

use Binding::{External, Local, Unknown};

pub const RUNTIME_GLOBALS: &[&str] = &[
    "Array",
    "BigInt",
    "Boolean",
    "Buffer",
    "Date",
    "Error",
    "Infinity",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "Promise",
    "Reflect",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "WeakMap",
    "WeakSet",
    "__dirname",
    "__filename",
    "alert",
    "clearInterval",
    "clearTimeout",
    "confirm",
    "console",
    "decodeURI",
    "decodeURIComponent",
    "document",
    "encodeURI",
    "encodeURIComponent",
    "eval",
    "exports",
    "fetch",
    "global",
    "globalThis",
    "history",
    "isFinite",
    "isNaN",
    "localStorage",
    "location",
    "module",
    "navigator",
    "parseFloat",
    "parseInt",
    "process",
    "prompt",
    "queueMicrotask",
    "requestAnimationFrame",
    "screen",
    "sessionStorage",
    "setInterval",
    "setTimeout",
    "undefined",
    "window",
];

fn is_global(name: &str) -> bool {
    RUNTIME_GLOBALS.contains(&name)
}

const DYNAMIC_CALLEES: [&str; 2] = ["eval", "Function"];

fn is_function_like(kind: &str) -> bool {
    FUNCTIONS.contains(&kind) || kind == "class_declaration"
}

fn declare(
    bindings: &mut HashMap<String, Binding>,
    ambiguous: &mut HashSet<String>,
    name: &str,
    value: Binding,
) {
    if bindings.get(name).is_some_and(|b| *b != value) {
        ambiguous.insert(name.to_owned());
        bindings.insert(name.to_owned(), Unknown);
    } else {
        bindings.insert(name.to_owned(), value);
    }
}

pub struct FileCtx<'a> {
    pub scope: &'a Scope,
    pub rel: &'a str,
    pub module: &'a str,
    pub src: &'a [u8],
}

impl FileCtx<'_> {
    fn resolve(&self, spec: &str) -> Option<Target> {
        self.scope.resolve(spec, self.rel)
    }

    fn module_of(&self, b: &Binding) -> Option<String> {
        match b {
            Binding::Module(m) | Binding::Symbol(m, _) if self.scope.known_modules.contains(m) => {
                Some(m.clone())
            }
            _ => None,
        }
    }
}

/// Single static string argument of a call.
fn literal_arg(call: Node, src: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    match named_children(args).as_slice() {
        [only] if only.kind() == "string" => string_value(*only, src),
        _ => None,
    }
}

fn is_require_call(value: Option<Node>, src: &[u8]) -> bool {
    value.is_some_and(|v| {
        v.kind() == "call_expression"
            && v.child_by_field_name("function")
                .is_some_and(|f| f.kind() == "identifier" && text(f, src) == "require")
    })
}

fn shadowed_require(value: Option<Node>, bindings: &HashMap<String, Binding>, src: &[u8]) -> bool {
    is_require_call(value, src) && bindings.contains_key("require")
}

/// `_require_target`: `None` when not an unshadowed require() at all.
fn require_target(
    ctx: &FileCtx,
    value: Option<Node>,
    bindings: &HashMap<String, Binding>,
) -> Option<Binding> {
    if !is_require_call(value, ctx.src) || bindings.contains_key("require") {
        return None;
    }
    let spec = value.and_then(|v| literal_arg(v, ctx.src));
    Some(match spec.and_then(|s| ctx.resolve(&s)) {
        None => Unknown,
        Some(Target::External) => External,
        Some(Target::Module(m)) => Binding::Module(m),
    })
}

/// `(name, key)` pairs of `const <pattern> = require(...)`; `None` if unsupported.
fn require_names(name: Node, src: &[u8]) -> Option<Vec<(String, Option<String>)>> {
    if name.kind() == "identifier" {
        return Some(vec![(text(name, src).into_owned(), None)]);
    }
    if name.kind() != "object_pattern" {
        return None;
    }
    let mut out = Vec::new();
    for prop in walk(name) {
        match prop.kind() {
            "shorthand_property_identifier_pattern" => {
                let t = text(prop, src).into_owned();
                out.push((t.clone(), Some(t)));
            }
            "pair_pattern" => {
                let key = prop
                    .child_by_field_name("key")
                    .filter(|k| k.kind() == "property_identifier")?;
                let val = prop
                    .child_by_field_name("value")
                    .filter(|v| v.kind() == "identifier")?;
                out.push((
                    text(val, src).into_owned(),
                    Some(text(key, src).into_owned()),
                ));
            }
            _ => {}
        }
    }
    (!out.is_empty()).then_some(out)
}

fn bind_require_shape(
    name: Node,
    target: &Binding,
    bindings: &mut HashMap<String, Binding>,
    ambiguous: &mut HashSet<String>,
    src: &[u8],
) {
    for (local, key) in require_names(name, src).unwrap_or_default() {
        let value = match (target, key) {
            (Unknown, _) => Unknown,
            (External, _) => External,
            (t, None) => t.clone(),
            (Binding::Module(m), Some(k)) => Binding::Symbol(m.clone(), k),
            (_, Some(_)) => Unknown,
        };
        declare(bindings, ambiguous, &local, value);
    }
}

// ---- file level --------------------------------------------------------

pub struct FileImports {
    pub aliases: HashMap<String, Binding>,
    pub ambiguous: HashSet<String>,
    pub reexports: BTreeSet<String>,
}

fn clause_names(clause: Node, src: &[u8]) -> Vec<(String, bool)> {
    let mut names = Vec::new();
    for child in children(clause) {
        match child.kind() {
            "identifier" => names.push((text(child, src).into_owned(), false)),
            "named_imports" => {
                for spec in children(child)
                    .into_iter()
                    .filter(|s| s.kind() == "import_specifier")
                {
                    let local = children(spec)
                        .into_iter()
                        .rfind(|p| p.kind() == "identifier");
                    names.extend(local.map(|l| (text(l, src).into_owned(), false)));
                }
            }
            "namespace_import" => {
                names.extend(
                    children(child)
                        .into_iter()
                        .filter(|p| p.kind() == "identifier")
                        .map(|p| (text(p, src).into_owned(), true)),
                );
            }
            _ => {}
        }
    }
    names
}

enum Found {
    Edge(String),
    Name(String, Option<Target>, bool),
}

fn string_child(node: Node, src: &[u8]) -> Option<String> {
    children(node)
        .into_iter()
        .filter(|c| c.kind() == "string")
        .map(|c| string_value(c, src))
        .next_back()
        .flatten()
}

fn import_statement(ctx: &FileCtx, node: Node, found: &mut Vec<Found>) {
    let spec = children(node)
        .into_iter()
        .filter(|c| c.kind() == "string")
        .map(|c| string_value(c, ctx.src))
        .next_back()
        .flatten();
    let clause = children(node)
        .into_iter()
        .find(|c| c.kind() == "import_clause");
    let Some(spec) = spec else { return };
    let target = ctx.resolve(&spec);
    match clause {
        None => {
            if let Some(Target::Module(m)) = target
                && m != ctx.module
            {
                found.push(Found::Edge(m));
            }
        }
        Some(clause) => {
            for (name, ns) in clause_names(clause, ctx.src) {
                found.push(Found::Name(name, target.clone(), ns));
            }
        }
    }
}

struct ImportScan {
    imp: FileImports,
    found: Vec<Found>,
    reassigns: Vec<String>,
}

impl ImportScan {
    fn declarator(&mut self, ctx: &FileCtx, node: Node) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let value = node.child_by_field_name("value");
        if let Some(target) = require_target(ctx, value, &self.imp.aliases) {
            bind_require_shape(
                name,
                &target,
                &mut self.imp.aliases,
                &mut self.imp.ambiguous,
                ctx.src,
            );
        } else if name.kind() == "identifier" && value.is_some() && text(name, ctx.src) == "require"
        {
            self.imp.aliases.insert("require".into(), Unknown);
        }
    }

    fn assignment(&mut self, ctx: &FileCtx, node: Node) {
        let Some(left) = node
            .child_by_field_name("left")
            .filter(|l| l.kind() == "identifier")
        else {
            return;
        };
        let value = node.child_by_field_name("value");
        if let Some(target) = require_target(ctx, value, &self.imp.aliases) {
            bind_require_shape(
                left,
                &target,
                &mut self.imp.aliases,
                &mut self.imp.ambiguous,
                ctx.src,
            );
        } else {
            let name = text(left, ctx.src).into_owned();
            if name == "require" {
                self.imp.aliases.insert("require".into(), Unknown);
            }
            self.reassigns.push(name);
        }
    }

    fn visit(&mut self, ctx: &FileCtx, node: Node, nested: bool) {
        match node.kind() {
            "import_statement" => import_statement(ctx, node, &mut self.found),
            "export_statement" => {
                if let Some(Target::Module(m)) =
                    string_child(node, ctx.src).and_then(|s| ctx.resolve(&s))
                    && m != ctx.module
                {
                    self.imp.reexports.insert(m);
                }
            }
            "variable_declarator" if !nested => self.declarator(ctx, node),
            "assignment_expression" if !nested => self.assignment(ctx, node),
            _ => {}
        }
    }

    /// Bind collected import names; conflicts and reassignments poison.
    fn finish(mut self) -> FileImports {
        let imp = &mut self.imp;
        for f in self.found {
            let (alias, binding) = match f {
                Found::Edge(m) => {
                    imp.reexports.insert(m);
                    continue;
                }
                Found::Name(alias, target, ns) => {
                    let binding = match target {
                        None => Unknown,
                        Some(Target::External) => External,
                        Some(Target::Module(m)) if ns => Binding::Module(m),
                        Some(Target::Module(m)) => Binding::Symbol(m, alias.clone()),
                    };
                    (alias, binding)
                }
            };
            if imp.aliases.get(&alias).is_some_and(|b| *b != binding) {
                imp.ambiguous.insert(alias.clone());
                imp.aliases.insert(alias, Unknown);
            } else {
                imp.aliases.insert(alias, binding);
            }
        }
        for name in self.reassigns {
            if imp.aliases.contains_key(&name) {
                imp.aliases.insert(name.clone(), Unknown);
                imp.ambiguous.insert(name);
            }
        }
        self.imp
    }
}

/// `resolve_js_imports`: breadth-first over the tree with a nesting flag.
pub fn resolve_imports(ctx: &FileCtx, tree: &Tree) -> FileImports {
    let imp = FileImports {
        aliases: HashMap::new(),
        ambiguous: HashSet::new(),
        reexports: BTreeSet::new(),
    };
    let mut scan = ImportScan {
        imp,
        found: Vec::new(),
        reassigns: Vec::new(),
    };
    let mut queue = VecDeque::from([(tree.root_node(), false)]);
    while let Some((node, nested)) = queue.pop_front() {
        scan.visit(ctx, node, nested);
        let child_nested = nested || is_function_like(node.kind());
        queue.extend(children(node).into_iter().map(|c| (c, child_nested)));
    }
    scan.finish()
}

/// Top-level statements, looking through `export` wrappers.
fn top_level(root: Node) -> Vec<Node> {
    let mut out = Vec::new();
    for child in children(root) {
        if child.kind() == "export_statement" {
            out.extend(children(child));
        } else {
            out.push(child);
        }
    }
    out
}

const FUNCTION_VALUES: [&str; 7] = [
    "function_expression",
    "generator_function",
    "arrow_function",
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "class",
];

/// Module-scope local names: declarations plus `const f = <function-like>`.
pub fn file_symbols(root: Node, src: &[u8]) -> HashSet<String> {
    let mut symbols = HashSet::new();
    for node in top_level(root) {
        if is_function_like(node.kind())
            && let Some(n) = node
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")
        {
            symbols.insert(text(n, src).into_owned());
        }
        if !matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
            continue;
        }
        let kind = children(node)
            .into_iter()
            .find(|c| matches!(c.kind(), "const" | "let" | "var"));
        if kind.is_none_or(|k| k.kind() != "const") {
            continue;
        }
        for decl in children(node)
            .into_iter()
            .filter(|c| c.kind() == "variable_declarator")
        {
            let name = decl
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier");
            let value = decl
                .child_by_field_name("value")
                .filter(|v| FUNCTION_VALUES.contains(&v.kind()));
            if let (Some(n), Some(_)) = (name, value) {
                symbols.insert(text(n, src).into_owned());
            }
        }
    }
    symbols
}

/// Class name -> class node (first definition wins).
pub fn class_table<'t>(root: Node<'t>, src: &[u8]) -> HashMap<String, Node<'t>> {
    let mut table = HashMap::new();
    for node in top_level(root) {
        let (alias, cls) = match node.kind() {
            "class_declaration" => {
                let named = node
                    .child_by_field_name("name")
                    .filter(|n| n.kind() == "identifier");
                (named.map(|n| text(n, src).into_owned()), Some(node))
            }
            "lexical_declaration" | "variable_declaration" => children(node)
                .into_iter()
                .filter(|c| c.kind() == "variable_declarator")
                .find_map(|d| {
                    let name = d
                        .child_by_field_name("name")
                        .filter(|n| n.kind() == "identifier")?;
                    let value = d
                        .child_by_field_name("value")
                        .filter(|v| v.kind() == "class")?;
                    Some((Some(text(name, src).into_owned()), Some(value)))
                })
                .unwrap_or((None, None)),
            _ => (None, None),
        };
        if let Some(cls) = cls {
            table
                .entry(alias.unwrap_or_else(|| "<default>".into()))
                .or_insert(cls);
        }
    }
    table
}

fn heritage_base(class: Node) -> Option<Node> {
    let heritage = children(class)
        .into_iter()
        .find(|c| c.kind() == "class_heritage")?;
    children(heritage)
        .into_iter()
        .find(|p| p.is_named() && p.kind() != "extends")
}

/// `_heritage_resolution`: `(known, modules)` of a class's ancestry.
fn heritage(
    class: Node,
    bindings: &HashMap<String, Binding>,
    classes: &HashMap<String, Node>,
    src: &[u8],
) -> (bool, BTreeSet<String>) {
    let (mut modules, mut seen) = (BTreeSet::new(), HashSet::new());
    let mut current = class;
    loop {
        if !seen.insert(current.id()) {
            return (false, BTreeSet::new());
        }
        let Some(base) = heritage_base(current) else {
            return (true, modules);
        };
        if base.kind() != "identifier" {
            return (false, BTreeSet::new());
        }
        let name = text(base, src);
        if is_global(&name) {
            return (true, modules);
        }
        if let Some(next) = classes.get(name.as_ref()) {
            if next.id() == current.id() {
                return (true, modules);
            }
            current = *next;
            continue;
        }
        return match bindings.get(name.as_ref()) {
            Some(Binding::Module(m) | Binding::Symbol(m, _)) => {
                modules.insert(m.clone());
                (true, modules)
            }
            Some(Local) => (true, modules),
            _ => (false, BTreeSet::new()),
        };
    }
}

fn enclosing_class(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(c) = current {
        if matches!(c.kind(), "class_declaration" | "class") {
            return Some(c);
        }
        current = c.parent();
    }
    None
}

/// First-party modules named by literal require()/import() anywhere.
fn literal_targets(ctx: &FileCtx, tree: &Tree, skip_require: bool) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression"
            && let Some(func) = node.child_by_field_name("function")
        {
            let loader = if func.kind() == "identifier" && !skip_require {
                text(func, ctx.src) == "require"
            } else {
                func.kind() == "import"
            };
            if loader
                && let Some(Target::Module(m)) =
                    literal_arg(node, ctx.src).and_then(|s| ctx.resolve(&s))
            {
                targets.insert(m);
            }
        }
        push_children(&mut stack, node);
    }
    targets
}

/// File-level coupling facts shared by every region.
pub struct FileFacts<'t> {
    pub imports: FileImports,
    pub symbols: HashSet<String>,
    pub classes: HashMap<String, Node<'t>>,
    /// Import-graph edges of this module.
    pub graph: BTreeSet<String>,
}

pub fn file_facts<'t>(ctx: &FileCtx, tree: &'t Tree) -> FileFacts<'t> {
    let root = tree.root_node();
    let imports = resolve_imports(ctx, tree);
    let mut graph: BTreeSet<String> = imports
        .aliases
        .values()
        .filter_map(|b| ctx.module_of(b))
        .collect();
    graph.extend(imports.reexports.iter().cloned());
    graph.extend(literal_targets(
        ctx,
        tree,
        imports.aliases.contains_key("require"),
    ));
    graph.remove(ctx.module);
    FileFacts {
        symbols: file_symbols(root, ctx.src),
        classes: class_table(root, ctx.src),
        imports,
        graph,
    }
}

mod region;
pub use region::{RegionCoupling, Unresolved, region_coupling};
