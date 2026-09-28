//! The precise profile's binder: lexical environments with value origins.
//!
//! Parity (`js-probe-0.10`) marks a region's coupling unknown whenever a
//! call's receiver is not a first-party import, which makes every method
//! call on a parameter or local (`str.indexOf`, `args.slice`) unknown. The
//! precise profile measures *static* coupling instead: a region depends on
//! the first-party modules its code reaches through lexically visible
//! names. Each binding carries the origin of its value:
//!
//! - `Module`/`Symbol`: a first-party module or a member of it — code
//!   reached through a reference, a dependency wherever it is used (call
//!   results and instances are values: their dependency is counted at the
//!   call or `new` site, as in parity);
//! - `External`: a runtime global or a package outside the scope;
//! - `Local`: a value built here (literals, functions, classes);
//! - `Dispatch`: a value handed in (parameters, loop items, `this` fields),
//!   whose calls are dynamic dispatch — the static dependency belongs to
//!   the code that obtained the value, not to this region;
//! - `Unknown`: an origin that could be first-party but is not provable
//!   (conflicting imports, unresolvable or non-literal loaders).
//!
//! Environments nest: a callback sees its enclosing functions' names.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tree_sitter::Node;

use super::{Binding, FileCtx, is_function_like, is_runtime_global, literal_arg};
use crate::resolve::Target;
use crate::rules::{children, named_children, text};
use Binding::{Dispatch, External, Local, Unknown};

/// A lexical environment: this scope's bindings over the enclosing one.
/// Lookups fall through to the parent, so a scope behaves like a copy of
/// its parent plus its own names without storing that copy (a bundled file
/// has thousands of nested functions under one large module scope).
#[derive(Clone, Debug, Default)]
pub struct Env {
    local: HashMap<String, Binding>,
    parent: Option<Arc<Env>>,
}

impl Env {
    pub fn from_map(local: HashMap<String, Binding>) -> Env {
        Env {
            local,
            parent: None,
        }
    }

    fn child(parent: &Arc<Env>) -> Env {
        Env {
            local: HashMap::new(),
            parent: Some(Arc::clone(parent)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Binding> {
        let mut env = self;
        loop {
            if let Some(b) = env.local.get(name) {
                return Some(b);
            }
            env = env.parent.as_deref()?;
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn insert(&mut self, name: String, value: Binding) {
        self.local.insert(name, value);
    }

    /// Every visible binding, inner scopes shadowing outer ones.
    pub fn flatten(&self) -> HashMap<String, Binding> {
        let mut out = self.parent.as_deref().map(Env::flatten).unwrap_or_default();
        out.extend(self.local.iter().map(|(k, v)| (k.clone(), v.clone())));
        out
    }
}

/// Least upper bound of two origins.
pub fn join(a: &Binding, b: &Binding) -> Binding {
    let module = |x: &Binding| match x {
        Binding::Module(m) | Binding::Symbol(m, _) => Some(m.clone()),
        _ => None,
    };
    if a == b {
        return a.clone();
    }
    match (a, b, module(a), module(b)) {
        (Unknown, _, _, _) | (_, Unknown, _, _) => Unknown,
        (_, _, Some(m), Some(n)) => {
            if m == n {
                Binding::Module(m)
            } else {
                Unknown
            }
        }
        (_, _, Some(m), None) | (_, _, None, Some(m)) => Binding::Module(m),
        _ => Dispatch,
    }
}

/// A value derived from `b` (a member, call result, or instance).
fn derived(b: Binding) -> Binding {
    match b {
        Binding::Symbol(m, _) => Binding::Module(m),
        other => other,
    }
}

fn loader_origin(ctx: &FileCtx, call: Node) -> Binding {
    match literal_arg(call, ctx.src).map(|s| ctx.resolve(&s)) {
        Some(Some(Target::Module(m))) => Binding::Module(m),
        Some(Some(Target::External)) => External,
        _ => Unknown,
    }
}

const LOCAL_VALUES: [&str; 22] = [
    "string",
    "template_string",
    "number",
    "regex",
    "true",
    "false",
    "null",
    "undefined",
    "array",
    "object",
    "function_expression",
    "function",
    "arrow_function",
    "generator_function",
    "class",
    "unary_expression",
    "update_expression",
    "jsx_element",
    "jsx_self_closing_element",
    "jsx_fragment",
    "meta_property",
    "private_property_identifier",
];

const WRAPPERS: [&str; 6] = [
    "parenthesized_expression",
    "await_expression",
    "as_expression",
    "satisfies_expression",
    "non_null_expression",
    "type_assertion",
];

/// Origin of the value an expression evaluates to.
pub fn origin(ctx: &FileCtx, node: Node, env: &Env) -> Binding {
    let kind = node.kind();
    if LOCAL_VALUES.contains(&kind) {
        return Local;
    }
    if WRAPPERS.contains(&kind) {
        return named_children(node)
            .first()
            .map_or(Dispatch, |c| origin(ctx, *c, env));
    }
    let field = |name: &str| node.child_by_field_name(name);
    match kind {
        "identifier" => {
            let name = text(node, ctx.src);
            env.get(name.as_ref())
                .cloned()
                .unwrap_or(if is_runtime_global(&name, true) {
                    External
                } else {
                    Unknown
                })
        }
        "member_expression" | "subscript_expression" => {
            field("object").map_or(Dispatch, |o| derived(origin(ctx, o, env)))
        }
        "call_expression" => match field("function") {
            Some(f) if f.kind() == "import" => loader_origin(ctx, node),
            Some(f)
                if f.kind() == "identifier"
                    && text(f, ctx.src) == "require"
                    && !env.contains_key("require") =>
            {
                loader_origin(ctx, node)
            }
            // A call result is a value the call produced: the dependency
            // is recorded at the call site, not wherever the value flows.
            Some(_) => Local,
            None => Dispatch,
        },
        "new_expression" => Local,
        "assignment_expression" | "augmented_assignment_expression" => {
            field("right").map_or(Dispatch, |r| origin(ctx, r, env))
        }
        "sequence_expression" => named_children(node)
            .last()
            .map_or(Dispatch, |c| origin(ctx, *c, env)),
        "ternary_expression" => match (field("consequence"), field("alternative")) {
            (Some(a), Some(b)) => join(&origin(ctx, a, env), &origin(ctx, b, env)),
            _ => Dispatch,
        },
        "binary_expression" => {
            let logical = field("operator").is_some_and(|o| matches!(o.kind(), "||" | "&&" | "??"));
            match (logical, field("left"), field("right")) {
                (true, Some(a), Some(b)) => join(&origin(ctx, a, env), &origin(ctx, b, env)),
                _ => Local,
            }
        }
        _ => Dispatch,
    }
}

/// Identifiers a declaration pattern binds.
fn pattern_names<'t>(pattern: Node<'t>, out: &mut Vec<Node<'t>>) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(pattern),
        "pair_pattern" => {
            if let Some(v) = pattern.child_by_field_name("value") {
                pattern_names(v, out);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(l) = pattern.child_by_field_name("left") {
                pattern_names(l, out);
            }
        }
        _ => {
            for c in named_children(pattern) {
                pattern_names(c, out);
            }
        }
    }
}

/// One scope's declarations, collected without entering nested functions.
struct ScopeWalk<'t> {
    params: Vec<Node<'t>>,
    hoisted: Vec<Node<'t>>,
    declarators: Vec<Node<'t>>,
    loop_items: Vec<Node<'t>>,
    assigns: Vec<Node<'t>>,
}

fn scope_walk(owner: Node<'_>) -> ScopeWalk<'_> {
    let mut w = ScopeWalk {
        params: Vec::new(),
        hoisted: Vec::new(),
        declarators: Vec::new(),
        loop_items: Vec::new(),
        assigns: Vec::new(),
    };
    for field in ["parameters", "parameter"] {
        if let Some(p) = owner.child_by_field_name(field) {
            pattern_names(p, &mut w.params);
        }
    }
    let mut stack: Vec<Node> = children(owner);
    stack.reverse();
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if is_function_like(kind) && node.id() != owner.id() {
            if let Some(n) = node
                .child_by_field_name("name")
                .filter(|n| matches!(n.kind(), "identifier" | "type_identifier"))
            {
                w.hoisted.push(n);
            }
            continue;
        }
        match kind {
            "formal_parameters" => continue,
            "variable_declarator" => w.declarators.push(node),
            "assignment_expression" => w.assigns.push(node),
            "for_in_statement" => {
                if let Some(l) = node.child_by_field_name("left") {
                    pattern_names(l, &mut w.loop_items);
                }
            }
            "catch_clause" => {
                if let Some(p) = node.child_by_field_name("parameter") {
                    pattern_names(p, &mut w.params);
                }
            }
            _ => {}
        }
        let mut kids = children(node);
        kids.reverse();
        stack.extend(kids);
    }
    w
}

fn bind(env: &mut Env, declared: &mut HashSet<String>, name: String, value: Binding) {
    let merged = if declared.contains(&name) {
        env.get(&name)
            .map_or(value.clone(), |old| join(old, &value))
    } else {
        value
    };
    declared.insert(name.clone());
    env.insert(name, merged);
}

/// The environment inside `owner`, given the enclosing one.
fn scope_env(ctx: &FileCtx, owner: Node, outer: &Arc<Env>, is_root: bool) -> Env {
    let w = scope_walk(owner);
    let mut env = Env::child(outer);
    let mut declared = HashSet::new();
    let name_of = |n: Node| text(n, ctx.src).into_owned();
    for p in &w.params {
        bind(&mut env, &mut declared, name_of(*p), Dispatch);
    }
    for n in &w.hoisted {
        bind(&mut env, &mut declared, name_of(*n), Local);
    }
    for n in &w.loop_items {
        bind(&mut env, &mut declared, name_of(*n), Dispatch);
    }
    for d in &w.declarators {
        let Some(pattern) = d.child_by_field_name("name") else {
            continue;
        };
        let value = d
            .child_by_field_name("value")
            .map_or(Local, |v| origin(ctx, v, &env));
        let mut names = Vec::new();
        pattern_names(pattern, &mut names);
        for n in names {
            // Module-scope imports keep their import binding.
            if is_root && outer.contains_key(text(n, ctx.src).as_ref()) {
                continue;
            }
            bind(
                &mut env,
                &mut declared,
                name_of(n),
                derived_pick(pattern, &value),
            );
        }
    }
    for a in &w.assigns {
        let Some(left) = a
            .child_by_field_name("left")
            .filter(|l| l.kind() == "identifier")
        else {
            continue;
        };
        let name = name_of(left);
        let Some(old) = env.get(&name).cloned() else {
            continue;
        };
        if is_root && outer.contains_key(&name) {
            continue;
        }
        let value = a
            .child_by_field_name("right")
            .map_or(Dispatch, |r| origin(ctx, r, &env));
        env.insert(name, join(&old, &value));
    }
    env
}

/// A destructured name holds a member of the value.
fn derived_pick(pattern: Node, value: &Binding) -> Binding {
    if pattern.kind() == "identifier" {
        value.clone()
    } else {
        derived(value.clone())
    }
}

/// Environment of every function-like node (and the root), top-down so
/// inner scopes see outer names. `module` holds the import bindings.
pub fn environments(
    ctx: &FileCtx,
    root: Node,
    module: &HashMap<String, Binding>,
) -> HashMap<usize, Arc<Env>> {
    let mut envs = HashMap::new();
    let module = Arc::new(Env::from_map(module.clone()));
    let root_env = Arc::new(scope_env(ctx, root, &module, true));
    let mut stack = vec![(root, root_env)];
    while let Some((node, env)) = stack.pop() {
        let mut inner = Vec::new();
        let mut probe: Vec<Node> = children(node);
        while let Some(n) = probe.pop() {
            if is_function_like(n.kind()) {
                inner.push(n);
            } else {
                probe.extend(children(n));
            }
        }
        for f in inner {
            let fenv = scope_env(ctx, f, &env, false);
            stack.push((f, Arc::new(fenv)));
        }
        envs.insert(node.id(), env);
    }
    envs
}

#[cfg(test)]
mod env_tests {
    use super::*;

    #[test]
    fn a_scope_reads_like_a_copy_of_its_parent_plus_its_own_names() {
        let module = Arc::new(Env::from_map(HashMap::from([
            ("a".to_owned(), Binding::Module("m".into())),
            ("b".to_owned(), External),
        ])));
        let mut inner = Env::child(&module);
        inner.insert("b".to_owned(), Local); // shadows
        inner.insert("c".to_owned(), Dispatch);
        assert_eq!(inner.get("a"), Some(&Binding::Module("m".into())));
        assert_eq!(inner.get("b"), Some(&Local));
        assert_eq!(module.get("b"), Some(&External), "the parent is untouched");
        assert!(inner.contains_key("c") && !module.contains_key("c"));
        let flat = inner.flatten();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat["b"], Local);
    }
}
