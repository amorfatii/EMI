//! Coupling evidence of `python_probe` (profile 0.12): static import
//! bindings, a catalog of provably callable first-party declarations, class
//! models, and per-region resolved dependencies and unresolved calls.
//!
//! Every traversal uses the CPython-shaped walk in `pyast` (verified against
//! `ast.walk` order and positions), because binding resolution is sequential
//! and unresolved calls are reported in walk order.

use std::collections::{BTreeSet, HashMap, HashSet};

use ruff_python_ast::{self as ast, Expr, ExprContext, Stmt, StmtFunctionDef};

use crate::pyast::{bfs, PyNode, Positions};

/// What a name is statically known to refer to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Module(String),
    Symbol(String, String),
    Class(String, String),
    Instance(String, String),
    /// A first-party value (function or lambda) of a module, or of no module.
    Value(Option<String>),
    External,
    Unknown,
}

use Binding::{Class, External, Instance, Symbol, Unknown, Value};

fn merge(current: Option<&Binding>, incoming: Binding) -> Binding {
    match current {
        None => incoming,
        Some(c) if *c == incoming => incoming,
        Some(_) => Unknown,
    }
}

/// Module name of a scope-relative path (`pkg/__init__.py` is `pkg`).
pub fn module_name(rel: &str) -> String {
    let stem = rel.rsplit_once('.').map_or(rel, |(s, _)| s);
    let mut parts: Vec<&str> = stem.split('/').collect();
    if parts.last() == Some(&"__init__") {
        parts.pop();
    }
    parts.join(".")
}

// ---- per-file facts that need no other file --------------------------

#[derive(Clone, Debug)]
enum RawTarget {
    Module(String),
    From { base: String, name: String },
}

#[derive(Clone, Debug, Default)]
pub struct ClassFacts {
    pub methods: HashSet<String>,
    pub safe: bool,
}

/// Declarations one module contributes to the scope-wide catalog.
#[derive(Clone, Debug, Default)]
pub struct CatalogPart {
    pub symbols: Vec<String>,
    pub classes: Vec<(String, ClassFacts)>,
}

fn function_names(body: &[Stmt]) -> impl Iterator<Item = &StmtFunctionDef> {
    body.iter().filter_map(|s| match s {
        Stmt::FunctionDef(f) => Some(f),
        _ => None,
    })
}

fn self_attribute(target: &Expr) -> Option<&str> {
    match target {
        Expr::Attribute(a) => match &*a.value {
            Expr::Name(n) if matches!(n.id.as_str(), "self" | "cls") => Some(a.attr.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn assignment_targets(stmt: &Stmt) -> &[Expr] {
    match stmt {
        Stmt::Assign(a) => &a.targets,
        Stmt::AnnAssign(a) => std::slice::from_ref(&a.target),
        _ => &[],
    }
}

fn class_facts(stmt: &Stmt, c: &ast::StmtClassDef) -> ClassFacts {
    let methods: HashSet<String> = function_names(&c.body).map(|f| f.name.to_string()).collect();
    let has_args = c.arguments.as_deref().is_some_and(|a| !a.args.is_empty() || !a.keywords.is_empty());
    let mut unsafe_ = has_args
        || !c.decorator_list.is_empty()
        || ["__getattr__", "__getattribute__", "__new__"].iter().any(|m| methods.contains(*m))
        || function_names(&c.body).any(|f| !f.decorator_list.is_empty());
    for node in bfs(PyNode::Stmt(stmt)) {
        if let PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) = node
            && assignment_targets(s).iter().any(|t| self_attribute(t).is_some_and(|a| methods.contains(a)))
        {
            unsafe_ = true;
        }
    }
    ClassFacts { methods, safe: !unsafe_ }
}

pub fn catalog_part(module: &ast::ModModule) -> CatalogPart {
    let mut part = CatalogPart::default();
    for stmt in &module.body {
        match stmt {
            Stmt::FunctionDef(f) => part.symbols.push(f.name.to_string()),
            Stmt::ClassDef(c) => {
                part.symbols.push(c.name.to_string());
                part.classes.push((c.name.to_string(), class_facts(stmt, c)));
            }
            _ => {}
        }
    }
    part
}

/// Scope-wide knowledge: first-party modules and the declaration catalog.
#[derive(Default)]
pub struct Project {
    pub known_modules: HashSet<String>,
    /// Modules with at least one top-level def/class, and those names.
    pub symbols: HashMap<String, HashSet<String>>,
    pub classes: HashMap<(String, String), ClassFacts>,
}

impl Project {
    pub fn add(&mut self, module: &str, part: CatalogPart) {
        self.known_modules.insert(module.to_owned());
        if !part.symbols.is_empty() {
            self.symbols.entry(module.to_owned()).or_default().extend(part.symbols);
        }
        for (name, facts) in part.classes {
            self.classes.insert((module.to_owned(), name), facts);
        }
    }

    fn module_component(&self, binding: &Binding) -> Option<String> {
        let m = match binding {
            Binding::Module(m) | Symbol(m, _) | Class(m, _) | Instance(m, _) | Value(Some(m)) => m,
            _ => return None,
        };
        self.known_modules.contains(m).then(|| m.clone())
    }

    fn member_binding(&self, binding: Binding, attrs: &[&str]) -> Binding {
        match binding {
            External => External,
            Unknown => Unknown,
            b if attrs.is_empty() => b,
            Binding::Module(base) => {
                for index in (0..=attrs.len()).rev() {
                    let candidate = if index > 0 { format!("{base}.{}", attrs[..index].join(".")) } else { base.clone() };
                    if let Some(names) = self.symbols.get(&candidate) {
                        let remaining = &attrs[index..];
                        return match remaining {
                            [only] if names.contains(*only) => Symbol(candidate, (*only).to_owned()),
                            _ => Unknown,
                        };
                    }
                }
                if base.contains('.') { Unknown } else { External }
            }
            Symbol(m, _) if !self.symbols.contains_key(&m) => External,
            Instance(m, n) | Class(m, n) => match self.classes.get(&(m.clone(), n)) {
                Some(model) if model.safe && model.methods.contains(attrs[0]) && attrs.len() == 1 => Value(Some(m)),
                _ => Unknown,
            },
            _ => Unknown,
        }
    }
}

// ---- import bindings -------------------------------------------------

/// Import bindings of a module, resolved against the known modules.
pub struct Imports {
    pub aliases: HashMap<String, Binding>,
    pub dependencies: BTreeSet<String>,
    pub ambiguous: HashSet<String>,
}

/// `package[:len(package) - level + 1]` with Python's negative-index slicing.
fn relative_prefix(package: &[&str], level: u32) -> Vec<String> {
    let stop = i64::try_from(package.len()).unwrap_or(i64::MAX) - i64::from(level) + 1;
    let len = i64::try_from(package.len()).unwrap_or(i64::MAX);
    let stop = if stop < 0 { (len + stop).max(0) } else { stop.min(len) };
    package[..usize::try_from(stop).unwrap_or(0)].iter().map(|s| (*s).to_owned()).collect()
}

fn collect_imports<'a>(module_ast: &'a ast::ModModule, module: &str, is_package: bool) -> (Vec<(String, RawTarget)>, Vec<String>, bool) {
    let parts: Vec<&str> = module.split('.').collect();
    let package: &[&str] = if is_package { &parts } else { &parts[..parts.len().saturating_sub(1)] };
    let (mut found, mut reassigns, mut star) = (Vec::new(), Vec::new(), false);
    let mut queue = std::collections::VecDeque::from([(PyNode::Module(module_ast), false)]);
    while let Some((node, nested)) = queue.pop_front() {
        match node {
            PyNode::Stmt(Stmt::Import(i)) => {
                for item in &i.names {
                    let root = item.asname.as_ref().map_or_else(
                        || item.name.split('.').next().unwrap_or_default().to_owned(),
                        ToString::to_string,
                    );
                    let target = if item.asname.is_some() { item.name.to_string() } else { root.clone() };
                    found.push((root, RawTarget::Module(target)));
                }
            }
            PyNode::Stmt(Stmt::ImportFrom(i)) => {
                let base = if i.level > 0 {
                    let mut prefix = relative_prefix(package, i.level);
                    prefix.extend(i.module.as_ref().map(ToString::to_string));
                    prefix.join(".")
                } else {
                    i.module.as_ref().map(ToString::to_string).unwrap_or_default()
                };
                for item in &i.names {
                    if item.name.as_str() == "*" {
                        star = true;
                        continue;
                    }
                    let alias = item.asname.as_ref().unwrap_or(&item.name).to_string();
                    found.push((alias, RawTarget::From { base: base.clone(), name: item.name.to_string() }));
                }
            }
            PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) if !nested => {
                reassigns.extend(assignment_targets(s).iter().filter_map(name_id));
            }
            PyNode::Expr(Expr::Named(n)) if !nested => reassigns.extend(name_id(&n.target)),
            _ => {}
        }
        let child_nested = nested || matches!(node, PyNode::Stmt(Stmt::FunctionDef(_) | Stmt::ClassDef(_)));
        queue.extend(node.children().into_iter().map(|c| (c, child_nested)));
    }
    (found, reassigns, star)
}

fn name_id(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.to_string()),
        _ => None,
    }
}

pub fn resolve_imports(module_ast: &ast::ModModule, module: &str, is_package: bool, project: &Project) -> Imports {
    let (found, reassigns, star) = collect_imports(module_ast, module, is_package);
    let mut imports = Imports { aliases: HashMap::new(), dependencies: BTreeSet::new(), ambiguous: HashSet::new() };
    if star {
        imports.ambiguous.insert("*".to_owned());
    }
    for (alias, raw) in found {
        let target = match raw {
            RawTarget::Module(m) => Binding::Module(m),
            RawTarget::From { base, name } => {
                let possible = format!("{base}.{name}");
                if project.known_modules.contains(&possible) { Binding::Module(possible) } else { Symbol(base, name) }
            }
        };
        if imports.aliases.get(&alias).is_some_and(|b| *b != target) {
            imports.ambiguous.insert(alias.clone());
            imports.aliases.insert(alias, Unknown);
            continue;
        }
        if let Some(c) = project.module_component(&target).filter(|c| c != module) {
            imports.dependencies.insert(c);
        }
        imports.aliases.insert(alias, target);
    }
    for name in reassigns {
        if imports.aliases.contains_key(&name) {
            imports.aliases.insert(name.clone(), Unknown);
            imports.ambiguous.insert(name);
        }
    }
    imports
}

// ---- expression bindings and class models ----------------------------

pub struct ClassModel {
    pub attributes: HashMap<String, Binding>,
    pub safe: bool,
}

/// Root name and literal attribute path of `a.b.c`.
fn attribute_parts(e: &Expr) -> (Option<&str>, Vec<&str>) {
    let mut parts = Vec::new();
    let mut node = e;
    while let Expr::Attribute(a) = node {
        parts.push(a.attr.as_str());
        node = &a.value;
    }
    parts.reverse();
    match node {
        Expr::Name(n) => (Some(n.id.as_str()), parts),
        _ => (None, Vec::new()),
    }
}

struct Resolver<'p> {
    project: &'p Project,
}

impl Resolver<'_> {
    fn name(&self, id: &str, bindings: &HashMap<String, Binding>) -> Binding {
        let b = bindings.get(id).cloned().unwrap_or(Unknown);
        match b {
            Symbol(m, n) if self.project.classes.contains_key(&(m.clone(), n.clone())) => Class(m, n),
            other => other,
        }
    }

    fn expr(&self, e: &Expr, bindings: &HashMap<String, Binding>, class: Option<&ClassModel>) -> Binding {
        match e {
            Expr::Name(n) => self.name(n.id.as_str(), bindings),
            Expr::Attribute(_) => {
                let (root, attrs) = attribute_parts(e);
                if let (Some("self" | "cls"), Some(model)) = (root, class) {
                    if !model.safe {
                        return Unknown;
                    }
                    let first = attrs.first().and_then(|a| model.attributes.get(*a)).cloned().unwrap_or(Unknown);
                    return self.project.member_binding(first, attrs.get(1..).unwrap_or_default());
                }
                let binding = root.map_or(Unknown, |r| self.name(r, bindings));
                if binding == External {
                    return External;
                }
                self.project.member_binding(binding, &attrs)
            }
            Expr::Call(c) => match self.expr(&c.func, bindings, class) {
                Class(m, n) => Instance(m, n),
                _ => Unknown,
            },
            Expr::Lambda(_) => Value(None),
            _ => Unknown,
        }
    }
}

/// Class models keyed by method node address.
pub fn class_models(
    module_ast: &ast::ModModule,
    aliases: &HashMap<String, Binding>,
    module: &str,
    project: &Project,
) -> (Vec<ClassModel>, HashMap<*const StmtFunctionDef, usize>) {
    let resolver = Resolver { project };
    let (mut models, mut contexts) = (Vec::new(), HashMap::new());
    let mut top = aliases.clone();
    for stmt in &module_ast.body {
        match stmt {
            Stmt::FunctionDef(f) => {
                top.insert(f.name.to_string(), Value(Some(module.to_owned())));
            }
            Stmt::ClassDef(c) => {
                top.insert(c.name.to_string(), Class(module.to_owned(), c.name.to_string()));
            }
            _ => {}
        }
    }
    for node in bfs(PyNode::Module(module_ast)) {
        let PyNode::Stmt(Stmt::ClassDef(cls)) = node else { continue };
        let safe = project.classes.get(&(module.to_owned(), cls.name.to_string())).is_some_and(|f| f.safe);
        let mut model = ClassModel { attributes: HashMap::new(), safe };
        let methods: Vec<&StmtFunctionDef> = function_names(&cls.body).collect();
        for m in &methods {
            contexts.insert(std::ptr::from_ref(*m), models.len());
        }
        let mut base = top.clone();
        for member in bfs(node) {
            let PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) = member else { continue };
            let value = match s {
                Stmt::Assign(a) => &*a.value,
                Stmt::AnnAssign(a) => match a.value.as_deref() {
                    Some(v) => v,
                    None => continue,
                },
                _ => continue,
            };
            let binding = resolver.expr(value, &base, Some(&model));
            for target in assignment_targets(s) {
                if let Some(attr) = self_attribute(target) {
                    let merged = merge(model.attributes.get(attr), binding.clone());
                    model.attributes.insert(attr.to_owned(), merged);
                } else if let Some(id) = name_id(target) {
                    let merged = merge(base.get(&id), binding.clone());
                    base.insert(id, merged);
                }
            }
        }
        if model.safe {
            for m in methods {
                model.attributes.entry(m.name.to_string()).or_insert_with(|| Value(Some(module.to_owned())));
            }
        }
        models.push(model);
    }
    (models, contexts)
}

// ---- per-region dependencies -----------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Unresolved {
    pub line: Option<u32>,
    pub column: Option<usize>,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct RegionCoupling {
    pub deps: BTreeSet<String>,
    pub unresolved: Vec<Unresolved>,
    pub local_callees: Vec<String>,
}

pub struct ModuleContext<'a> {
    pub module: &'a str,
    pub imports: &'a Imports,
    pub project: &'a Project,
    pub positions: &'a Positions<'a>,
    pub models: &'a [ClassModel],
}

/// Names bound by store-context Name nodes anywhere under `e`.
fn stored_names(e: &Expr) -> Vec<String> {
    bfs(PyNode::Expr(e))
        .into_iter()
        .filter_map(|n| match n {
            PyNode::Expr(Expr::Name(x)) if x.ctx == ExprContext::Store => Some(x.id.to_string()),
            _ => None,
        })
        .collect()
}

fn initial_bindings(ctx: &ModuleContext, root: PyNode) -> HashMap<String, Binding> {
    let module = ctx.module.to_owned();
    let mut bindings = ctx.imports.aliases.clone();
    for name in ctx.project.symbols.get(ctx.module).into_iter().flatten() {
        let b = if ctx.project.classes.contains_key(&(module.clone(), name.clone())) {
            Class(module.clone(), name.clone())
        } else {
            Value(Some(module.clone()))
        };
        bindings.insert(name.clone(), b);
    }
    let body: &[Stmt] = match root {
        PyNode::Module(m) => &m.body,
        PyNode::Stmt(Stmt::FunctionDef(f)) => &f.body,
        _ => &[],
    };
    for s in body {
        match s {
            Stmt::FunctionDef(f) => {
                bindings.insert(f.name.to_string(), Value(Some(module.clone())));
            }
            Stmt::ClassDef(c) => {
                bindings.insert(c.name.to_string(), Class(module.clone(), c.name.to_string()));
            }
            _ => {}
        }
    }
    if let PyNode::Stmt(Stmt::FunctionDef(f)) = root {
        for n in bfs(PyNode::Arguments(&f.parameters)) {
            if let PyNode::Arg(a) = n {
                bindings.insert(a.name.to_string(), Unknown);
            }
        }
    }
    bindings
}

const DYNAMIC: [&str; 4] = ["eval", "exec", "getattr", "__import__"];

/// Resolved dependencies, unresolved calls and local callees of a region.
pub fn region_coupling(ctx: &ModuleContext, root: PyNode, owned: &BTreeSet<u32>, class: Option<&ClassModel>) -> RegionCoupling {
    let resolver = Resolver { project: ctx.project };
    let mut bindings = initial_bindings(ctx, root);
    let (mut assigns, mut uses, mut shadowed) = (Vec::new(), Vec::new(), BTreeSet::new());
    for node in bfs(root) {
        let Some((line, _)) = ctx.positions.lineno_col(node, None) else { continue };
        if !owned.contains(&line) {
            continue;
        }
        match node {
            PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) => assigns.push(Assign::Stmt(s)),
            PyNode::Expr(Expr::Named(n)) => assigns.push(Assign::Named(n)),
            PyNode::Stmt(Stmt::For(f)) => shadowed.extend(stored_names(&f.target)),
            PyNode::Stmt(Stmt::With(w)) if !w.is_async => {
                for item in &w.items {
                    shadowed.extend(item.optional_vars.as_deref().map(stored_names).unwrap_or_default());
                }
            }
            PyNode::Handler(h) => shadowed.extend(h.name.as_ref().map(ToString::to_string)),
            PyNode::Expr(Expr::Call(_)) => uses.push(node),
            PyNode::Expr(Expr::Name(n)) if n.ctx == ExprContext::Load => uses.push(node),
            _ => {}
        }
    }
    for name in shadowed {
        let merged = merge(bindings.get(&name), Unknown);
        bindings.insert(name, merged);
    }
    for a in assigns {
        let (value, targets) = a.parts();
        let binding = value.map_or(Unknown, |v| resolver.expr(v, &bindings, class));
        for id in targets.iter().filter_map(|t| name_id(t)) {
            let merged = merge(bindings.get(&id), binding.clone());
            bindings.insert(id, merged);
        }
    }
    resolve_uses(ctx, &resolver, &bindings, root, &uses, class)
}

enum Assign<'a> {
    Stmt(&'a Stmt),
    Named(&'a ast::ExprNamed),
}

impl<'a> Assign<'a> {
    fn parts(&self) -> (Option<&'a Expr>, &'a [Expr]) {
        match self {
            Assign::Stmt(s @ Stmt::Assign(a)) => (Some(&*a.value), assignment_targets(s)),
            Assign::Stmt(s @ Stmt::AnnAssign(a)) => (a.value.as_deref(), assignment_targets(s)),
            Assign::Named(n) => (Some(&*n.value), std::slice::from_ref(&*n.target)),
            Assign::Stmt(_) => (None, &[]),
        }
    }
}

fn resolve_uses(
    ctx: &ModuleContext,
    resolver: &Resolver,
    bindings: &HashMap<String, Binding>,
    root: PyNode,
    uses: &[PyNode],
    class: Option<&ClassModel>,
) -> RegionCoupling {
    let mut out = RegionCoupling::default();
    let mut seen = HashSet::new();
    let mut callees = BTreeSet::new();
    let mut unknown = |node: PyNode, reason: &'static str, out: &mut RegionCoupling| {
        let pos = ctx.positions.lineno_col(node, None);
        let (line, column) = (pos.map(|p| p.0), pos.map(|p| p.1));
        if seen.insert((line, column, reason)) {
            out.unresolved.push(Unresolved { line, column, reason });
        }
    };
    if ctx.imports.ambiguous.contains("*") {
        unknown(root, "star import can bind an unknown callable", &mut out);
    }
    for &node in uses {
        let func = match node {
            PyNode::Expr(Expr::Name(n)) => {
                let b = bindings.get(n.id.as_str()).cloned().unwrap_or(Unknown);
                out.deps.extend(ctx.project.module_component(&b).filter(|c| c != ctx.module));
                continue;
            }
            PyNode::Expr(Expr::Call(c)) => &*c.func,
            _ => continue,
        };
        let func_name = name_id(func);
        if func_name.as_deref().is_some_and(|f| DYNAMIC.contains(&f)) {
            unknown(node, "dynamic callable lookup", &mut out);
            continue;
        }
        let binding = resolver.expr(func, bindings, class);
        if let Some(c) = ctx.project.module_component(&binding).filter(|c| c != ctx.module) {
            out.deps.insert(c);
        } else if binding == Unknown {
            let (root_name, _) = attribute_parts(func);
            let reason = match root_name {
                Some(r) if ctx.imports.ambiguous.contains(r) => Some("conflicting import binding"),
                Some("self" | "cls") => Some("unresolved instance or class attribute"),
                Some(r) if bindings.contains_key(r) => Some("call through unresolved local binding"),
                Some(r) if crate::builtins::is_builtin(r) => None,
                _ => Some("unresolved callable"),
            };
            if let Some(reason) = reason {
                unknown(node, reason, &mut out);
            }
        }
        if let Some(name) = func_name {
            match &binding {
                Value(Some(m)) if m == ctx.module => {
                    callees.insert(name);
                }
                Class(m, n) if m == ctx.module => {
                    callees.insert(n.clone());
                }
                _ => {}
            }
        }
    }
    out.local_callees = callees.into_iter().collect();
    out
}

/// Tarjan strongly connected components, iterative, in the oracle's order:
/// roots and children visited sorted, groups sorted, singletons omitted.
pub fn strongly_connected(graph: &std::collections::BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut lowlink: HashMap<&str, usize> = HashMap::new();
    let mut on_stack: HashSet<&str> = HashSet::new();
    let (mut stack, mut groups, mut counter) = (Vec::new(), Vec::new(), 0usize);
    let empty = BTreeSet::new();
    for root in graph.keys() {
        if index_of.contains_key(root.as_str()) {
            continue;
        }
        index_of.insert(root, counter);
        lowlink.insert(root, counter);
        counter += 1;
        stack.push(root.as_str());
        on_stack.insert(root);
        let mut work: Vec<(&str, std::collections::btree_set::Iter<String>)> =
            vec![(root.as_str(), graph.get(root).unwrap_or(&empty).iter())];
        while let Some((node, children)) = work.last_mut() {
            let node = *node;
            let mut descended = None;
            for child in children.by_ref() {
                if !index_of.contains_key(child.as_str()) {
                    index_of.insert(child, counter);
                    lowlink.insert(child, counter);
                    counter += 1;
                    stack.push(child);
                    on_stack.insert(child);
                    descended = Some(child.as_str());
                    break;
                } else if on_stack.contains(child.as_str()) && index_of[child.as_str()] < lowlink[node] {
                    lowlink.insert(node, index_of[child.as_str()]);
                }
            }
            if let Some(child) = descended {
                work.push((child, graph.get(child).unwrap_or(&empty).iter()));
                continue;
            }
            work.pop();
            if let Some((parent, _)) = work.last()
                && lowlink[node] < lowlink[*parent]
            {
                lowlink.insert(parent, lowlink[node]);
            }
            if lowlink[node] == index_of[node] {
                let mut group = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack.remove(member);
                    group.push(member.to_owned());
                    if member == node {
                        break;
                    }
                }
                if group.len() > 1 {
                    group.sort();
                    groups.push(group);
                }
            }
        }
    }
    groups.sort();
    groups
}
