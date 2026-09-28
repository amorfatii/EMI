//! `region_dependencies`: scope bindings of one region, then resolution of
//! its identifier, call, require/import and JSX uses.

use std::collections::{BTreeSet, HashMap, HashSet};

use tree_sitter::Node;

use super::{
    Binding, DYNAMIC_CALLEES, FileCtx, FileFacts, bind_require_shape, declare, enclosing_class,
    heritage, is_function_like, is_global, literal_arg, require_names, require_target,
    shadowed_require,
};
use crate::parse::{push_children, row};
use crate::resolve::Target;
use crate::rules::{children, is_binding_position, named_children, text, walk};
use Binding::{Local, Unknown};

#[derive(Clone, Debug, PartialEq)]
pub struct Unresolved {
    pub line: u32,
    pub column: u32,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct RegionCoupling {
    pub deps: BTreeSet<String>,
    pub unresolved: Vec<Unresolved>,
    pub local_callees: Vec<String>,
}

struct Walked<'t> {
    scope: Vec<Node<'t>>,
    assigns: Vec<Node<'t>>,
    uses: Vec<Node<'t>>,
    jsx: Vec<Node<'t>>,
}

fn collect<'t>(region: Node<'t>, lines: &BTreeSet<u32>) -> Walked<'t> {
    let mut w = Walked {
        scope: Vec::new(),
        assigns: Vec::new(),
        uses: Vec::new(),
        jsx: Vec::new(),
    };
    let mut stack = vec![region];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        let own_params =
            kind == "formal_parameters" && node.parent().is_some_and(|p| p.id() == region.id());
        if own_params || kind == "catch_clause" {
            w.scope.push(node);
        }
        if lines.contains(&row(node.start_position().row)) {
            match kind {
                "variable_declarator" | "assignment_expression" => w.assigns.push(node),
                "identifier"
                | "type_identifier"
                | "shorthand_property_identifier"
                | "call_expression" => w.uses.push(node),
                "jsx_opening_element" | "jsx_self_closing_element" => w.jsx.push(node),
                _ => {}
            }
        }
        push_children(&mut stack, node);
    }
    w
}

struct Region<'a, 'c> {
    ctx: &'a FileCtx<'c>,
    facts: &'a FileFacts<'a>,
    node: Node<'a>,
    bindings: HashMap<String, Binding>,
    out: RegionCoupling,
    seen: HashSet<(u32, u32, &'static str)>,
    callees: BTreeSet<String>,
    heritage: Option<(bool, BTreeSet<String>)>,
}

impl Region<'_, '_> {
    fn src(&self) -> &[u8] {
        self.ctx.src
    }

    fn unknown(&mut self, node: Node, reason: &'static str) {
        let p = node.start_position();
        let key = (
            row(p.row),
            u32::try_from(p.column).unwrap_or(u32::MAX),
            reason,
        );
        if self.seen.insert(key) {
            self.out.unresolved.push(Unresolved {
                line: key.0,
                column: key.1,
                reason,
            });
        }
    }

    fn dep(&mut self, binding: &Binding) {
        if let Some(m) = self.ctx.module_of(binding).filter(|m| m != self.ctx.module) {
            self.out.deps.insert(m);
        }
    }

    fn use_name(&mut self, name: &str) {
        let b = self.bindings.get(name).cloned().unwrap_or(Unknown);
        self.dep(&b);
    }

    fn call_name(
        &mut self,
        name: &str,
        node: Node,
        ambiguous: &HashSet<String>,
    ) -> Option<Binding> {
        if DYNAMIC_CALLEES.contains(&name) {
            self.unknown(node, "dynamic callable lookup");
            return None;
        }
        let b = self.bindings.get(name).cloned().unwrap_or(Unknown);
        if self.ctx.module_of(&b).is_some_and(|m| m != self.ctx.module) {
            self.dep(&b);
        } else if b == Unknown {
            if ambiguous.contains(name) {
                self.unknown(node, "conflicting import binding");
            } else if !(super::is_runtime_global(name, self.ctx.scope.precise)
                && !self.bindings.contains_key(name))
            {
                self.unknown(node, "unresolved callable");
            }
        }
        Some(b)
    }

    fn heritage_deps(&mut self) -> (bool, BTreeSet<String>) {
        if self.heritage.is_none() && self.ctx.scope.precise {
            let resolved = this_class(self.node).map_or((false, BTreeSet::new()), |cls| {
                heritage(cls, &self.bindings, &self.facts.classes, self.ctx.src, true)
            });
            self.heritage = Some(resolved);
        }
        if self.heritage.is_none() {
            let resolved = if self.node.kind() == "method_definition" {
                enclosing_class(self.node).map_or((false, BTreeSet::new()), |cls| {
                    heritage(
                        cls,
                        &self.bindings,
                        &self.facts.classes,
                        self.ctx.src,
                        self.ctx.scope.precise,
                    )
                })
            } else {
                (false, BTreeSet::new())
            };
            self.heritage = Some(resolved);
        }
        self.heritage.clone().unwrap_or_default()
    }

    fn heritage_or(&mut self, node: Node, reason: &'static str) {
        let (known, found) = self.heritage_deps();
        if known {
            for dep in found {
                if dep != self.ctx.module && self.ctx.scope.known_modules.contains(&dep) {
                    self.out.deps.insert(dep);
                }
            }
        } else {
            self.unknown(node, reason);
        }
    }

    fn loader(&mut self, node: Node, what: &str) {
        let (non_literal, unresolvable) = if what == "require" {
            (
                "require of non-literal specifier",
                "require of unresolvable specifier",
            )
        } else {
            (
                "dynamic import of non-literal specifier",
                "dynamic import of unresolvable specifier",
            )
        };
        match literal_arg(node, self.src()) {
            None => self.unknown(node, non_literal),
            Some(spec) => match self.ctx.resolve(&spec) {
                None => self.unknown(node, unresolvable),
                Some(Target::Module(m)) if m != self.ctx.module => {
                    self.out.deps.insert(m);
                }
                Some(_) => {}
            },
        }
    }
}

fn scope_phase(r: &mut Region, scope: &[Node]) {
    for node in scope {
        if node.kind() == "formal_parameters" {
            for sub in walk(*node) {
                if sub.kind() == "identifier" && is_binding_position(sub) {
                    r.bindings
                        .insert(text(sub, r.ctx.src).into_owned(), Unknown);
                }
            }
        } else if let Some(param) = node.child_by_field_name("parameter") {
            let targets = if param.kind() == "identifier" {
                vec![param]
            } else {
                walk(param)
                    .into_iter()
                    .filter(|s| s.kind() == "identifier")
                    .collect()
            };
            for sub in targets {
                r.bindings
                    .insert(text(sub, r.ctx.src).into_owned(), Unknown);
            }
        }
    }
}

fn declarator(r: &mut Region, node: Node, ambiguous: &mut HashSet<String>) {
    let src = r.ctx.src;
    let (Some(name), value) = (
        node.child_by_field_name("name"),
        node.child_by_field_name("value"),
    ) else {
        return;
    };
    match name.kind() {
        "identifier" => {
            let local = text(name, src).into_owned();
            if let Some(target) = require_target(r.ctx, value, &r.bindings) {
                bind_require_shape(name, &target, &mut r.bindings, ambiguous, src);
            } else {
                // A shadowed require, an alias of an unknown value, or a
                // rebinding is unprovable; a fresh name is a local value.
                let aliases_unknown = value.is_some_and(|v| {
                    v.kind() == "identifier"
                        && r.bindings.get(text(v, src).as_ref()) == Some(&Unknown)
                });
                let unprovable = shadowed_require(value, &r.bindings, src)
                    || aliases_unknown
                    || r.bindings.contains_key(&local);
                r.bindings
                    .insert(local, if unprovable { Unknown } else { Local });
            }
        }
        "object_pattern" => {
            if let Some(target) = require_target(r.ctx, value, &r.bindings) {
                bind_require_shape(name, &target, &mut r.bindings, ambiguous, src);
            } else if shadowed_require(value, &r.bindings, src) {
                for (local, _) in require_names(name, src).unwrap_or_default() {
                    r.bindings.insert(local, Unknown);
                }
            }
        }
        _ => {}
    }
}

fn assignment(r: &mut Region, node: Node, ambiguous: &mut HashSet<String>) {
    let src = r.ctx.src;
    let Some(left) = node
        .child_by_field_name("left")
        .filter(|l| l.kind() == "identifier")
    else {
        return;
    };
    let value = node.child_by_field_name("value");
    let name = text(left, src).into_owned();
    if let Some(target) = require_target(r.ctx, value, &r.bindings) {
        declare(&mut r.bindings, ambiguous, &name, target);
    } else if shadowed_require(value, &r.bindings, src) || r.bindings.contains_key(&name) {
        r.bindings.insert(name, Unknown);
    }
}

/// `(kind, prop)` of a member callee: direct this/super or deeper chains.
fn this_super(func: Node) -> Option<&'static str> {
    let obj = func.child_by_field_name("object")?;
    let direct = func.child_by_field_name("property").is_some_and(|p| {
        matches!(
            p.kind(),
            "property_identifier" | "private_property_identifier"
        )
    });
    match obj.kind() {
        "this" => Some(if direct { "this" } else { "this-chain" }),
        "super" => Some(if direct { "super" } else { "super-chain" }),
        _ => {
            let mut probe = Some(obj);
            while let Some(p) = probe.filter(|p| p.kind() == "member_expression") {
                probe = p.child_by_field_name("object");
            }
            match probe.map(|p| p.kind()) {
                Some("this") => Some("this-chain"),
                Some("super") => Some("super-chain"),
                _ => None,
            }
        }
    }
}

fn callee_root(func: Node) -> Option<Node> {
    let mut f = Some(func);
    while let Some(p) = f.filter(|p| p.kind() == "parenthesized_expression") {
        f = named_children(p).first().copied();
    }
    while let Some(m) = f.filter(|m| m.kind() == "member_expression") {
        f = m.child_by_field_name("object");
    }
    f
}

fn call(r: &mut Region, node: Node, ambiguous: &HashSet<String>) {
    let Some(func) = node.child_by_field_name("function") else {
        return;
    };
    match func.kind() {
        "identifier" => {
            let name = text(func, r.ctx.src).into_owned();
            if name == "require" && !r.bindings.contains_key("require") {
                r.loader(node, "require");
            } else if r.call_name(&name, node, ambiguous) == Some(Local) {
                r.callees.insert(name);
            }
        }
        "import" => r.loader(node, "import"),
        "member_expression" if r.ctx.scope.precise => precise_member_call(r, node, func, ambiguous),
        "subscript_expression" if r.ctx.scope.precise => {
            precise_member_call(r, node, func, ambiguous)
        }
        "member_expression" => match this_super(func) {
            Some("this") => r.heritage_or(node, "this call (unresolved receiver)"),
            Some("super") => r.heritage_or(node, "super call"),
            Some("this-chain") => r.unknown(node, "this call (unresolved receiver)"),
            Some(_) => r.unknown(node, "super call"),
            None => match callee_root(func) {
                Some(root) if root.kind() == "identifier" => {
                    let name = text(root, r.ctx.src).into_owned();
                    r.call_name(&name, node, ambiguous);
                }
                Some(root) if root.kind() == "this" => {
                    r.unknown(node, "this call (unresolved receiver)")
                }
                Some(root) if root.kind() == "super" => r.unknown(node, "super call"),
                Some(root) if root.kind() == "import" => {
                    r.unknown(node, "dynamic import of non-literal specifier")
                }
                _ => {}
            },
        },
        "subscript_expression" => r.unknown(node, "computed call (unresolved receiver)"),
        "super" => {
            let heritage = enclosing_class(r.node)
                .is_some_and(|c| children(c).iter().any(|x| x.kind() == "class_heritage"));
            if heritage {
                r.heritage_or(node, "super call");
            } else {
                r.unknown(node, "super call");
            }
        }
        _ => {}
    }
}

/// The class whose instance `this` denotes in `region`: arrow functions
/// inherit `this` from their enclosing function; methods and class
/// fields bind it to the class; any other function rebinds it.
fn this_class(region: Node) -> Option<Node> {
    let mut current = Some(region);
    while let Some(node) = current {
        match node.kind() {
            "method_definition"
            | "field_definition"
            | "public_field_definition"
            | "class_static_block" => {
                return enclosing_class(node);
            }
            "arrow_function" | "class_body" => {}
            kind if super::is_function_like(kind) => return None,
            "program" => return None,
            _ => {}
        }
        current = node.parent();
    }
    None
}

/// Precise profile: a member or computed call depends on its receiver's
/// origin. `this` inside a class resolves through the class ancestry;
/// fields and non-class `this` are dynamic dispatch.
fn precise_member_call(r: &mut Region, node: Node, func: Node, ambiguous: &HashSet<String>) {
    match this_super(func) {
        Some("this") if this_class(r.node).is_some() => {
            return r.heritage_or(node, "this call (unresolved receiver)");
        }
        Some("super") => return r.heritage_or(node, "super call"),
        Some("super-chain") => return r.unknown(node, "super call"),
        Some(_) => return,
        None => {}
    }
    let mut root = Some(func);
    while let Some(n) = root.filter(|n| {
        matches!(
            n.kind(),
            "member_expression" | "subscript_expression" | "parenthesized_expression"
        )
    }) {
        root = n
            .child_by_field_name("object")
            .or_else(|| named_children(n).first().copied());
    }
    match root {
        Some(n) if n.kind() == "identifier" => {
            let name = text(n, r.ctx.src).into_owned();
            r.call_name(&name, node, ambiguous);
        }
        Some(n) if n.kind() == "import" => {
            r.unknown(node, "dynamic import of non-literal specifier")
        }
        _ => {}
    }
}

fn jsx(r: &mut Region, node: Node, ambiguous: &HashSet<String>) {
    let Some(name_node) = named_children(node).first().copied() else {
        return;
    };
    let full = text(name_node, r.ctx.src).into_owned();
    let root_name = full
        .split('.')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_owned();
    let intrinsic = !full.contains('.')
        && !full.contains(':')
        && root_name.chars().next().is_some_and(char::is_lowercase);
    if intrinsic || is_global(&root_name) {
        return;
    }
    let b = r.bindings.get(&root_name).cloned().unwrap_or(Unknown);
    if r.ctx.module_of(&b).is_some_and(|m| m != r.ctx.module) {
        r.dep(&b);
    } else if b == Unknown {
        let reason = if ambiguous.contains(&root_name) {
            "conflicting import binding"
        } else {
            "unresolved component"
        };
        r.unknown(node, reason);
    }
}

/// Coupling of one region; `ambiguous` is the file's set, shared across regions.
pub fn region_coupling(
    ctx: &FileCtx,
    facts: &FileFacts,
    node: Node,
    lines: &BTreeSet<u32>,
    ambiguous: &mut HashSet<String>,
) -> RegionCoupling {
    let precise = facts.envs.get(&node.id());
    let mut bindings = precise.map_or_else(|| facts.imports.aliases.clone(), |env| env.flatten());
    for name in &facts.symbols {
        bindings.entry(name.clone()).or_insert(Local);
    }
    for child in children(node) {
        if is_function_like(child.kind())
            && let Some(n) = child
                .child_by_field_name("name")
                .filter(|n| n.kind() == "identifier")
        {
            declare(&mut bindings, ambiguous, &text(n, ctx.src), Local);
        }
    }
    let w = collect(node, lines);
    let mut r = Region {
        ctx,
        facts,
        node,
        bindings,
        out: RegionCoupling::default(),
        seen: HashSet::new(),
        callees: BTreeSet::new(),
        heritage: None,
    };
    if precise.is_none() {
        bind_parity(&mut r, &w, ambiguous);
    }
    uses(&mut r, &w, ambiguous);
    r.out.local_callees = r.callees.into_iter().collect();
    r.out
}

/// Parity binder: the region's own parameters and declarations.
fn bind_parity(r: &mut Region, w: &Walked, ambiguous: &mut HashSet<String>) {
    scope_phase(r, &w.scope);
    for n in &w.assigns {
        if n.kind() == "variable_declarator" {
            declarator(r, *n, ambiguous);
        } else {
            assignment(r, *n, ambiguous);
        }
    }
}

fn uses(r: &mut Region, w: &Walked, ambiguous: &HashSet<String>) {
    let ctx = r.ctx;
    for n in &w.uses {
        match n.kind() {
            "identifier" | "type_identifier" => {
                let name = text(*n, ctx.src);
                if !is_binding_position(*n) && !super::is_runtime_global(&name, ctx.scope.precise) {
                    r.use_name(&name);
                }
            }
            "shorthand_property_identifier" => r.use_name(&text(*n, ctx.src)),
            _ => call(r, *n, ambiguous),
        }
    }
    for n in &w.jsx {
        jsx(r, *n, ambiguous);
    }
}
