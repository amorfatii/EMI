//! Waste and error-masking rules of `python_probe.waste_findings`, reproduced
//! on ruff's AST with CPython line semantics.
//!
//! The oracle walks CPython's AST breadth-first; findings are sorted by
//! (first line, rule) with walk order breaking ties. Nodes are gathered here
//! with their AST depth and offset so ties resolve the same way in practice.

use std::collections::{BTreeSet, HashMap};

use ruff_python_ast::token::Tokens;
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{
    self as ast, CmpOp, Comprehension, ElifElseClause, ExceptHandler, Expr, ExprContext, Keyword,
    MatchCase, Parameters, Stmt, StmtFunctionDef,
};
use ruff_text_size::{Ranged, TextRange};

use crate::collect::stmt_line;
use crate::lines::LineIndex;

/// Rules whose lines carry V mass; all others are diagnostics.
pub const SCORING_RULES: [&str; 5] = [
    "waste-len-check",
    "waste-bool-return",
    "mask-empty-handler",
    "waste-dict-keys",
    "waste-eq-none-bool",
];

#[derive(Clone, Debug)]
pub struct RuleHit {
    pub rule: &'static str,
    pub lines: Vec<u32>,
    order: (u32, u32),
}

/// Node lists gathered in one pre-order walk, tagged `(depth, offset)`.
#[derive(Default)]
struct Nodes<'a> {
    exprs: Vec<(&'a Expr, (u32, u32))>,
    stmts: Vec<(&'a Stmt, (u32, u32))>,
    handlers: Vec<(&'a ExceptHandler, (u32, u32))>,
    /// CPython-view If nodes: `if` and each `elif` with its orelse.
    ifs: Vec<(IfView<'a>, (u32, u32))>,
    functions: Vec<(&'a StmtFunctionDef, (u32, u32))>,
    depth: u32,
}

/// An If as CPython sees it: `elif` clauses are Ifs nested in orelse.
#[derive(Clone, Copy)]
struct IfView<'a> {
    start: ruff_text_size::TextSize,
    body: &'a [Stmt],
    rest: &'a [ElifElseClause],
}

impl IfView<'_> {
    /// The orelse list's single statement when it is plain code, or `None`
    /// when orelse is empty, longer than one, or a nested `elif`.
    fn single_else(&self) -> Option<&Stmt> {
        match self.rest {
            [only] if only.test.is_none() && only.body.len() == 1 => only.body.first(),
            _ => None,
        }
    }

    fn orelse_len(&self) -> usize {
        match self.rest.first() {
            None => 0,
            Some(c) if c.test.is_some() => 1,
            Some(c) => c.body.len(),
        }
    }
}

impl<'a> Nodes<'a> {
    fn key(&self, range: TextRange) -> (u32, u32) {
        (self.depth, range.start().to_u32())
    }
}

impl<'a> Visitor<'a> for Nodes<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        let key = self.key(stmt.range());
        self.stmts.push((stmt, key));
        if let Stmt::FunctionDef(f) = stmt {
            self.functions.push((f, key));
        }
        if let Stmt::If(s) = stmt {
            self.ifs.push((
                IfView {
                    start: s.start(),
                    body: &s.body,
                    rest: &s.elif_else_clauses,
                },
                key,
            ));
            for (i, clause) in s.elif_else_clauses.iter().enumerate() {
                if clause.test.is_some() {
                    let view = IfView {
                        start: clause.start(),
                        body: &clause.body,
                        rest: &s.elif_else_clauses[i + 1..],
                    };
                    let depth = self.depth + u32::try_from(i + 1).unwrap_or(u32::MAX);
                    self.ifs.push((view, (depth, clause.start().to_u32())));
                }
            }
        }
        self.depth += 1;
        visitor::walk_stmt(self, stmt);
        self.depth -= 1;
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        self.exprs.push((expr, self.key(expr.range())));
        self.depth += 1;
        visitor::walk_expr(self, expr);
        self.depth -= 1;
    }

    fn visit_except_handler(&mut self, handler: &'a ExceptHandler) {
        self.handlers.push((handler, self.key(handler.range())));
        self.depth += 1;
        visitor::walk_except_handler(self, handler);
        self.depth -= 1;
    }

    fn visit_comprehension(&mut self, c: &'a Comprehension) {
        self.depth += 1;
        visitor::walk_comprehension(self, c);
        self.depth -= 1;
    }

    fn visit_keyword(&mut self, k: &'a Keyword) {
        self.depth += 1;
        visitor::walk_keyword(self, k);
        self.depth -= 1;
    }

    fn visit_match_case(&mut self, c: &'a MatchCase) {
        self.depth += 1;
        visitor::walk_match_case(self, c);
        self.depth -= 1;
    }
}

fn is_constant(e: &Expr) -> bool {
    matches!(
        e,
        Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
    )
}

fn is_bool(e: &Expr) -> Option<bool> {
    match e {
        Expr::BooleanLiteral(b) => Some(b.value),
        _ => None,
    }
}

fn name_of(e: &Expr) -> Option<&str> {
    match e {
        Expr::Name(n) => Some(n.id.as_str()),
        _ => None,
    }
}

/// `f(arg)` with a plain-name callee in `names`, one argument, no keywords.
fn single_arg_call<'e>(e: &'e Expr, names: &[&str]) -> Option<&'e Expr> {
    let Expr::Call(c) = e else { return None };
    let func = name_of(&c.func)?;
    (names.contains(&func) && c.arguments.args.len() == 1 && c.arguments.keywords.is_empty())
        .then(|| &c.arguments.args[0])
}

/// Single-operator comparison: `(left, op, right)`.
fn simple_compare(e: &Expr) -> Option<(&Expr, CmpOp, &Expr)> {
    let Expr::Compare(c) = e else { return None };
    match (&*c.ops, &*c.operands) {
        ([op], [left, right]) => Some((left, *op, right)),
        _ => None,
    }
}

/// Numeric zero other than `False` (`0 == 0.0 == 0j` in Python).
fn is_zero(e: &Expr) -> bool {
    let Expr::NumberLiteral(n) = e else {
        return false;
    };
    match &n.value {
        ast::Number::Int(i) => i.as_u64() == Some(0),
        ast::Number::Float(f) => *f == 0.0,
        ast::Number::Complex { real, imag } => *real == 0.0 && *imag == 0.0,
    }
}

/// Unfiltered synchronous single generator whose element is its own target.
fn identity_generator(elt: &Expr, generators: &[Comprehension]) -> bool {
    let [g] = generators else { return false };
    !g.is_async
        && g.ifs.is_empty()
        && matches!((name_of(&g.target), name_of(elt)), (Some(t), Some(e)) if t == e)
}

struct Ctx<'a> {
    index: &'a LineIndex<'a>,
    tokens: &'a Tokens,
    hits: Vec<RuleHit>,
}

impl Ctx<'_> {
    fn hit(&mut self, rule: &'static str, first: u32, last: u32, order: (u32, u32)) {
        self.hits.push(RuleHit {
            rule,
            lines: (first..=last).collect(),
            order,
        });
    }

    fn hit_range(&mut self, rule: &'static str, range: TextRange, order: (u32, u32)) {
        let (first, last) = (
            self.index.line(range.start()),
            self.index.end_line(range.end()),
        );
        self.hit(rule, first, last, order);
    }

    fn stmt_lines(&self, stmt: &Stmt) -> (u32, u32) {
        (
            stmt_line(stmt, self.tokens, self.index),
            self.index.end_line(stmt.end()),
        )
    }
}

fn expr_rules(ctx: &mut Ctx, nodes: &Nodes) {
    for &(e, order) in &nodes.exprs {
        identity_comp(ctx, e, order);
        len_check(ctx, e, order);
        join_list(ctx, e, order);
        dict_keys(ctx, e, order);
        eq_none_bool(ctx, e, order);
        type_eq(ctx, e, order);
    }
}

fn identity_comp(ctx: &mut Ctx, e: &Expr, order: (u32, u32)) {
    let flagged = match e {
        Expr::ListComp(c) => identity_generator(&c.elt, &c.generators),
        _ => matches!(single_arg_call(e, &["list", "set"]), Some(Expr::Generator(g))
            if identity_generator(&g.elt, &g.generators)),
    };
    if flagged {
        ctx.hit_range("waste-identity-comp", e.range(), order);
    }
}

fn len_check(ctx: &mut Ctx, e: &Expr, order: (u32, u32)) {
    let Some((left, op, right)) = simple_compare(e) else {
        return;
    };
    if !matches!(
        op,
        CmpOp::Eq | CmpOp::NotEq | CmpOp::Lt | CmpOp::LtE | CmpOp::Gt | CmpOp::GtE
    ) {
        return;
    }
    let is_len = |x: &Expr| single_arg_call(x, &["len"]).is_some();
    if (is_len(left) && is_zero(right)) || (is_len(right) && is_zero(left)) {
        ctx.hit_range("waste-len-check", e.range(), order);
    }
}

fn join_list(ctx: &mut Ctx, e: &Expr, order: (u32, u32)) {
    let Expr::Call(c) = e else { return };
    let [Expr::ListComp(_)] = &*c.arguments.args else {
        return;
    };
    if !c.arguments.keywords.is_empty() {
        return;
    }
    let target = match &*c.func {
        Expr::Attribute(a) => a.attr.as_str() == "join",
        Expr::Name(n) => matches!(n.id.as_str(), "sum" | "any" | "all" | "max" | "min"),
        _ => false,
    };
    if target {
        ctx.hit_range("waste-join-list", e.range(), order);
    }
}

fn dict_keys(ctx: &mut Ctx, e: &Expr, order: (u32, u32)) {
    let Some((left, op, right)) = simple_compare(e) else {
        return;
    };
    if !matches!(op, CmpOp::In | CmpOp::NotIn) {
        return;
    }
    let call = if matches!(left, Expr::Call(_)) {
        left
    } else {
        right
    };
    if let Expr::Call(c) = call
        && let Expr::Attribute(a) = &*c.func
        && a.attr.as_str() == "keys"
        && c.arguments.args.is_empty()
        && c.arguments.keywords.is_empty()
    {
        ctx.hit_range("waste-dict-keys", e.range(), order);
    }
}

fn eq_none_bool(ctx: &mut Ctx, e: &Expr, order: (u32, u32)) {
    let Some((left, op, right)) = simple_compare(e) else {
        return;
    };
    if !matches!(op, CmpOp::Eq | CmpOp::NotEq) {
        return;
    }
    let singleton = |x: &Expr| matches!(x, Expr::BooleanLiteral(_) | Expr::NoneLiteral(_));
    if (singleton(left) && !is_constant(right)) || (singleton(right) && !is_constant(left)) {
        ctx.hit_range("waste-eq-none-bool", e.range(), order);
    }
}

fn type_eq(ctx: &mut Ctx, e: &Expr, order: (u32, u32)) {
    let Some((left, op, right)) = simple_compare(e) else {
        return;
    };
    if !matches!(op, CmpOp::Eq | CmpOp::Is) {
        return;
    }
    let is_type = |x: &Expr| single_arg_call(x, &["type"]).is_some();
    if (is_type(left) && name_of(right).is_some()) || (is_type(right) && name_of(left).is_some()) {
        ctx.hit_range("waste-type-eq", e.range(), order);
    }
}

fn bool_return(ctx: &mut Ctx, nodes: &Nodes) {
    for &(view, order) in &nodes.ifs {
        let (Some(Stmt::Return(then)), Some(Stmt::Return(els))) =
            (view.body.first(), view.single_else())
        else {
            continue;
        };
        if view.body.len() != 1 || view.orelse_len() != 1 {
            continue;
        }
        let values = (
            then.value.as_deref().and_then(is_bool),
            els.value.as_deref().and_then(is_bool),
        );
        if let (Some(a), Some(b)) = values
            && a != b
        {
            let last = ctx.index.end_line(els.end());
            let first = ctx.index.line(view.start);
            ctx.hit("waste-bool-return", first, last, order);
        }
    }
    for &(s, order) in &nodes.stmts {
        if let Stmt::Return(r) = s
            && let Some(Expr::If(i)) = r.value.as_deref()
            && let (Some(a), Some(b)) = (is_bool(&i.body), is_bool(&i.orelse))
            && a != b
        {
            ctx.hit_range("waste-bool-return", s.range(), order);
        }
    }
}

fn is_inert(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Pass(_) => true,
        Stmt::Expr(e) => is_constant(&e.value),
        Stmt::AnnAssign(a) => a.value.is_none(),
        _ => false,
    }
}

fn handlers(ctx: &mut Ctx, nodes: &Nodes) {
    for &(h, order) in &nodes.handlers {
        let ExceptHandler::ExceptHandler(h) = h;
        if h.type_.is_none() {
            ctx.hit_range("mask-bare-except", h.range(), order);
        }
    }
    for &(h, order) in &nodes.handlers {
        let ExceptHandler::ExceptHandler(h) = h;
        if !h.body.is_empty() && h.body.iter().all(is_inert) {
            ctx.hit_range("mask-empty-handler", h.range(), order);
        }
    }
}

fn range_len(ctx: &mut Ctx, nodes: &Nodes) {
    for &(s, order) in &nodes.stmts {
        let Stmt::For(f) = s else { continue };
        if f.is_async {
            continue;
        }
        let Some(bound) = single_arg_call(&f.iter, &["range"]) else {
            continue;
        };
        let Some(Expr::Name(seq)) = single_arg_call(bound, &["len"]) else {
            continue;
        };
        let Some(var) = name_of(&f.target) else {
            continue;
        };
        let mut finder = SubscriptFinder {
            seq: seq.id.as_str(),
            var,
            found: false,
        };
        finder.visit_stmt(s);
        if finder.found {
            let (first, last) = ctx.stmt_lines(s);
            ctx.hit("waste-range-len", first, last, order);
        }
    }
}

struct SubscriptFinder<'n> {
    seq: &'n str,
    var: &'n str,
    found: bool,
}

impl<'a> Visitor<'a> for SubscriptFinder<'_> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Subscript(s) = expr
            && name_of(&s.value) == Some(self.seq)
            && name_of(&s.slice) == Some(self.var)
        {
            self.found = true;
        }
        visitor::walk_expr(self, expr);
    }
}

/// Loads of each name anywhere inside a function (nested scopes included).
fn name_loads(f: &StmtFunctionDef) -> HashMap<String, u32> {
    struct Loads(HashMap<String, u32>);
    impl<'a> Visitor<'a> for Loads {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Name(n) = expr
                && n.ctx == ExprContext::Load
            {
                *self.0.entry(n.id.to_string()).or_insert(0) += 1;
            }
            visitor::walk_expr(self, expr);
        }
    }
    let mut loads = Loads(HashMap::new());
    walk_function(&mut loads, f);
    loads.0
}

/// Walk everything `ast.walk(fn)` reaches, without cloning the node.
fn walk_function<'a, V: Visitor<'a>>(v: &mut V, f: &'a StmtFunctionDef) {
    for d in &f.decorator_list {
        v.visit_decorator(d);
    }
    if let Some(tp) = &f.type_params {
        v.visit_type_params(tp);
    }
    v.visit_parameters(&f.parameters);
    if let Some(r) = &f.returns {
        v.visit_annotation(r);
    }
    v.visit_body(&f.body);
}

/// Every statement list inside a function (bodies, orelse, finalbody,
/// handler and case bodies), the function's own body first.
fn blocks(f: &StmtFunctionDef) -> Vec<&[Stmt]> {
    struct Blocks<'a>(Vec<&'a [Stmt]>);
    impl<'a> Visitor<'a> for Blocks<'a> {
        fn visit_body(&mut self, body: &'a [Stmt]) {
            self.0.push(body);
            visitor::walk_body(self, body);
        }
    }
    let mut b = Blocks(Vec::new());
    b.visit_body(&f.body);
    b.0
}

fn assigned_name(stmt: &Stmt) -> Option<&str> {
    match stmt {
        Stmt::Assign(a) if a.targets.len() == 1 => name_of(&a.targets[0]),
        Stmt::AnnAssign(a) if a.value.is_some() => name_of(&a.target),
        _ => None,
    }
}

fn single_use_return(ctx: &mut Ctx, nodes: &Nodes) {
    for &(f, order) in &nodes.functions {
        let loads = name_loads(f);
        for block in blocks(f) {
            for pair in block.windows(2) {
                let [first, Stmt::Return(ret)] = pair else {
                    continue;
                };
                let Some(target) = assigned_name(first) else {
                    continue;
                };
                if loads.get(target) == Some(&1)
                    && ret.value.as_deref().and_then(name_of) == Some(target)
                {
                    let (start, _) = ctx.stmt_lines(first);
                    let end = ctx.index.end_line(ret.end());
                    ctx.hit("waste-single-use-return", start, end, order);
                }
            }
        }
    }
}

const DOC_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "for", "with", "by", "on", "in", "is", "are", "be",
    "as", "at", "from", "that", "this", "it", "its", "given", "using", "into", "over",
];

use emi_ir::pytext::lower_ascii_words as words;

/// Every parameter name reachable from a signature, lambda defaults included.
fn parameter_names(params: &Parameters) -> Vec<String> {
    struct Names(Vec<String>);
    impl<'a> Visitor<'a> for Names {
        fn visit_parameter(&mut self, p: &'a ast::Parameter) {
            self.0.push(p.name.to_string());
            visitor::walk_parameter(self, p);
        }
    }
    let mut n = Names(Vec::new());
    n.visit_parameters(params);
    n.0
}

fn doc_restates_signature(ctx: &mut Ctx, nodes: &Nodes) {
    for &(f, order) in &nodes.functions {
        let Some(Stmt::Expr(first)) = f.body.first() else {
            continue;
        };
        let Expr::StringLiteral(s) = &*first.value else {
            continue;
        };
        let doc = s.value.to_str();
        if doc.contains(">>>") || doc.matches('\n').count() > 2 {
            continue;
        }
        let mut signature = words(f.name.as_str());
        for arg in parameter_names(&f.parameters) {
            if arg != "self" && arg != "cls" {
                signature.extend(words(&arg));
            }
        }
        let content: BTreeSet<String> = words(doc)
            .into_iter()
            .filter(|w| !DOC_STOPWORDS.contains(&w.as_str()))
            .collect();
        if !content.is_empty() && content.is_subset(&signature) {
            ctx.hit_range("doc-restates-signature", first.range(), order);
        }
    }
}

/// All waste/mask findings for one module, in the oracle's order.
pub fn findings(module: &ast::ModModule, tokens: &Tokens, index: &LineIndex) -> Vec<RuleHit> {
    let mut nodes = Nodes::default();
    nodes.visit_body(&module.body);
    let mut ctx = Ctx {
        index,
        tokens,
        hits: Vec::new(),
    };
    // Finder order follows the oracle; the final sort is by (first line,
    // rule), so only same-rule ties depend on it.
    expr_rules(&mut ctx, &nodes);
    single_use_return(&mut ctx, &nodes);
    bool_return(&mut ctx, &nodes);
    handlers(&mut ctx, &nodes);
    range_len(&mut ctx, &nodes);
    doc_restates_signature(&mut ctx, &nodes);
    let mut hits = ctx.hits;
    hits.sort_by(|a, b| (a.lines[0], a.rule, a.order).cmp(&(b.lines[0], b.rule, b.order)));
    hits
}
