//! One walk over the ruff AST gathering what the v0.5 Python probe reads
//! from CPython's `ast`: function definitions, docstring spans, overload
//! stubs, and branch events for cyclomatic complexity.

use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{
    Comprehension, Decorator, ExceptHandler, Expr, ExprCall, MatchCase, ModModule, Stmt,
    StmtFunctionDef,
};
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::lines::LineIndex;

pub struct FunctionInfo<'a> {
    pub node: &'a StmtFunctionDef,
    pub name: String,
    pub qname: String,
    /// Line of `def` (or `async`), as `ast` reports `lineno`.
    pub def_line: u32,
    /// First line including decorators.
    pub start_line: u32,
    /// `end_lineno`: last line of the last body statement.
    pub end_line: u32,
    /// Depth in CPython's AST (elif, except and case each add a level):
    /// with `offset` it reproduces `ast.walk` breadth-first order.
    pub depth: u32,
    pub offset: u32,
}

/// Token-exclusion span: `((line, byte col), (line, byte col))`, compared
/// against token positions the same way the oracle does.
pub type Span = ((u32, usize), (u32, usize));

#[derive(Default)]
pub struct Collected<'a> {
    pub functions: Vec<FunctionInfo<'a>>,
    pub excluded: Vec<Span>,
    /// `(line, increment)` complexity events.
    pub events: Vec<(u32, u32)>,
}

struct Collector<'a> {
    index: &'a LineIndex<'a>,
    tokens: &'a Tokens,
    scope: Vec<String>,
    depth: u32,
    out: Collected<'a>,
    /// Generator expressions already attributed to their call's parenthesis.
    counted_generators: Vec<TextRange>,
}

pub fn collect<'a>(
    module: &'a ModModule,
    tokens: &'a Tokens,
    index: &'a LineIndex,
) -> Collected<'a> {
    let mut c = Collector {
        index,
        tokens,
        scope: Vec::new(),
        depth: 0,
        out: Collected::default(),
        counted_generators: Vec::new(),
    };
    c.docstring(&module.body);
    c.visit_body(&module.body);
    c.out
}

fn is_overload(d: &Decorator) -> bool {
    match &d.expression {
        Expr::Name(n) => n.id.as_str() == "overload",
        Expr::Attribute(a) => a.attr.as_str() == "overload",
        _ => false,
    }
}

/// Offset of a definition's keyword token (`def`, `async` or `class`) after
/// its decorators, as CPython's `lineno` reports it.
fn keyword_offset(decorators: &[Decorator], start: TextSize, tokens: &Tokens) -> TextSize {
    let after = decorators.last().map_or(start, Ranged::end);
    let from = tokens.partition_point(|t| t.start() < after);
    tokens[from..]
        .iter()
        .find(|t| {
            matches!(
                t.kind(),
                TokenKind::Def | TokenKind::Async | TokenKind::Class
            )
        })
        .map_or(start, Ranged::start)
}

/// CPython `lineno` of a function definition (the `def`/`async` line).
pub fn function_line(f: &StmtFunctionDef, tokens: &Tokens, index: &LineIndex) -> u32 {
    index.line(keyword_offset(&f.decorator_list, f.start(), tokens))
}

/// CPython's `lineno` for a statement: decorated definitions report the
/// `def`/`class` keyword line, everything else its first token.
pub fn stmt_line(stmt: &Stmt, tokens: &Tokens, index: &LineIndex) -> u32 {
    match stmt {
        Stmt::FunctionDef(f) => function_line(f, tokens, index),
        Stmt::ClassDef(c) => index.line(keyword_offset(&c.decorator_list, c.start(), tokens)),
        _ => index.line(stmt.start()),
    }
}

impl<'a> Collector<'a> {
    fn line(&self, offset: TextSize) -> u32 {
        self.index.line(offset)
    }

    fn event(&mut self, offset: TextSize, inc: u32) {
        if inc > 0 {
            let line = self.line(offset);
            self.out.events.push((line, inc));
        }
    }

    /// A leading string-literal expression statement is a docstring.
    fn docstring(&mut self, body: &[Stmt]) {
        if let Some(Stmt::Expr(stmt)) = body.first()
            && matches!(*stmt.value, Expr::StringLiteral(_))
        {
            let span = (
                self.index.ast_pos(stmt.start()),
                self.index.ast_pos(stmt.end()),
            );
            self.out.excluded.push(span);
        }
    }

    /// Record a definition; returns its name for the enclosing scope.
    fn function(&mut self, f: &'a StmtFunctionDef) -> String {
        let def_line = function_line(f, self.tokens, self.index);
        let start_line = f
            .decorator_list
            .iter()
            .map(|d| self.line(d.start()))
            .fold(def_line, u32::min);
        let end_line = f
            .body
            .last()
            .map_or(def_line, |s| self.index.end_line(s.end()));
        if f.decorator_list.iter().any(is_overload) {
            // Overload stubs are declarations, not executable source.
            self.out.excluded.push(((start_line, 0), (end_line + 1, 0)));
        }
        let name = f.name.as_str().to_owned();
        let qname = if self.scope.is_empty() {
            name.clone()
        } else {
            format!("{}.{name}", self.scope.join("."))
        };
        self.out.functions.push(FunctionInfo {
            node: f,
            name: name.clone(),
            qname,
            def_line,
            start_line,
            end_line,
            depth: self.depth,
            offset: f.start().to_u32(),
        });
        self.docstring(&f.body);
        name
    }

    fn comprehension_cost(generators: &[Comprehension]) -> u32 {
        generators
            .iter()
            .map(|g| 1 + u32::try_from(g.ifs.len()).unwrap_or(u32::MAX))
            .sum()
    }

    /// CPython places a call's sole unparenthesized generator argument at
    /// the call's opening parenthesis; attribute its cost there.
    fn call_generator(&mut self, call: &ExprCall) {
        let args = &call.arguments;
        if let ([Expr::Generator(g)], true) = (&*args.args, args.keywords.is_empty())
            && !g.parenthesized
        {
            self.event(args.start(), Self::comprehension_cost(&g.generators));
            self.counted_generators.push(g.range());
        }
    }
}

impl<'a> Visitor<'a> for Collector<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        self.depth += 1;
        match stmt {
            Stmt::FunctionDef(f) => {
                let name = self.function(f);
                self.scope.push(name);
                visitor::walk_stmt(self, stmt);
                self.scope.pop();
                self.depth -= 1;
                return;
            }
            Stmt::ClassDef(c) => {
                self.docstring(&c.body);
                self.scope.push(c.name.as_str().to_owned());
                visitor::walk_stmt(self, stmt);
                self.scope.pop();
                self.depth -= 1;
                return;
            }
            Stmt::If(s) => {
                self.event(s.start(), 1);
                self.visit_expr(&s.test);
                self.visit_body(&s.body);
                // CPython nests each `elif` as an If in the previous orelse,
                // so every elif adds one AST level; `else` reuses the last.
                let mut elifs = 0;
                for clause in &s.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.event(clause.start(), 1);
                        elifs += 1;
                        self.depth += elifs;
                        self.visit_expr(test);
                    } else {
                        self.depth += elifs;
                    }
                    self.visit_body(&clause.body);
                    self.depth -= elifs;
                }
                self.depth -= 1;
                return;
            }
            Stmt::For(_) | Stmt::While(_) | Stmt::Assert(_) => self.event(stmt.start(), 1),
            Stmt::Match(m) => {
                let cases = u32::try_from(m.cases.len()).unwrap_or(u32::MAX);
                self.event(m.start(), cases.saturating_sub(1));
            }
            _ => {}
        }
        visitor::walk_stmt(self, stmt);
        self.depth -= 1;
    }

    fn visit_except_handler(&mut self, handler: &'a ExceptHandler) {
        self.event(handler.start(), 1);
        self.depth += 1;
        visitor::walk_except_handler(self, handler);
        self.depth -= 1;
    }

    fn visit_match_case(&mut self, case: &'a MatchCase) {
        self.depth += 1;
        visitor::walk_match_case(self, case);
        self.depth -= 1;
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::BoolOp(b) => {
                let n = u32::try_from(b.values.len()).unwrap_or(u32::MAX);
                self.event(b.start(), n.saturating_sub(1));
            }
            Expr::If(e) => self.event(e.start(), 1),
            Expr::ListComp(e) => self.event(e.start(), Self::comprehension_cost(&e.generators)),
            Expr::SetComp(e) => self.event(e.start(), Self::comprehension_cost(&e.generators)),
            Expr::DictComp(e) => self.event(e.start(), Self::comprehension_cost(&e.generators)),
            Expr::Generator(e) => {
                if !self.counted_generators.contains(&e.range()) {
                    self.event(e.start(), Self::comprehension_cost(&e.generators));
                }
            }
            Expr::Call(call) => self.call_generator(call),
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }
}
