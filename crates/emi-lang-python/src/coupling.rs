//! Coupling evidence of `python_probe` (profile 0.12): static import
//! bindings, a catalog of provably callable first-party declarations, class
//! models, and per-region resolved dependencies and unresolved calls.
//!
//! Every traversal uses the CPython-shaped walk in `pyast` (verified against
//! `ast.walk` order and positions), because binding resolution is sequential
//! and unresolved calls are reported in walk order.

use std::collections::{BTreeSet, HashMap, HashSet};

use ruff_python_ast::{self as ast, Expr, ExprContext, Stmt, StmtFunctionDef};

use crate::pyast::{Positions, PyNode, bfs};

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
    /// Precise profile: a value handed in or produced at runtime
    /// (parameters, loop items, call results); calls through it are
    /// dynamic dispatch, not a static dependency.
    Dispatch,
    /// Precise profile: an attribute inherited from these first-party
    /// ancestor modules.
    Inherited(Vec<String>),
}

use Binding::{Class, Dispatch, External, Instance, Symbol, Unknown, Value};

fn merge(current: Option<&Binding>, incoming: Binding) -> Binding {
    match current {
        None => incoming,
        Some(c) if *c == incoming => incoming,
        Some(_) => Unknown,
    }
}

/// First-party module a reference binding reaches, if any.
fn reference_module(b: &Binding) -> Option<&String> {
    match b {
        Binding::Module(m) | Symbol(m, _) | Class(m, _) | Instance(m, _) | Value(Some(m)) => {
            Some(m)
        }
        _ => None,
    }
}

/// Precise profile: least upper bound of two origins. Conflicting
/// first-party references stay unknown; a reference joined with a runtime
/// value keeps the reference (the code is reachable); values join to
/// dispatch.
fn join(current: Option<&Binding>, incoming: Binding) -> Binding {
    let Some(current) = current else {
        return incoming;
    };
    if *current == incoming {
        return incoming;
    }
    match (current, &incoming) {
        (Unknown, _) | (_, Unknown) => Unknown,
        _ => match (reference_module(current), reference_module(&incoming)) {
            (Some(a), Some(b)) if a == b => Value(Some(a.clone())),
            (Some(_), Some(_)) => Unknown,
            (Some(_), None) => current.clone(),
            (None, Some(_)) => incoming,
            (None, None) => Dispatch,
        },
    }
}

/// `merge` under the parity profile, `join` under the precise one.
fn combine(precise: bool, current: Option<&Binding>, incoming: Binding) -> Binding {
    if precise {
        join(current, incoming)
    } else {
        merge(current, incoming)
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
    let methods: HashSet<String> = function_names(&c.body)
        .map(|f| f.name.to_string())
        .collect();
    let has_args = c
        .arguments
        .as_deref()
        .is_some_and(|a| !a.args.is_empty() || !a.keywords.is_empty());
    let mut unsafe_ = has_args
        || !c.decorator_list.is_empty()
        || ["__getattr__", "__getattribute__", "__new__"]
            .iter()
            .any(|m| methods.contains(*m))
        || function_names(&c.body).any(|f| !f.decorator_list.is_empty());
    for node in bfs(PyNode::Stmt(stmt)) {
        if let PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) = node
            && assignment_targets(s)
                .iter()
                .any(|t| self_attribute(t).is_some_and(|a| methods.contains(a)))
        {
            unsafe_ = true;
        }
    }
    ClassFacts {
        methods,
        safe: !unsafe_,
    }
}

pub fn catalog_part(module: &ast::ModModule) -> CatalogPart {
    let mut part = CatalogPart::default();
    for stmt in &module.body {
        match stmt {
            Stmt::FunctionDef(f) => part.symbols.push(f.name.to_string()),
            Stmt::ClassDef(c) => {
                part.symbols.push(c.name.to_string());
                part.classes
                    .push((c.name.to_string(), class_facts(stmt, c)));
            }
            _ => {}
        }
    }
    part
}

/// Scope-wide knowledge: first-party modules and the declaration catalog.
#[derive(Default)]
pub struct Project {
    /// Measure with the precise profile's binder.
    pub precise: bool,
    pub known_modules: HashSet<String>,
    /// Modules with at least one top-level def/class, and those names.
    pub symbols: HashMap<String, HashSet<String>>,
    pub classes: HashMap<(String, String), ClassFacts>,
}

impl Project {
    pub fn add(&mut self, module: &str, part: CatalogPart) {
        self.known_modules.insert(module.to_owned());
        if !part.symbols.is_empty() {
            self.symbols
                .entry(module.to_owned())
                .or_default()
                .extend(part.symbols);
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
        if self.precise && !attrs.is_empty() {
            match &binding {
                Dispatch | Value(None) => return Dispatch,
                Binding::Inherited(_) => return binding,
                Value(Some(m)) => return Value(Some(m.clone())),
                Symbol(m, _) if self.symbols.contains_key(m) => return Value(Some(m.clone())),
                Instance(m, n) | Class(m, n) => {
                    let known = self.classes.get(&(m.clone(), n.clone()));
                    return match known {
                        Some(f) if f.methods.contains(attrs[0]) && attrs.len() == 1 => {
                            Value(Some(m.clone()))
                        }
                        // An attribute of a first-party object: its code
                        // lives with the class's module.
                        _ => Value(Some(m.clone())),
                    };
                }
                _ => {}
            }
        }
        match binding {
            External => External,
            Unknown => Unknown,
            b if attrs.is_empty() => b,
            Binding::Module(base) => {
                for index in (0..=attrs.len()).rev() {
                    let candidate = if index > 0 {
                        format!("{base}.{}", attrs[..index].join("."))
                    } else {
                        base.clone()
                    };
                    if let Some(names) = self.symbols.get(&candidate) {
                        let remaining = &attrs[index..];
                        return match remaining {
                            [only] if names.contains(*only) => {
                                Symbol(candidate, (*only).to_owned())
                            }
                            // Precise: a member of a first-party symbol is
                            // that module's code.
                            [first, ..] if self.precise && names.contains(*first) => {
                                Value(Some(candidate))
                            }
                            _ => Unknown,
                        };
                    }
                }
                if base.contains('.') {
                    Unknown
                } else {
                    External
                }
            }
            Symbol(m, _) if !self.symbols.contains_key(&m) => External,
            Instance(m, n) | Class(m, n) => match self.classes.get(&(m.clone(), n)) {
                Some(model)
                    if model.safe && model.methods.contains(attrs[0]) && attrs.len() == 1 =>
                {
                    Value(Some(m))
                }
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
    let stop = if stop < 0 {
        (len + stop).max(0)
    } else {
        stop.min(len)
    };
    package[..usize::try_from(stop).unwrap_or(0)]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

fn collect_imports(
    module_ast: &ast::ModModule,
    module: &str,
    is_package: bool,
) -> (Vec<(String, RawTarget)>, Vec<String>, bool) {
    let parts: Vec<&str> = module.split('.').collect();
    let package: &[&str] = if is_package {
        &parts
    } else {
        &parts[..parts.len().saturating_sub(1)]
    };
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
                    let target = if item.asname.is_some() {
                        item.name.to_string()
                    } else {
                        root.clone()
                    };
                    found.push((root, RawTarget::Module(target)));
                }
            }
            PyNode::Stmt(Stmt::ImportFrom(i)) => {
                let base = if i.level > 0 {
                    let mut prefix = relative_prefix(package, i.level);
                    prefix.extend(i.module.as_ref().map(ToString::to_string));
                    prefix.join(".")
                } else {
                    i.module
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default()
                };
                for item in &i.names {
                    if item.name.as_str() == "*" {
                        star = true;
                        continue;
                    }
                    let alias = item.asname.as_ref().unwrap_or(&item.name).to_string();
                    found.push((
                        alias,
                        RawTarget::From {
                            base: base.clone(),
                            name: item.name.to_string(),
                        },
                    ));
                }
            }
            PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) if !nested => {
                reassigns.extend(assignment_targets(s).iter().filter_map(name_id));
            }
            PyNode::Expr(Expr::Named(n)) if !nested => reassigns.extend(name_id(&n.target)),
            _ => {}
        }
        let child_nested =
            nested || matches!(node, PyNode::Stmt(Stmt::FunctionDef(_) | Stmt::ClassDef(_)));
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

pub fn resolve_imports(
    module_ast: &ast::ModModule,
    module: &str,
    is_package: bool,
    project: &Project,
) -> Imports {
    let (found, reassigns, star) = collect_imports(module_ast, module, is_package);
    let mut imports = Imports {
        aliases: HashMap::new(),
        dependencies: BTreeSet::new(),
        ambiguous: HashSet::new(),
    };
    if star {
        imports.ambiguous.insert("*".to_owned());
    }
    for (alias, raw) in found {
        let target = match raw {
            RawTarget::Module(m) => Binding::Module(m),
            RawTarget::From { base, name } => {
                let possible = format!("{base}.{name}");
                if project.known_modules.contains(&possible) {
                    Binding::Module(possible)
                } else {
                    Symbol(base, name)
                }
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
    /// Precise profile: first-party ancestor modules when every base
    /// resolved, `None` when some base (or the metaclass) did not.
    pub heritage: Option<Vec<String>>,
    /// Defines `__getattr__`/`__getattribute__`: missing attributes are
    /// computed at runtime.
    pub dynamic: bool,
}

/// The expression under an attribute chain (`f()` in `f().a.b`).
fn attribute_base(e: &Expr) -> &Expr {
    let mut node = e;
    while let Expr::Attribute(a) = node {
        node = &a.value;
    }
    node
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
            Symbol(m, n) if self.project.classes.contains_key(&(m.clone(), n.clone())) => {
                Class(m, n)
            }
            other => other,
        }
    }

    fn expr(
        &self,
        e: &Expr,
        bindings: &HashMap<String, Binding>,
        class: Option<&ClassModel>,
    ) -> Binding {
        match e {
            Expr::Name(n) => self.name(n.id.as_str(), bindings),
            Expr::Attribute(_) => {
                let (root, attrs) = attribute_parts(e);
                if let (Some("self" | "cls"), Some(model)) = (root, class) {
                    if self.project.precise {
                        return self.self_attribute(model, &attrs);
                    }
                    if !model.safe {
                        return Unknown;
                    }
                    let first = attrs
                        .first()
                        .and_then(|a| model.attributes.get(*a))
                        .cloned()
                        .unwrap_or(Unknown);
                    return self
                        .project
                        .member_binding(first, attrs.get(1..).unwrap_or_default());
                }
                if root.is_none() && self.project.precise {
                    let base = match self.expr(attribute_base(e), bindings, class) {
                        External => return External,
                        Unknown => return Unknown,
                        b => b,
                    };
                    return self.project.member_binding(base, &attrs);
                }
                let binding = root.map_or(Unknown, |r| self.name(r, bindings));
                if binding == External {
                    return External;
                }
                self.project.member_binding(binding, &attrs)
            }
            Expr::Call(c) => match self.expr(&c.func, bindings, class) {
                Class(m, n) => Instance(m, n),
                _ if self.project.precise => Dispatch,
                _ => Unknown,
            },
            Expr::Lambda(_) => Value(None),
            Expr::If(i) if self.project.precise => join(
                Some(&self.expr(&i.body, bindings, class)),
                self.expr(&i.orelse, bindings, class),
            ),
            Expr::BoolOp(b) if self.project.precise => b
                .values
                .iter()
                .map(|v| self.expr(v, bindings, class))
                .reduce(|a, v| join(Some(&a), v))
                .unwrap_or(Dispatch),
            Expr::Named(n) if self.project.precise => self.expr(&n.value, bindings, class),
            Expr::Await(a) if self.project.precise => match self.expr(&a.value, bindings, class) {
                Unknown => Unknown,
                _ => Dispatch,
            },
            _ if self.project.precise => Dispatch,
            _ => Unknown,
        }
    }

    /// Precise profile: `self.a.b` in a method of `model`'s class.
    fn self_attribute(&self, model: &ClassModel, attrs: &[&str]) -> Binding {
        let rest = attrs.get(1..).unwrap_or_default();
        match attrs.first().and_then(|a| model.attributes.get(*a)) {
            Some(b) => self.project.member_binding(b.clone(), rest),
            None if attrs.is_empty() => Dispatch,
            None if model.dynamic => Unknown,
            None => match &model.heritage {
                None => Unknown,
                Some(mods) if mods.is_empty() => Dispatch,
                Some(mods) => Binding::Inherited(mods.clone()),
            },
        }
    }
}

/// Precise profile: `(heritage, dynamic)` of a class. Every base (and a
/// metaclass keyword) must resolve: first-party bases contribute their
/// modules, builtins and packages contribute nothing.
fn precise_heritage(
    resolver: &Resolver,
    cls: &ast::StmtClassDef,
    top: &HashMap<String, Binding>,
    module: &str,
) -> (Option<Vec<String>>, bool) {
    let dynamic = function_names(&cls.body)
        .any(|f| matches!(f.name.as_str(), "__getattr__" | "__getattribute__"));
    if !resolver.project.precise {
        return (None, dynamic);
    }
    let mut modules = BTreeSet::new();
    let args = cls.arguments.as_deref();
    let bases = args
        .map(|a| a.args.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let keywords = args
        .map(|a| a.keywords.iter().map(|k| &k.value).collect::<Vec<_>>())
        .unwrap_or_default();
    for base in bases.into_iter().chain(keywords) {
        if let Expr::Name(n) = base
            && !top.contains_key(n.id.as_str())
            && crate::builtins::is_builtin(n.id.as_str())
        {
            continue;
        }
        match resolver.expr(base, top, None) {
            External => {}
            b => match reference_module(&b) {
                Some(m) if resolver.project.known_modules.contains(m) => {
                    if m != module {
                        modules.insert(m.clone());
                    }
                }
                Some(_) => {}
                None => return (None, dynamic),
            },
        }
    }
    (Some(modules.into_iter().collect()), dynamic)
}

/// Statements of a scope without entering nested functions or classes.
fn scope_statements<'a>(body: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for s in body {
        out.push(s);
        match s {
            Stmt::If(i) => {
                scope_statements(&i.body, out);
                for clause in &i.elif_else_clauses {
                    scope_statements(&clause.body, out);
                }
            }
            Stmt::For(f) => {
                scope_statements(&f.body, out);
                scope_statements(&f.orelse, out);
            }
            Stmt::While(w) => {
                scope_statements(&w.body, out);
                scope_statements(&w.orelse, out);
            }
            Stmt::With(w) => scope_statements(&w.body, out),
            Stmt::Try(t) => {
                scope_statements(&t.body, out);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    scope_statements(&h.body, out);
                }
                scope_statements(&t.orelse, out);
                scope_statements(&t.finalbody, out);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    scope_statements(&case.body, out);
                }
            }
            _ => {}
        }
    }
}

/// Precise profile: one scope's own bindings over `outer` — nested defs
/// and classes, assignments, and loop/with/except targets (handed in).
fn scope_env(
    resolver: &Resolver,
    body: &[Stmt],
    outer: &HashMap<String, Binding>,
    module: &str,
    keep: &HashSet<String>,
) -> HashMap<String, Binding> {
    let mut env = outer.clone();
    let mut stmts = Vec::new();
    scope_statements(body, &mut stmts);
    for s in &stmts {
        match s {
            Stmt::FunctionDef(f) => {
                env.insert(f.name.to_string(), Value(Some(module.to_owned())));
            }
            Stmt::ClassDef(c) => {
                env.insert(
                    c.name.to_string(),
                    Class(module.to_owned(), c.name.to_string()),
                );
            }
            _ => {}
        }
    }
    let mut declared: HashSet<String> = HashSet::new();
    let mut bind = |env: &mut HashMap<String, Binding>, name: String, value: Binding| {
        if keep.contains(&name) {
            return;
        }
        let merged = if declared.contains(&name) {
            join(env.get(&name), value)
        } else {
            value
        };
        declared.insert(name.clone());
        env.insert(name, merged);
    };
    for s in &stmts {
        match s {
            Stmt::Assign(_) | Stmt::AnnAssign(_) => {
                let value = match s {
                    Stmt::Assign(a) => Some(&*a.value),
                    Stmt::AnnAssign(a) => a.value.as_deref(),
                    _ => None,
                };
                let binding = value.map_or(Dispatch, |v| resolver.expr(v, &env, None));
                for target in assignment_targets(s) {
                    for id in stored_names(target) {
                        let b = if name_id(target).is_some() {
                            binding.clone()
                        } else {
                            Dispatch
                        };
                        bind(&mut env, id, b);
                    }
                }
            }
            Stmt::For(f) => {
                for id in stored_names(&f.target) {
                    bind(&mut env, id, Dispatch);
                }
            }
            Stmt::With(w) => {
                for item in &w.items {
                    for id in item
                        .optional_vars
                        .as_deref()
                        .map(stored_names)
                        .unwrap_or_default()
                    {
                        bind(&mut env, id, Dispatch);
                    }
                }
            }
            _ => {}
        }
    }
    env
}

/// Precise profile: the module scope — imports, top-level defs/classes
/// and module-level variables (imports keep their binding).
pub fn module_env(
    module_ast: &ast::ModModule,
    aliases: &HashMap<String, Binding>,
    module: &str,
    project: &Project,
) -> HashMap<String, Binding> {
    let resolver = Resolver { project };
    let keep: HashSet<String> = aliases.keys().cloned().collect();
    scope_env(&resolver, &module_ast.body, aliases, module, &keep)
}

/// Precise profile: for every function, the bindings its region starts
/// from — the module scope plus each enclosing function's scope (its
/// parameters are handed in), outermost first.
pub fn function_envs<'a>(
    module_ast: &'a ast::ModModule,
    module_env: &HashMap<String, Binding>,
    module: &str,
    project: &Project,
) -> HashMap<*const StmtFunctionDef, HashMap<String, Binding>> {
    let resolver = Resolver { project };
    let mut envs: HashMap<*const StmtFunctionDef, HashMap<String, Binding>> = HashMap::new();
    let none = HashSet::new();
    let mut stack: Vec<(&'a [Stmt], HashMap<String, Binding>)> =
        vec![(&module_ast.body, module_env.clone())];
    while let Some((body, env)) = stack.pop() {
        for node in bfs_statements(body) {
            match node {
                Stmt::FunctionDef(f) => {
                    envs.insert(std::ptr::from_ref(f), env.clone());
                    let mut inner = env.clone();
                    for n in bfs(PyNode::Arguments(&f.parameters)) {
                        if let PyNode::Arg(a) = n {
                            inner.insert(a.name.to_string(), Dispatch);
                        }
                    }
                    let inner = scope_env(&resolver, &f.body, &inner, module, &none);
                    stack.push((&f.body, inner));
                }
                Stmt::ClassDef(c) => stack.push((&c.body, env.clone())),
                _ => {}
            }
        }
    }
    envs
}

/// Function and class definitions of a scope (not nested in each other).
fn bfs_statements(body: &[Stmt]) -> Vec<&Stmt> {
    let mut stmts = Vec::new();
    scope_statements(body, &mut stmts);
    stmts.retain(|s| matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)));
    stmts
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
    let top = top_bindings(module_ast, aliases, module, project);
    for node in bfs(PyNode::Module(module_ast)) {
        let PyNode::Stmt(Stmt::ClassDef(cls)) = node else {
            continue;
        };
        for m in function_names(&cls.body) {
            contexts.insert(std::ptr::from_ref(m), models.len());
        }
        models.push(class_model(&resolver, node, cls, &top, module));
    }
    (models, contexts)
}

/// Module-scope bindings class bodies resolve against.
fn top_bindings(
    module_ast: &ast::ModModule,
    aliases: &HashMap<String, Binding>,
    module: &str,
    project: &Project,
) -> HashMap<String, Binding> {
    let mut top = if project.precise {
        module_env(module_ast, aliases, module, project)
    } else {
        aliases.clone()
    };
    for stmt in &module_ast.body {
        match stmt {
            Stmt::FunctionDef(f) => {
                top.insert(f.name.to_string(), Value(Some(module.to_owned())));
            }
            Stmt::ClassDef(c) => {
                top.insert(
                    c.name.to_string(),
                    Class(module.to_owned(), c.name.to_string()),
                );
            }
            _ => {}
        }
    }
    top
}

/// One class's attribute bindings, from every assignment in its body.
fn class_model(
    resolver: &Resolver,
    node: PyNode,
    cls: &ast::StmtClassDef,
    top: &HashMap<String, Binding>,
    module: &str,
) -> ClassModel {
    let project = resolver.project;
    let safe = project
        .classes
        .get(&(module.to_owned(), cls.name.to_string()))
        .is_some_and(|f| f.safe);
    let (heritage, dynamic) = precise_heritage(resolver, cls, top, module);
    let mut model = ClassModel {
        attributes: HashMap::new(),
        safe,
        heritage,
        dynamic,
    };
    let methods: Vec<&StmtFunctionDef> = function_names(&cls.body).collect();
    let mut base = top.clone();
    if project.precise {
        // Values assigned to fields from method parameters are handed in.
        for m in &methods {
            for n in bfs(PyNode::Arguments(&m.parameters)) {
                if let PyNode::Arg(a) = n {
                    base.insert(a.name.to_string(), Dispatch);
                }
            }
        }
    }
    for member in bfs(node) {
        let PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) = member else {
            continue;
        };
        let value = match s {
            Stmt::Assign(a) => Some(&*a.value),
            Stmt::AnnAssign(a) => a.value.as_deref(),
            _ => None,
        };
        let Some(value) = value else { continue };
        let binding = resolver.expr(value, &base, Some(&model));
        for target in assignment_targets(s) {
            if let Some(attr) = self_attribute(target) {
                let merged = combine(project.precise, model.attributes.get(attr), binding.clone());
                model.attributes.insert(attr.to_owned(), merged);
            } else if let Some(id) = name_id(target) {
                let merged = combine(project.precise, base.get(&id), binding.clone());
                base.insert(id, merged);
            }
        }
    }
    if model.safe || project.precise {
        for m in methods {
            model
                .attributes
                .entry(m.name.to_string())
                .or_insert_with(|| Value(Some(module.to_owned())));
        }
    }
    model
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
    /// Precise profile: bindings visible in a region before its own
    /// (module scope plus enclosing functions), by function node.
    pub envs: Option<&'a HashMap<*const StmtFunctionDef, HashMap<String, Binding>>>,
    pub module_env: Option<&'a HashMap<String, Binding>>,
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
    let precise_base = match root {
        PyNode::Stmt(Stmt::FunctionDef(f)) => ctx.envs.and_then(|e| e.get(&std::ptr::from_ref(f))),
        _ => ctx.module_env,
    };
    let mut bindings = precise_base
        .cloned()
        .unwrap_or_else(|| ctx.imports.aliases.clone());
    for name in ctx.project.symbols.get(ctx.module).into_iter().flatten() {
        let b = if ctx
            .project
            .classes
            .contains_key(&(module.clone(), name.clone()))
        {
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
                bindings.insert(
                    c.name.to_string(),
                    Class(module.clone(), c.name.to_string()),
                );
            }
            _ => {}
        }
    }
    if let PyNode::Stmt(Stmt::FunctionDef(f)) = root {
        let param = if ctx.project.precise {
            Dispatch
        } else {
            Unknown
        };
        for n in bfs(PyNode::Arguments(&f.parameters)) {
            if let PyNode::Arg(a) = n {
                bindings.insert(a.name.to_string(), param.clone());
            }
        }
    }
    bindings
}

const DYNAMIC: [&str; 4] = ["eval", "exec", "getattr", "__import__"];

/// Resolved dependencies, unresolved calls and local callees of a region.
pub fn region_coupling(
    ctx: &ModuleContext,
    root: PyNode,
    owned: &BTreeSet<u32>,
    class: Option<&ClassModel>,
) -> RegionCoupling {
    let resolver = Resolver {
        project: ctx.project,
    };
    let mut bindings = initial_bindings(ctx, root);
    let (mut assigns, mut uses, mut shadowed) = (Vec::new(), Vec::new(), BTreeSet::new());
    for node in bfs(root) {
        let Some((line, _)) = ctx.positions.lineno_col(node, None) else {
            continue;
        };
        if !owned.contains(&line) {
            continue;
        }
        match node {
            PyNode::Stmt(s @ (Stmt::Assign(_) | Stmt::AnnAssign(_))) => {
                assigns.push(Assign::Stmt(s))
            }
            PyNode::Expr(Expr::Named(n)) => assigns.push(Assign::Named(n)),
            PyNode::Stmt(Stmt::For(f)) => shadowed.extend(stored_names(&f.target)),
            PyNode::Stmt(Stmt::With(w)) if !w.is_async => {
                for item in &w.items {
                    shadowed.extend(
                        item.optional_vars
                            .as_deref()
                            .map(stored_names)
                            .unwrap_or_default(),
                    );
                }
            }
            PyNode::Handler(h) => shadowed.extend(h.name.as_ref().map(ToString::to_string)),
            PyNode::Expr(
                e @ (Expr::ListComp(_) | Expr::SetComp(_) | Expr::DictComp(_) | Expr::Generator(_)),
            ) if ctx.project.precise => {
                shadowed.extend(comprehension_targets(e));
            }
            PyNode::Expr(Expr::Call(_)) => uses.push(node),
            PyNode::Expr(Expr::Name(n)) if n.ctx == ExprContext::Load => uses.push(node),
            _ => {}
        }
    }
    let precise = ctx.project.precise;
    let item = if precise { Dispatch } else { Unknown };
    for name in shadowed {
        let merged = combine(precise, bindings.get(&name), item.clone());
        bindings.insert(name, merged);
    }
    for a in assigns {
        let (value, targets) = a.parts();
        let binding = value.map_or(item.clone(), |v| resolver.expr(v, &bindings, class));
        for id in targets.iter().filter_map(name_id) {
            let merged = combine(precise, bindings.get(&id), binding.clone());
            bindings.insert(id, merged);
        }
    }
    resolve_uses(ctx, &resolver, &bindings, root, &uses, class)
}

/// Names bound by a comprehension's `for` targets.
fn comprehension_targets(e: &Expr) -> Vec<String> {
    let generators: &[ast::Comprehension] = match e {
        Expr::ListComp(c) => &c.generators,
        Expr::SetComp(c) => &c.generators,
        Expr::DictComp(c) => &c.generators,
        Expr::Generator(c) => &c.generators,
        _ => return Vec::new(),
    };
    generators
        .iter()
        .flat_map(|g| stored_names(&g.target))
        .collect()
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

/// Why a call resolved to nothing, or `None` for builtins.
fn unknown_reason(
    func: &Expr,
    ctx: &ModuleContext,
    bindings: &HashMap<String, Binding>,
) -> Option<&'static str> {
    let (root_name, _) = attribute_parts(func);
    match root_name {
        Some(r) if ctx.imports.ambiguous.contains(r) => Some("conflicting import binding"),
        Some("self" | "cls") => Some("unresolved instance or class attribute"),
        Some(r) if bindings.contains_key(r) => Some("call through unresolved local binding"),
        Some(r) if crate::builtins::is_builtin(r) => None,
        _ => Some("unresolved callable"),
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
            out.unresolved.push(Unresolved {
                line,
                column,
                reason,
            });
        }
    };
    if ctx.imports.ambiguous.contains("*") {
        unknown(root, "star import can bind an unknown callable", &mut out);
    }
    for &node in uses {
        let func = match node {
            PyNode::Expr(Expr::Name(n)) => {
                let b = bindings.get(n.id.as_str()).cloned().unwrap_or(Unknown);
                out.deps
                    .extend(ctx.project.module_component(&b).filter(|c| c != ctx.module));
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
        if let Binding::Inherited(mods) = &binding {
            out.deps
                .extend(mods.iter().filter(|m| *m != ctx.module).cloned());
            continue;
        }
        if let Some(c) = ctx
            .project
            .module_component(&binding)
            .filter(|c| c != ctx.module)
        {
            out.deps.insert(c);
        } else if binding == Unknown
            && let Some(reason) = unknown_reason(func, ctx, bindings)
        {
            unknown(node, reason, &mut out);
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

pub use emi_ir::graph::strongly_connected;
