//! A CPython-shaped view of ruff's AST: children in CPython `_fields` order,
//! `elif` as nested `If` nodes, and CPython's `(lineno, col_offset)` rules.
//! `bfs` reproduces `ast.walk` order for every node kind the adapter reads.
//!
//! Constants, expression contexts and operators are leaves in CPython that
//! never influence the relative order of other nodes, so they are omitted.

use std::collections::VecDeque;

use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_python_ast::{
    self as ast, ElifElseClause, ExceptHandler, Expr, InterpolatedStringElement, Parameters, Pattern,
    Stmt, TypeParam,
};
use ruff_text_size::{Ranged, TextSize};

use crate::lines::LineIndex;

#[derive(Clone, Copy)]
pub enum PyNode<'a> {
    Module(&'a ast::ModModule),
    Stmt(&'a Stmt),
    /// The `If` CPython builds for `elif` clause `rest[0]`, with `rest[1..]` as its orelse.
    Elif(&'a [ElifElseClause]),
    Expr(&'a Expr),
    Handler(&'a ast::ExceptHandlerExceptHandler),
    Arguments(&'a Parameters),
    Arg(&'a ast::Parameter),
    Keyword(&'a ast::Keyword),
    Alias(&'a ast::Alias),
    WithItem(&'a ast::WithItem),
    MatchCase(&'a ast::MatchCase),
    Pattern(&'a Pattern),
    Comprehension(&'a ast::Comprehension),
    TypeParam(&'a TypeParam),
    Interpolation(&'a ast::InterpolatedElement),
    FormatSpec(&'a ast::InterpolatedStringFormatSpec),
}

fn push_stmts<'a>(out: &mut Vec<PyNode<'a>>, body: &'a [Stmt]) {
    out.extend(body.iter().map(PyNode::Stmt));
}

fn push_expr<'a>(out: &mut Vec<PyNode<'a>>, e: Option<&'a Expr>) {
    if let Some(e) = e {
        out.push(PyNode::Expr(e));
    }
}

fn push_exprs<'a>(out: &mut Vec<PyNode<'a>>, items: &'a [Expr]) {
    out.extend(items.iter().map(PyNode::Expr));
}

/// CPython `orelse` of an If whose remaining clauses are `rest`.
fn push_orelse<'a>(out: &mut Vec<PyNode<'a>>, rest: &'a [ElifElseClause]) {
    match rest.first() {
        Some(c) if c.test.is_some() => out.push(PyNode::Elif(rest)),
        Some(c) => push_stmts(out, &c.body),
        None => {}
    }
}

fn push_type_params<'a>(out: &mut Vec<PyNode<'a>>, tp: Option<&'a ast::TypeParams>) {
    if let Some(tp) = tp {
        out.extend(tp.type_params.iter().map(PyNode::TypeParam));
    }
}

fn push_interpolations<'a>(out: &mut Vec<PyNode<'a>>, elements: &'a ast::InterpolatedStringElements) {
    for el in elements {
        if let InterpolatedStringElement::Interpolation(i) = el {
            out.push(PyNode::Interpolation(i));
        }
    }
}

fn stmt_children<'a>(s: &'a Stmt, out: &mut Vec<PyNode<'a>>) {
    match s {
        Stmt::FunctionDef(f) => {
            out.push(PyNode::Arguments(&f.parameters));
            push_stmts(out, &f.body);
            out.extend(f.decorator_list.iter().map(|d| PyNode::Expr(&d.expression)));
            push_expr(out, f.returns.as_deref());
            push_type_params(out, f.type_params.as_deref());
        }
        Stmt::ClassDef(c) => {
            if let Some(a) = c.arguments.as_deref() {
                push_exprs(out, &a.args);
                out.extend(a.keywords.iter().map(PyNode::Keyword));
            }
            push_stmts(out, &c.body);
            out.extend(c.decorator_list.iter().map(|d| PyNode::Expr(&d.expression)));
            push_type_params(out, c.type_params.as_deref());
        }
        Stmt::Return(r) => push_expr(out, r.value.as_deref()),
        Stmt::Delete(d) => push_exprs(out, &d.targets),
        Stmt::TypeAlias(t) => {
            out.push(PyNode::Expr(&t.name));
            push_type_params(out, t.type_params.as_deref());
            out.push(PyNode::Expr(&t.value));
        }
        Stmt::Assign(a) => {
            push_exprs(out, &a.targets);
            out.push(PyNode::Expr(&a.value));
        }
        Stmt::AugAssign(a) => {
            out.push(PyNode::Expr(&a.target));
            out.push(PyNode::Expr(&a.value));
        }
        Stmt::AnnAssign(a) => {
            out.push(PyNode::Expr(&a.target));
            out.push(PyNode::Expr(&a.annotation));
            push_expr(out, a.value.as_deref());
        }
        Stmt::For(f) => {
            out.push(PyNode::Expr(&f.target));
            out.push(PyNode::Expr(&f.iter));
            push_stmts(out, &f.body);
            push_stmts(out, &f.orelse);
        }
        Stmt::While(w) => {
            out.push(PyNode::Expr(&w.test));
            push_stmts(out, &w.body);
            push_stmts(out, &w.orelse);
        }
        Stmt::If(i) => {
            out.push(PyNode::Expr(&i.test));
            push_stmts(out, &i.body);
            push_orelse(out, &i.elif_else_clauses);
        }
        Stmt::With(w) => {
            out.extend(w.items.iter().map(PyNode::WithItem));
            push_stmts(out, &w.body);
        }
        Stmt::Match(m) => {
            out.push(PyNode::Expr(&m.subject));
            out.extend(m.cases.iter().map(PyNode::MatchCase));
        }
        Stmt::Raise(r) => {
            push_expr(out, r.exc.as_deref());
            push_expr(out, r.cause.as_deref());
        }
        Stmt::Try(t) => {
            push_stmts(out, &t.body);
            out.extend(t.handlers.iter().map(|h| match h {
                ExceptHandler::ExceptHandler(h) => PyNode::Handler(h),
            }));
            push_stmts(out, &t.orelse);
            push_stmts(out, &t.finalbody);
        }
        Stmt::Assert(a) => {
            out.push(PyNode::Expr(&a.test));
            push_expr(out, a.msg.as_deref());
        }
        Stmt::Import(i) => out.extend(i.names.iter().map(PyNode::Alias)),
        Stmt::ImportFrom(i) => out.extend(i.names.iter().map(PyNode::Alias)),
        Stmt::Expr(e) => out.push(PyNode::Expr(&e.value)),
        _ => {}
    }
}

fn expr_children<'a>(e: &'a Expr, out: &mut Vec<PyNode<'a>>) {
    match e {
        Expr::BoolOp(b) => push_exprs(out, &b.values),
        Expr::Named(n) => {
            out.push(PyNode::Expr(&n.target));
            out.push(PyNode::Expr(&n.value));
        }
        Expr::BinOp(b) => {
            out.push(PyNode::Expr(&b.left));
            out.push(PyNode::Expr(&b.right));
        }
        Expr::UnaryOp(u) => out.push(PyNode::Expr(&u.operand)),
        Expr::Lambda(l) => {
            if let Some(p) = l.parameters.as_deref() {
                out.push(PyNode::Arguments(p));
            }
            out.push(PyNode::Expr(&l.body));
        }
        Expr::If(i) => {
            out.push(PyNode::Expr(&i.test));
            out.push(PyNode::Expr(&i.body));
            out.push(PyNode::Expr(&i.orelse));
        }
        Expr::Dict(d) => {
            out.extend(d.items.iter().filter_map(|i| i.key.as_ref()).map(PyNode::Expr));
            out.extend(d.items.iter().map(|i| PyNode::Expr(&i.value)));
        }
        Expr::Set(s) => push_exprs(out, &s.elts),
        Expr::ListComp(c) => comp_children(&c.elt, &c.generators, out),
        Expr::SetComp(c) => comp_children(&c.elt, &c.generators, out),
        Expr::Generator(c) => comp_children(&c.elt, &c.generators, out),
        Expr::DictComp(c) => {
            push_expr(out, c.key.as_deref());
            out.push(PyNode::Expr(&c.value));
            out.extend(c.generators.iter().map(PyNode::Comprehension));
        }
        Expr::Await(a) => out.push(PyNode::Expr(&a.value)),
        Expr::Yield(y) => push_expr(out, y.value.as_deref()),
        Expr::YieldFrom(y) => out.push(PyNode::Expr(&y.value)),
        Expr::Compare(c) => push_exprs(out, &c.operands),
        Expr::Call(c) => {
            out.push(PyNode::Expr(&c.func));
            push_exprs(out, &c.arguments.args);
            out.extend(c.arguments.keywords.iter().map(PyNode::Keyword));
        }
        Expr::FString(f) => {
            for part in &f.value {
                if let ast::FStringPartRef::FString(fs) = part {
                    push_interpolations(out, &fs.elements);
                }
            }
        }
        Expr::TString(t) => {
            for ts in &t.value {
                push_interpolations(out, &ts.elements);
            }
        }
        Expr::Attribute(a) => out.push(PyNode::Expr(&a.value)),
        Expr::Subscript(s) => {
            out.push(PyNode::Expr(&s.value));
            out.push(PyNode::Expr(&s.slice));
        }
        Expr::Starred(s) => out.push(PyNode::Expr(&s.value)),
        Expr::List(l) => push_exprs(out, &l.elts),
        Expr::Tuple(t) => push_exprs(out, &t.elts),
        Expr::Slice(s) => {
            push_expr(out, s.lower.as_deref());
            push_expr(out, s.upper.as_deref());
            push_expr(out, s.step.as_deref());
        }
        _ => {}
    }
}

fn comp_children<'a>(elt: &'a Expr, gens: &'a [ast::Comprehension], out: &mut Vec<PyNode<'a>>) {
    out.push(PyNode::Expr(elt));
    out.extend(gens.iter().map(PyNode::Comprehension));
}

fn arguments_children<'a>(p: &'a Parameters, out: &mut Vec<PyNode<'a>>) {
    out.extend(p.posonlyargs.iter().map(|x| PyNode::Arg(&x.parameter)));
    out.extend(p.args.iter().map(|x| PyNode::Arg(&x.parameter)));
    if let Some(v) = p.vararg.as_deref() {
        out.push(PyNode::Arg(v));
    }
    out.extend(p.kwonlyargs.iter().map(|x| PyNode::Arg(&x.parameter)));
    out.extend(p.kwonlyargs.iter().filter_map(|x| x.default.as_deref()).map(PyNode::Expr));
    if let Some(k) = p.kwarg.as_deref() {
        out.push(PyNode::Arg(k));
    }
    out.extend(p.posonlyargs.iter().chain(p.args.iter()).filter_map(|x| x.default.as_deref()).map(PyNode::Expr));
}

fn pattern_children<'a>(p: &'a Pattern, out: &mut Vec<PyNode<'a>>) {
    match p {
        Pattern::MatchValue(v) => out.push(PyNode::Expr(&v.value)),
        Pattern::MatchSequence(v) => out.extend(v.patterns.iter().map(PyNode::Pattern)),
        Pattern::MatchMapping(v) => {
            push_exprs(out, &v.keys);
            out.extend(v.patterns.iter().map(PyNode::Pattern));
        }
        Pattern::MatchClass(v) => {
            out.push(PyNode::Expr(&v.cls));
            out.extend(v.arguments.patterns.iter().map(PyNode::Pattern));
            out.extend(v.arguments.keywords.iter().map(|k| PyNode::Pattern(&k.pattern)));
        }
        Pattern::MatchAs(v) => {
            if let Some(p) = v.pattern.as_deref() {
                out.push(PyNode::Pattern(p));
            }
        }
        Pattern::MatchOr(v) => out.extend(v.patterns.iter().map(PyNode::Pattern)),
        Pattern::MatchSingleton(_) | Pattern::MatchStar(_) => {}
    }
}

impl<'a> PyNode<'a> {
    /// Children in CPython `_fields` order.
    pub fn children(self) -> Vec<PyNode<'a>> {
        let mut out = Vec::new();
        match self {
            PyNode::Module(m) => push_stmts(&mut out, &m.body),
            PyNode::Stmt(s) => stmt_children(s, &mut out),
            PyNode::Elif(rest) => {
                let clause = &rest[0];
                push_expr(&mut out, clause.test.as_ref());
                push_stmts(&mut out, &clause.body);
                push_orelse(&mut out, &rest[1..]);
            }
            PyNode::Expr(e) => expr_children(e, &mut out),
            PyNode::Handler(h) => {
                push_expr(&mut out, h.type_.as_deref());
                push_stmts(&mut out, &h.body);
            }
            PyNode::Arguments(p) => arguments_children(p, &mut out),
            PyNode::Arg(a) => push_expr(&mut out, a.annotation.as_deref()),
            PyNode::Keyword(k) => out.push(PyNode::Expr(&k.value)),
            PyNode::Alias(_) => {}
            PyNode::WithItem(w) => {
                out.push(PyNode::Expr(&w.context_expr));
                push_expr(&mut out, w.optional_vars.as_deref());
            }
            PyNode::MatchCase(c) => {
                out.push(PyNode::Pattern(&c.pattern));
                push_expr(&mut out, c.guard.as_deref());
                push_stmts(&mut out, &c.body);
            }
            PyNode::Pattern(p) => pattern_children(p, &mut out),
            PyNode::Comprehension(c) => {
                out.push(PyNode::Expr(&c.target));
                out.push(PyNode::Expr(&c.iter));
                push_exprs(&mut out, &c.ifs);
            }
            PyNode::TypeParam(t) => match t {
                TypeParam::TypeVar(v) => {
                    push_expr(&mut out, v.bound.as_deref());
                    push_expr(&mut out, v.default.as_deref());
                }
                TypeParam::ParamSpec(v) => push_expr(&mut out, v.default.as_deref()),
                TypeParam::TypeVarTuple(v) => push_expr(&mut out, v.default.as_deref()),
            },
            PyNode::Interpolation(i) => {
                out.push(PyNode::Expr(&i.expression));
                if let Some(spec) = i.format_spec.as_deref() {
                    out.push(PyNode::FormatSpec(spec));
                }
            }
            PyNode::FormatSpec(spec) => push_interpolations(&mut out, &spec.elements),
        }
        out
    }
}

/// `ast.walk` order from `root`: breadth-first, children in field order.
pub fn bfs(root: PyNode<'_>) -> Vec<PyNode<'_>> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([root]);
    while let Some(node) = queue.pop_front() {
        queue.extend(node.children());
        out.push(node);
    }
    out
}

/// CPython `(lineno, col_offset)` computation.
pub struct Positions<'a> {
    index: &'a LineIndex<'a>,
    tokens: &'a Tokens,
    /// For each `(` token index, the end offset of its matching `)`.
    close_of: Vec<Option<TextSize>>,
}

impl<'a> Positions<'a> {
    pub fn new(index: &'a LineIndex<'a>, tokens: &'a Tokens) -> Self {
        let mut close_of = vec![None; tokens.len()];
        let mut stack = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            match t.kind() {
                TokenKind::Lpar => stack.push(i),
                TokenKind::Rpar => {
                    if let Some(open) = stack.pop() {
                        close_of[open] = Some(t.end());
                    }
                }
                _ => {}
            }
        }
        Positions { index, tokens, close_of }
    }

    /// Extend `start` back over `(` tokens that close inside `[start, end)`:
    /// CPython starts a node at the parenthesis of a parenthesized leading child.
    fn extend_parens(&self, start: TextSize, end: TextSize) -> TextSize {
        let mut i = self.tokens.partition_point(|t| t.start() < start);
        let mut start = start;
        while i > 0 {
            let prev = &self.tokens[i - 1];
            if prev.kind() != TokenKind::Lpar || self.close_of[i - 1].is_none_or(|c| c > end) {
                break;
            }
            start = prev.start();
            i -= 1;
        }
        start
    }

    fn keyword_after(&self, decorators: &[ast::Decorator], start: TextSize) -> TextSize {
        let after = decorators.last().map_or(start, Ranged::end);
        let from = self.tokens.partition_point(|t| t.start() < after);
        self.tokens[from..]
            .iter()
            .find(|t| matches!(t.kind(), TokenKind::Def | TokenKind::Async | TokenKind::Class))
            .map_or(start, Ranged::start)
    }

    /// Offset CPython reports as the node's start; `None` for nodes without
    /// positions (arguments, comprehension, withitem, match_case, patterns
    /// are positioned in CPython but never read here).
    pub fn start(&self, node: PyNode, call_paren: Option<TextSize>) -> Option<TextSize> {
        Some(match node {
            PyNode::Stmt(Stmt::FunctionDef(f)) => self.keyword_after(&f.decorator_list, f.start()),
            PyNode::Stmt(Stmt::ClassDef(c)) => self.keyword_after(&c.decorator_list, c.start()),
            PyNode::Stmt(s) => self.extend_parens(s.start(), s.end()),
            PyNode::Elif(rest) => rest[0].start(),
            PyNode::Expr(Expr::Generator(g)) if !g.parenthesized => call_paren.unwrap_or(g.start()),
            PyNode::Expr(e) => self.extend_parens(e.start(), e.end()),
            PyNode::Handler(h) => h.start(),
            PyNode::Arg(a) => a.name.start(),
            PyNode::Keyword(k) => k.start(),
            PyNode::Alias(a) => a.start(),
            _ => return None,
        })
    }

    /// `(lineno, col_offset)` in CPython units (1-based line, UTF-8 bytes).
    pub fn lineno_col(&self, node: PyNode, call_paren: Option<TextSize>) -> Option<(u32, usize)> {
        self.start(node, call_paren).map(|o| self.index.ast_pos(o))
    }
}

/// For a call whose sole argument is an unparenthesized generator, the
/// offset of the call's `(`, which CPython uses as the generator's position.
pub fn generator_parens<'a>(nodes: &[PyNode<'a>]) -> Vec<(*const Expr, TextSize)> {
    let mut out = Vec::new();
    for n in nodes {
        if let PyNode::Expr(Expr::Call(c)) = n
            && let ([arg @ Expr::Generator(g)], true) = (&*c.arguments.args, c.arguments.keywords.is_empty())
            && !g.parenthesized
        {
            out.push((std::ptr::from_ref(arg), c.arguments.start()));
        }
    }
    out
}

/// CPython class name for a node, for diagnostics and the walk check.
pub fn kind_name(node: PyNode) -> &'static str {
    match node {
        PyNode::Module(_) => "Module",
        PyNode::Stmt(s) => match s {
            Stmt::FunctionDef(f) if f.is_async => "AsyncFunctionDef",
            Stmt::FunctionDef(_) => "FunctionDef",
            Stmt::ClassDef(_) => "ClassDef",
            Stmt::Return(_) => "Return",
            Stmt::Delete(_) => "Delete",
            Stmt::TypeAlias(_) => "TypeAlias",
            Stmt::Assign(_) => "Assign",
            Stmt::AugAssign(_) => "AugAssign",
            Stmt::AnnAssign(_) => "AnnAssign",
            Stmt::For(f) if f.is_async => "AsyncFor",
            Stmt::For(_) => "For",
            Stmt::While(_) => "While",
            Stmt::If(_) => "If",
            Stmt::With(w) if w.is_async => "AsyncWith",
            Stmt::With(_) => "With",
            Stmt::Match(_) => "Match",
            Stmt::Raise(_) => "Raise",
            Stmt::Try(t) if t.is_star => "TryStar",
            Stmt::Try(_) => "Try",
            Stmt::Assert(_) => "Assert",
            Stmt::Import(_) => "Import",
            Stmt::ImportFrom(_) => "ImportFrom",
            Stmt::Global(_) => "Global",
            Stmt::Nonlocal(_) => "Nonlocal",
            Stmt::Expr(_) => "Expr",
            Stmt::Pass(_) => "Pass",
            Stmt::Break(_) => "Break",
            Stmt::Continue(_) => "Continue",
            Stmt::IpyEscapeCommand(_) => "IpyEscape",
        },
        PyNode::Elif(_) => "If",
        PyNode::Expr(e) => expr_kind_name(e),
        PyNode::Handler(_) => "ExceptHandler",
        PyNode::Arguments(_) => "arguments",
        PyNode::Arg(_) => "arg",
        PyNode::Keyword(_) => "keyword",
        PyNode::Alias(_) => "alias",
        PyNode::WithItem(_) => "withitem",
        PyNode::MatchCase(_) => "match_case",
        PyNode::Pattern(_) => "pattern",
        PyNode::Comprehension(_) => "comprehension",
        PyNode::TypeParam(_) => "type_param",
        PyNode::Interpolation(_) => "FormattedValue",
        PyNode::FormatSpec(_) => "JoinedStr",
    }
}

fn expr_kind_name(e: &Expr) -> &'static str {
    match e {
        Expr::BoolOp(_) => "BoolOp",
        Expr::Named(_) => "NamedExpr",
        Expr::BinOp(_) => "BinOp",
        Expr::UnaryOp(_) => "UnaryOp",
        Expr::Lambda(_) => "Lambda",
        Expr::If(_) => "IfExp",
        Expr::Dict(_) => "Dict",
        Expr::Set(_) => "Set",
        Expr::ListComp(_) => "ListComp",
        Expr::SetComp(_) => "SetComp",
        Expr::DictComp(_) => "DictComp",
        Expr::Generator(_) => "GeneratorExp",
        Expr::Await(_) => "Await",
        Expr::Yield(_) => "Yield",
        Expr::YieldFrom(_) => "YieldFrom",
        Expr::Compare(_) => "Compare",
        Expr::Call(_) => "Call",
        Expr::FString(_) => "JoinedStr",
        Expr::TString(_) => "TemplateStr",
        Expr::Attribute(_) => "Attribute",
        Expr::Subscript(_) => "Subscript",
        Expr::Starred(_) => "Starred",
        Expr::Name(_) => "Name",
        Expr::List(_) => "List",
        Expr::Tuple(_) => "Tuple",
        Expr::Slice(_) => "Slice",
        _ => "Constant",
    }
}
