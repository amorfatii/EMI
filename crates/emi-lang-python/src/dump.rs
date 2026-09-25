//! Structural fingerprints of statement lists with the equivalence classes of
//! CPython's `ast.dump(..., include_attributes=False)`, computed from ruff's
//! AST. The oracle groups clones by that string; any injective encoding of
//! the CPython tree shape groups identically, so this emits one and hashes
//! it. Shape differences between ruff and CPython are undone here: `elif`
//! chains become nested `If`s, implicit string concatenations become one
//! constant, and f-strings become `JoinedStr` values with merged literals.
//!
//! `normalized` mirrors the oracle's `_CloneNormalizer`: names, constants,
//! attribute names, parameter names, keyword names, import aliases and
//! def/class names are blanked; everything else stays exact.

use std::fmt::Write as _;

use ruff_python_ast::{
    self as ast, Arguments, Comprehension, ElifElseClause, ExceptHandler, Expr,
    InterpolatedStringElement, InterpolatedStringElements, Number, Parameters, Pattern, Stmt,
    TypeParam,
};

pub fn fingerprint(body: &[Stmt], normalized: bool) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut d = Dumper {
        out: String::new(),
        norm: normalized,
    };
    d.stmts(body);
    Sha256::digest(d.out.as_bytes()).into()
}

struct Dumper {
    out: String,
    norm: bool,
}

/// One text piece of a JoinedStr/TemplateStr under construction.
enum Piece<'a> {
    Text(String),
    Value(&'a ast::InterpolatedElement),
}

impl Dumper {
    fn tag(&mut self, t: &str) {
        self.out.push_str(t);
        self.out.push('(');
    }

    fn end(&mut self) {
        self.out.push(')');
    }

    fn ident(&mut self, s: &str) {
        let _ = write!(self.out, "{}:{s};", s.len());
    }

    fn name(&mut self, s: &str, blank: bool) {
        self.ident(if blank && self.norm { "_" } else { s });
    }

    fn opt_name(&mut self, s: Option<&str>, blank: bool) {
        match s {
            Some(s) => self.name(s, blank),
            None => self.none(),
        }
    }

    fn none(&mut self) {
        self.out.push('~');
    }

    fn int(&mut self, v: i64) {
        let _ = write!(self.out, "#{v};");
    }

    fn list<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) {
        let _ = write!(self.out, "[{};", items.len());
        for item in items {
            f(self, item);
        }
        self.out.push(']');
    }

    fn stmts(&mut self, body: &[Stmt]) {
        self.list(body, Self::stmt);
    }

    fn exprs(&mut self, items: &[Expr]) {
        self.list(items, Self::expr);
    }

    fn opt_expr(&mut self, e: Option<&Expr>) {
        match e {
            Some(e) => self.expr(e),
            None => self.none(),
        }
    }

    fn debug<T: std::fmt::Debug>(&mut self, v: &T) {
        let _ = write!(self.out, "{v:?};");
    }

    // ---- statements ----------------------------------------------------

    /// Emit `tag(...)` with `body` writing the fields.
    fn node(&mut self, tag: &str, body: impl FnOnce(&mut Self)) {
        self.tag(tag);
        body(self);
        self.end();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::FunctionDef(f) => self.function(f),
            Stmt::ClassDef(c) => self.class(c),
            Stmt::If(i) => self.if_chain(&i.test, &i.body, &i.elif_else_clauses),
            Stmt::For(f) => self.node(if f.is_async { "AsyncFor" } else { "For" }, |d| {
                d.expr(&f.target);
                d.expr(&f.iter);
                d.stmts(&f.body);
                d.stmts(&f.orelse);
            }),
            Stmt::While(w) => self.node("While", |d| {
                d.expr(&w.test);
                d.stmts(&w.body);
                d.stmts(&w.orelse);
            }),
            Stmt::With(w) => self.node(if w.is_async { "AsyncWith" } else { "With" }, |d| {
                d.list(&w.items, |d, item| {
                    d.node("withitem", |d| {
                        d.expr(&item.context_expr);
                        d.opt_expr(item.optional_vars.as_deref());
                    });
                });
                d.stmts(&w.body);
            }),
            Stmt::Match(m) => self.node("Match", |d| {
                d.expr(&m.subject);
                d.list(&m.cases, |d, case| {
                    d.node("match_case", |d| {
                        d.pattern(&case.pattern);
                        d.opt_expr(case.guard.as_deref());
                        d.stmts(&case.body);
                    });
                });
            }),
            Stmt::Try(t) => self.try_stmt(t),
            _ => self.simple_stmt(s),
        }
    }

    fn simple_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Return(r) => self.unary("Return", r.value.as_deref()),
            Stmt::Delete(x) => self.node("Delete", |d| d.exprs(&x.targets)),
            Stmt::TypeAlias(x) => self.node("TypeAlias", |d| {
                d.expr(&x.name);
                d.type_params(x.type_params.as_deref());
                d.expr(&x.value);
            }),
            Stmt::Assign(a) => self.node("Assign", |d| {
                d.exprs(&a.targets);
                d.expr(&a.value);
            }),
            Stmt::AugAssign(a) => self.node("AugAssign", |d| {
                d.expr(&a.target);
                d.debug(&a.op);
                d.expr(&a.value);
            }),
            Stmt::AnnAssign(a) => self.node("AnnAssign", |d| {
                d.expr(&a.target);
                d.expr(&a.annotation);
                d.opt_expr(a.value.as_deref());
                d.int(i64::from(a.simple));
            }),
            Stmt::Raise(r) => self.node("Raise", |d| {
                d.opt_expr(r.exc.as_deref());
                d.opt_expr(r.cause.as_deref());
            }),
            Stmt::Assert(a) => self.node("Assert", |d| {
                d.expr(&a.test);
                d.opt_expr(a.msg.as_deref());
            }),
            Stmt::Import(i) => self.node("Import", |d| d.list(&i.names, Self::alias)),
            Stmt::ImportFrom(i) => self.node("ImportFrom", |d| {
                d.opt_name(i.module.as_ref().map(ast::Identifier::as_str), false);
                d.list(&i.names, Self::alias);
                d.int(i64::from(i.level));
            }),
            Stmt::Global(g) => self.node("Global", |d| {
                d.list(&g.names, |d, n| d.name(n.as_str(), false))
            }),
            Stmt::Nonlocal(g) => self.node("Nonlocal", |d| {
                d.list(&g.names, |d, n| d.name(n.as_str(), false))
            }),
            Stmt::Expr(e) => self.unary("Expr", Some(&e.value)),
            Stmt::Pass(_) => self.node("Pass", |_| {}),
            Stmt::Break(_) => self.node("Break", |_| {}),
            Stmt::Continue(_) => self.node("Continue", |_| {}),
            Stmt::IpyEscapeCommand(c) => self.node("IpyEscape", |d| d.ident(&c.value)),
            _ => {}
        }
    }

    fn class(&mut self, c: &ast::StmtClassDef) {
        self.node("ClassDef", |d| {
            d.name(c.name.as_str(), true);
            let (bases, keywords) = c
                .arguments
                .as_deref()
                .map_or((&[][..], &[][..]), |a| (&a.args[..], &a.keywords[..]));
            d.exprs(bases);
            d.list(keywords, Self::keyword);
            d.stmts(&c.body);
            d.list(&c.decorator_list, |d, x| d.expr(&x.expression));
            d.type_params(c.type_params.as_deref());
        });
    }

    fn try_stmt(&mut self, t: &ast::StmtTry) {
        self.node(if t.is_star { "TryStar" } else { "Try" }, |d| {
            d.stmts(&t.body);
            d.list(&t.handlers, |d, h| {
                let ExceptHandler::ExceptHandler(h) = h;
                d.node("ExceptHandler", |d| {
                    d.opt_expr(h.type_.as_deref());
                    d.opt_name(h.name.as_ref().map(ast::Identifier::as_str), false);
                    d.stmts(&h.body);
                });
            });
            d.stmts(&t.orelse);
            d.stmts(&t.finalbody);
        });
    }

    fn unary(&mut self, tag: &str, value: Option<&Expr>) {
        self.tag(tag);
        self.opt_expr(value);
        self.end();
    }

    fn function(&mut self, f: &ast::StmtFunctionDef) {
        self.tag(if f.is_async {
            "AsyncFunctionDef"
        } else {
            "FunctionDef"
        });
        self.name(f.name.as_str(), true);
        self.arguments(Some(&f.parameters));
        self.stmts(&f.body);
        self.list(&f.decorator_list, |d, x| d.expr(&x.expression));
        self.opt_expr(f.returns.as_deref());
        self.type_params(f.type_params.as_deref());
        self.end();
    }

    /// CPython nests `elif` as an If inside the previous orelse.
    fn if_chain(&mut self, test: &Expr, body: &[Stmt], clauses: &[ElifElseClause]) {
        self.tag("If");
        self.expr(test);
        self.stmts(body);
        match clauses.split_first() {
            None => self.stmts(&[]),
            Some((first, rest)) => match &first.test {
                Some(t) => {
                    self.out.push_str("[1;");
                    self.if_chain(t, &first.body, rest);
                    self.out.push(']');
                }
                None => self.stmts(&first.body),
            },
        }
        self.end();
    }

    fn alias(&mut self, a: &ast::Alias) {
        self.tag("alias");
        self.name(a.name.as_str(), true);
        self.opt_name(a.asname.as_ref().map(ast::Identifier::as_str), true);
        self.end();
    }

    fn keyword(&mut self, k: &ast::Keyword) {
        self.tag("keyword");
        self.opt_name(k.arg.as_ref().map(ast::Identifier::as_str), true);
        self.expr(&k.value);
        self.end();
    }

    fn arg(&mut self, p: &ast::Parameter) {
        self.tag("arg");
        self.name(p.name.as_str(), true);
        self.opt_expr(p.annotation.as_deref());
        self.end();
    }

    fn opt_arg(&mut self, p: Option<&ast::Parameter>) {
        match p {
            Some(p) => self.arg(p),
            None => self.none(),
        }
    }

    /// CPython `arguments`: defaults of positional parameters are one list
    /// aligned to the end; keyword-only defaults are a per-parameter list.
    fn arguments(&mut self, params: Option<&Parameters>) {
        self.tag("arguments");
        let Some(p) = params else {
            self.out.push_str("[0;][0;]~[0;][0;]~[0;])");
            return;
        };
        self.list(&p.posonlyargs, |d, x| d.arg(&x.parameter));
        self.list(&p.args, |d, x| d.arg(&x.parameter));
        self.opt_arg(p.vararg.as_deref());
        self.list(&p.kwonlyargs, |d, x| d.arg(&x.parameter));
        self.list(&p.kwonlyargs, |d, x| d.opt_expr(x.default.as_deref()));
        self.opt_arg(p.kwarg.as_deref());
        let defaults: Vec<&Expr> = p
            .posonlyargs
            .iter()
            .chain(p.args.iter())
            .filter_map(|x| x.default.as_deref())
            .collect();
        self.list(&defaults, |d, e| d.expr(e));
        self.end();
    }

    fn type_params(&mut self, tp: Option<&ast::TypeParams>) {
        let params: &[TypeParam] = tp.map_or(&[], |t| &t.type_params);
        self.list(params, |d, p| match p {
            TypeParam::TypeVar(v) => {
                d.tag("TypeVar");
                d.name(v.name.as_str(), false);
                d.opt_expr(v.bound.as_deref());
                d.opt_expr(v.default.as_deref());
                d.end();
            }
            TypeParam::ParamSpec(v) => {
                d.tag("ParamSpec");
                d.name(v.name.as_str(), false);
                d.opt_expr(v.default.as_deref());
                d.end();
            }
            TypeParam::TypeVarTuple(v) => {
                d.tag("TypeVarTuple");
                d.name(v.name.as_str(), false);
                d.opt_expr(v.default.as_deref());
                d.end();
            }
        });
    }

    fn pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::MatchValue(v) => self.unary("MatchValue", Some(&v.value)),
            Pattern::MatchSingleton(v) => {
                self.tag("MatchSingleton");
                self.debug(&v.value);
                self.end();
            }
            Pattern::MatchSequence(v) => {
                self.tag("MatchSequence");
                self.list(&v.patterns, Self::pattern);
                self.end();
            }
            Pattern::MatchMapping(v) => {
                self.tag("MatchMapping");
                self.exprs(&v.keys);
                self.list(&v.patterns, Self::pattern);
                self.opt_name(v.rest.as_ref().map(ast::Identifier::as_str), false);
                self.end();
            }
            Pattern::MatchClass(v) => {
                self.tag("MatchClass");
                self.expr(&v.cls);
                self.list(&v.arguments.patterns, Self::pattern);
                self.list(&v.arguments.keywords, |d, k| d.name(k.attr.as_str(), false));
                self.list(&v.arguments.keywords, |d, k| d.pattern(&k.pattern));
                self.end();
            }
            Pattern::MatchStar(v) => {
                self.tag("MatchStar");
                self.opt_name(v.name.as_ref().map(ast::Identifier::as_str), false);
                self.end();
            }
            Pattern::MatchAs(v) => {
                self.tag("MatchAs");
                match v.pattern.as_deref() {
                    Some(p) => self.pattern(p),
                    None => self.none(),
                }
                self.opt_name(v.name.as_ref().map(ast::Identifier::as_str), false);
                self.end();
            }
            Pattern::MatchOr(v) => {
                self.tag("MatchOr");
                self.list(&v.patterns, Self::pattern);
                self.end();
            }
        }
    }

    // ---- expressions ---------------------------------------------------

    fn ctx(&mut self, c: ast::ExprContext) {
        self.debug(&c);
    }

    fn comprehensions(&mut self, gens: &[Comprehension]) {
        self.list(gens, |d, g| {
            d.tag("comprehension");
            d.expr(&g.target);
            d.expr(&g.iter);
            d.exprs(&g.ifs);
            d.int(i64::from(g.is_async));
            d.end();
        });
    }

    fn call_arguments(&mut self, a: &Arguments) {
        self.exprs(&a.args);
        self.list(&a.keywords, Self::keyword);
    }

    /// A constant, or `Constant(0)` when normalizing.
    fn constant(&mut self, write: impl FnOnce(&mut String)) {
        self.tag("Constant");
        if self.norm {
            self.out.push_str("I0;~");
        } else {
            write(&mut self.out);
        }
        self.end();
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::BoolOp(b) => self.node("BoolOp", |d| {
                d.debug(&b.op);
                d.exprs(&b.values);
            }),
            Expr::Named(n) => self.node("NamedExpr", |d| {
                d.expr(&n.target);
                d.expr(&n.value);
            }),
            Expr::BinOp(b) => self.node("BinOp", |d| {
                d.expr(&b.left);
                d.debug(&b.op);
                d.expr(&b.right);
            }),
            Expr::UnaryOp(u) => self.node("UnaryOp", |d| {
                d.debug(&u.op);
                d.expr(&u.operand);
            }),
            Expr::Lambda(l) => self.node("Lambda", |d| {
                d.arguments(l.parameters.as_deref());
                d.expr(&l.body);
            }),
            Expr::If(i) => self.node("IfExp", |d| {
                d.expr(&i.test);
                d.expr(&i.body);
                d.expr(&i.orelse);
            }),
            Expr::Dict(x) => self.node("Dict", |d| {
                d.list(&x.items, |d, item| d.opt_expr(item.key.as_ref()));
                d.list(&x.items, |d, item| d.expr(&item.value));
            }),
            Expr::Set(s) => self.node("Set", |d| d.exprs(&s.elts)),
            Expr::ListComp(c) => self.comp("ListComp", &c.elt, &c.generators),
            Expr::SetComp(c) => self.comp("SetComp", &c.elt, &c.generators),
            Expr::Generator(c) => self.comp("GeneratorExp", &c.elt, &c.generators),
            Expr::DictComp(c) => self.node("DictComp", |d| {
                d.opt_expr(c.key.as_deref());
                d.expr(&c.value);
                d.comprehensions(&c.generators);
            }),
            Expr::Await(a) => self.unary("Await", Some(&a.value)),
            Expr::Yield(y) => self.unary("Yield", y.value.as_deref()),
            Expr::YieldFrom(y) => self.unary("YieldFrom", Some(&y.value)),
            Expr::Compare(c) => self.node("Compare", |d| {
                // ruff keeps `left` as operands[0]; CPython splits it off.
                let (left, comparators) = c
                    .operands
                    .split_first()
                    .map_or((None, &[][..]), |(l, r)| (Some(l), r));
                d.opt_expr(left);
                d.list(&c.ops, |d, op| d.debug(op));
                d.exprs(comparators);
            }),
            Expr::Call(c) => self.node("Call", |d| {
                d.expr(&c.func);
                d.call_arguments(&c.arguments);
            }),
            Expr::FString(f) => self.fstring(f),
            Expr::TString(t) => self.tstring(t),
            _ => self.leaf_expr(e),
        }
    }

    /// Constants and reference expressions.
    fn leaf_expr(&mut self, e: &Expr) {
        match e {
            Expr::StringLiteral(s) => {
                let value = s.value.to_str().to_owned();
                let kind = if s.value.is_unicode() { "u" } else { "~" };
                self.constant(|out| {
                    let _ = write!(out, "S{}:{value};{kind}", value.len());
                });
            }
            Expr::BytesLiteral(b) => {
                let bytes: Vec<u8> = b.value.bytes().collect();
                self.constant(|out| {
                    let _ = write!(out, "B{bytes:?};~");
                });
            }
            Expr::NumberLiteral(n) => {
                let text = match &n.value {
                    Number::Int(i) => format!("I{i};~"),
                    Number::Float(f) => format!("F{:016x};~", f.to_bits()),
                    Number::Complex { real, imag } => {
                        format!("C{:016x}:{:016x};~", real.to_bits(), imag.to_bits())
                    }
                };
                self.constant(|out| out.push_str(&text));
            }
            Expr::BooleanLiteral(b) => {
                let text = if b.value { "T;~" } else { "F;~" };
                self.constant(|out| out.push_str(text));
            }
            Expr::NoneLiteral(_) => self.constant(|out| out.push_str("N;~")),
            Expr::EllipsisLiteral(_) => self.constant(|out| out.push_str("E;~")),
            Expr::Attribute(a) => self.node("Attribute", |d| {
                d.expr(&a.value);
                d.name(a.attr.as_str(), true);
                d.ctx(a.ctx);
            }),
            Expr::Subscript(s) => self.node("Subscript", |d| {
                d.expr(&s.value);
                d.expr(&s.slice);
                d.ctx(s.ctx);
            }),
            Expr::Starred(s) => self.node("Starred", |d| {
                d.expr(&s.value);
                d.ctx(s.ctx);
            }),
            Expr::Name(n) => self.node("Name", |d| {
                d.name(n.id.as_str(), true);
                d.ctx(n.ctx);
            }),
            Expr::List(l) => self.node("List", |d| {
                d.exprs(&l.elts);
                d.ctx(l.ctx);
            }),
            Expr::Tuple(x) => self.node("Tuple", |d| {
                d.exprs(&x.elts);
                d.ctx(x.ctx);
            }),
            Expr::Slice(s) => self.node("Slice", |d| {
                d.opt_expr(s.lower.as_deref());
                d.opt_expr(s.upper.as_deref());
                d.opt_expr(s.step.as_deref());
            }),
            Expr::IpyEscapeCommand(c) => self.node("IpyEscape", |d| d.ident(&c.value)),
            _ => {}
        }
    }

    fn comp(&mut self, tag: &str, elt: &Expr, gens: &[Comprehension]) {
        self.tag(tag);
        self.expr(elt);
        self.comprehensions(gens);
        self.end();
    }

    // ---- f-strings and t-strings ---------------------------------------

    fn push_text<'a>(pieces: &mut Vec<Piece<'a>>, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Piece::Text(last)) = pieces.last_mut() {
            last.push_str(text);
        } else {
            pieces.push(Piece::Text(text.to_owned()));
        }
    }

    fn push_elements<'a>(pieces: &mut Vec<Piece<'a>>, elements: &'a InterpolatedStringElements) {
        for el in elements {
            match el {
                InterpolatedStringElement::Literal(l) => Self::push_text(pieces, &l.value),
                InterpolatedStringElement::Interpolation(i) => {
                    if let Some(debug) = &i.debug_text {
                        let text = format!(
                            "{}{}{}",
                            debug.leading(),
                            debug.expression(),
                            debug.trailing()
                        );
                        Self::push_text(pieces, &text);
                    }
                    pieces.push(Piece::Value(i));
                }
            }
        }
    }

    fn fstring(&mut self, f: &ast::ExprFString) {
        let mut pieces = Vec::new();
        for part in &f.value {
            match part {
                ast::FStringPartRef::Literal(l) => Self::push_text(&mut pieces, &l.value),
                ast::FStringPartRef::FString(fs) => Self::push_elements(&mut pieces, &fs.elements),
            }
        }
        self.joined("JoinedStr", &pieces, false);
    }

    fn tstring(&mut self, t: &ast::ExprTString) {
        let mut pieces = Vec::new();
        for ts in &t.value {
            Self::push_elements(&mut pieces, &ts.elements);
        }
        self.joined("TemplateStr", &pieces, true);
    }

    fn joined(&mut self, tag: &str, pieces: &[Piece], template: bool) {
        self.tag(tag);
        self.list(pieces, |d, piece| match piece {
            Piece::Text(text) => d.constant(|out| {
                let _ = write!(out, "S{}:{text};~", text.len());
            }),
            Piece::Value(i) => d.interpolation(i, template),
        });
        self.end();
    }

    fn interpolation(&mut self, i: &ast::InterpolatedElement, template: bool) {
        self.tag(if template {
            "Interpolation"
        } else {
            "FormattedValue"
        });
        self.expr(&i.expression);
        if template {
            let text = i
                .debug_text
                .as_ref()
                .map_or_else(String::new, |d| d.expression().to_owned());
            self.ident(&text);
        }
        // `{x=}` without conversion or spec implies `!r`.
        let conversion = if i.debug_text.is_some()
            && i.conversion == ast::ConversionFlag::None
            && i.format_spec.is_none()
        {
            i64::from(b'r')
        } else {
            i64::from(i.conversion as i8)
        };
        self.int(conversion);
        match i.format_spec.as_deref() {
            Some(spec) => {
                let mut pieces = Vec::new();
                Self::push_elements(&mut pieces, &spec.elements);
                self.joined("JoinedStr", &pieces, false);
            }
            None => self.none(),
        }
        self.end();
    }
}
