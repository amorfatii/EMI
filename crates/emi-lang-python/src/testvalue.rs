//! Diagnostic test-value findings of `python_probe.test_value_findings`:
//! `empty-test` (a test body with nothing that can fail) and
//! `constant-assert` (an assert on a provably constant-true expression).
//!
//! The oracle evaluates constant expressions with Python's own semantics.
//! This evaluator covers numbers (ints up to i128), strings, bytes, tuples,
//! lists and sets, and treats as unknown what it cannot decide exactly
//! (identity beyond singletons and small ints, `%` formatting, oversized
//! integers). Unknown means "not flagged", matching the oracle's default.

use std::cmp::Ordering;

use ruff_python_ast::token::Tokens;
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{
    self as ast, BoolOp, CmpOp, Expr, Number, Operator, Stmt, StmtFunctionDef, UnaryOp,
};
use ruff_text_size::Ranged;

use emi_ir::Finding;

use crate::collect::function_line;
use crate::lines::LineIndex;

#[derive(Clone, Debug)]
enum Val {
    None,
    Ellipsis,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Tuple(Vec<Val>),
    List(Vec<Val>),
    Set(Vec<Val>),
}

use Val::{Bool, Bytes, Ellipsis, Float, Int, List, Set, Str, Tuple};

fn truthy(v: &Val) -> bool {
    match v {
        Val::None => false,
        Ellipsis => true,
        Bool(b) => *b,
        Int(i) => *i != 0,
        Float(f) => *f != 0.0,
        Str(s) => !s.is_empty(),
        Bytes(b) => !b.is_empty(),
        Tuple(x) | List(x) | Set(x) => !x.is_empty(),
    }
}

/// Numeric view: bools are ints.
enum Num {
    I(i128),
    F(f64),
}

fn num(v: &Val) -> Option<Num> {
    match v {
        Bool(b) => Some(Num::I(i128::from(*b))),
        Int(i) => Some(Num::I(*i)),
        Float(f) => Some(Num::F(*f)),
        _ => None,
    }
}

#[allow(clippy::cast_precision_loss)] // Python converts int to float the same way
fn as_f64(n: &Num) -> f64 {
    match n {
        Num::I(i) => *i as f64,
        Num::F(f) => *f,
    }
}

fn py_eq(a: &Val, b: &Val) -> bool {
    match (num(a), num(b)) {
        (Some(Num::I(x)), Some(Num::I(y))) => return x == y,
        (Some(x), Some(y)) => return as_f64(&x) == as_f64(&y),
        _ => {}
    }
    match (a, b) {
        (Val::None, Val::None) | (Ellipsis, Ellipsis) => true,
        (Str(x), Str(y)) => x == y,
        (Bytes(x), Bytes(y)) => x == y,
        (Tuple(x), Tuple(y)) | (List(x), List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q))
        }
        (Set(x), Set(y)) => x.len() == y.len() && x.iter().all(|p| y.iter().any(|q| py_eq(p, q))),
        _ => false,
    }
}

/// Python ordering; `None` where Python raises TypeError.
fn py_cmp(a: &Val, b: &Val) -> Option<Ordering> {
    match (num(a), num(b)) {
        (Some(Num::I(x)), Some(Num::I(y))) => return Some(x.cmp(&y)),
        (Some(x), Some(y)) => return as_f64(&x).partial_cmp(&as_f64(&y)),
        _ => {}
    }
    match (a, b) {
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Bytes(x), Bytes(y)) => Some(x.cmp(y)),
        (Tuple(x), Tuple(y)) | (List(x), List(y)) => {
            for (p, q) in x.iter().zip(y) {
                if !py_eq(p, q) {
                    return py_cmp(p, q);
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        _ => None,
    }
}

fn is_hashable(v: &Val) -> bool {
    match v {
        List(_) | Set(_) => false,
        Tuple(x) => x.iter().all(is_hashable),
        _ => true,
    }
}

fn contains(container: &Val, item: &Val) -> Option<bool> {
    match (container, item) {
        (Str(h), Str(n)) => Some(h.contains(n.as_str())),
        (Bytes(h), Bytes(n)) => Some(n.is_empty() || h.windows(n.len()).any(|w| w == n.as_slice())),
        (Tuple(x) | List(x), _) => Some(x.iter().any(|v| py_eq(v, item))),
        (Set(x), _) if is_hashable(item) => Some(x.iter().any(|v| py_eq(v, item))),
        _ => None,
    }
}

/// `a is b` where CPython identity is certain: singletons and cached small ints.
fn identical(a: &Val, b: &Val) -> Option<bool> {
    match (a, b) {
        (Val::None, Val::None) | (Ellipsis, Ellipsis) => Some(true),
        (Bool(x), Bool(y)) => Some(x == y),
        (Val::None | Ellipsis | Bool(_), _) | (_, Val::None | Ellipsis | Bool(_)) => Some(false),
        (Int(x), Int(y)) if (-5..=256).contains(x) && (-5..=256).contains(y) => Some(x == y),
        _ => None,
    }
}

fn repeat<T: Clone>(items: &[T], times: i128) -> Option<Vec<T>> {
    let n = usize::try_from(times.max(0)).ok()?;
    if items.len().checked_mul(n)? > 1_000_000 {
        return None;
    }
    Some(
        items
            .iter()
            .cloned()
            .cycle()
            .take(items.len() * n)
            .collect(),
    )
}

fn floor_div_f(a: f64, b: f64) -> Option<f64> {
    if b == 0.0 {
        return None;
    }
    // CPython float_divmod.
    let m = a % b;
    let mut div = (a - m) / b;
    if m != 0.0 && ((b < 0.0) != (m < 0.0)) {
        div -= 1.0;
    }
    if div == 0.0 {
        return Some(0.0_f64.copysign(a / b));
    }
    let floor = div.floor();
    Some(if div - floor > 0.5 {
        floor + 1.0
    } else {
        floor
    })
}

fn mod_f(a: f64, b: f64) -> Option<f64> {
    if b == 0.0 {
        return None;
    }
    let mut m = a % b;
    if m != 0.0 {
        if (b < 0.0) != (m < 0.0) {
            m += b;
        }
    } else {
        m = 0.0_f64.copysign(b);
    }
    Some(m)
}

fn int_op(op: Operator, x: i128, y: i128) -> Option<Val> {
    Some(match op {
        Operator::Add => Int(x.checked_add(y)?),
        Operator::Sub => Int(x.checked_sub(y)?),
        Operator::Mult => Int(x.checked_mul(y)?),
        Operator::Div => {
            if y == 0 {
                return None;
            }
            #[allow(clippy::cast_precision_loss)]
            Float(x as f64 / y as f64)
        }
        Operator::FloorDiv => {
            let q = x.checked_div(y)?;
            Int(if x % y != 0 && ((x < 0) != (y < 0)) {
                q - 1
            } else {
                q
            })
        }
        Operator::Mod => {
            let r = x.checked_rem(y)?;
            Int(if r != 0 && ((r < 0) != (y < 0)) {
                r + y
            } else {
                r
            })
        }
        Operator::Pow if y >= 0 => Int(x.checked_pow(u32::try_from(y).ok()?)?),
        Operator::Pow => {
            if x == 0 {
                return None;
            }
            #[allow(clippy::cast_precision_loss)]
            Float((x as f64).powf(y as f64))
        }
        Operator::LShift if y >= 0 => {
            Int(x.checked_mul(2_i128.checked_pow(u32::try_from(y).ok()?)?)?)
        }
        Operator::RShift if y >= 0 => Int(x >> u32::try_from(y).ok()?.min(127)),
        Operator::BitOr => Int(x | y),
        Operator::BitXor => Int(x ^ y),
        Operator::BitAnd => Int(x & y),
        _ => return None,
    })
}

fn float_op(op: Operator, x: f64, y: f64) -> Option<Val> {
    let v = match op {
        Operator::Add => x + y,
        Operator::Sub => x - y,
        Operator::Mult => x * y,
        Operator::Div if y != 0.0 => x / y,
        Operator::FloorDiv => floor_div_f(x, y)?,
        Operator::Mod => mod_f(x, y)?,
        Operator::Pow => {
            if (x == 0.0 && y < 0.0) || (x < 0.0 && y.fract() != 0.0) {
                return None; // ZeroDivisionError / complex result
            }
            let r = x.powf(y);
            if r.is_infinite() && x.is_finite() && y.is_finite() {
                return None; // OverflowError
            }
            r
        }
        _ => return None,
    };
    Some(Float(v))
}

fn sequence_op(op: Operator, a: &Val, b: &Val) -> Option<Val> {
    match (op, a, b) {
        (Operator::Add, Str(x), Str(y)) => Some(Str(format!("{x}{y}"))),
        (Operator::Add, Bytes(x), Bytes(y)) => Some(Bytes([x.as_slice(), y].concat())),
        (Operator::Add, Tuple(x), Tuple(y)) => Some(Tuple([x.as_slice(), y].concat())),
        (Operator::Add, List(x), List(y)) => Some(List([x.as_slice(), y].concat())),
        (Operator::Mult, s, n) | (Operator::Mult, n, s) if !matches!(s, Int(_) | Bool(_)) => {
            let Some(Num::I(times)) = num(n) else {
                return None;
            };
            match s {
                Str(x) => Some(Str(String::from_iter(repeat(
                    &x.chars().collect::<Vec<_>>(),
                    times,
                )?))),
                Bytes(x) => Some(Bytes(repeat(x, times)?)),
                Tuple(x) => Some(Tuple(repeat(x, times)?)),
                List(x) => Some(List(repeat(x, times)?)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Operand guards of the oracle: magnitudes above 10**6, exponents above
/// 100 and shift counts above 1000 are left unevaluated.
fn guarded(op: Operator, a: &Val, b: &Val) -> bool {
    let big = |v: &Val| match num(v) {
        Some(Num::I(i)) => i.unsigned_abs() > 1_000_000,
        Some(Num::F(f)) => f.abs() > 1e6,
        None => false,
    };
    let right_int = match b {
        Int(i) => Some(i.unsigned_abs()),
        _ => None,
    };
    big(a)
        || big(b)
        || (op == Operator::Pow && right_int.is_some_and(|r| r > 100))
        || (matches!(op, Operator::LShift | Operator::RShift)
            && right_int.is_some_and(|r| r > 1000))
}

fn bin_op(op: Operator, a: &Val, b: &Val) -> Option<Val> {
    if guarded(op, a, b) {
        return None;
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        let both_bool = matches!((a, b), (Bool(_), Bool(_)));
        if both_bool && matches!(op, Operator::BitOr | Operator::BitXor | Operator::BitAnd) {
            let (Bool(p), Bool(q)) = (a, b) else {
                return None;
            };
            return Some(Bool(match op {
                Operator::BitOr => *p | *q,
                Operator::BitXor => *p ^ *q,
                _ => *p & *q,
            }));
        }
        return match (x, y) {
            (Num::I(i), Num::I(j)) => int_op(op, i, j),
            (x, y) => float_op(op, as_f64(&x), as_f64(&y)),
        };
    }
    sequence_op(op, a, b)
}

fn unary(op: UnaryOp, v: &Val) -> Option<Val> {
    match (op, num(v)) {
        (UnaryOp::Not, _) => Some(Bool(!truthy(v))),
        (UnaryOp::UAdd, Some(Num::I(i))) => Some(Int(i)),
        (UnaryOp::UAdd, Some(Num::F(f))) => Some(Float(f)),
        (UnaryOp::USub, Some(Num::I(i))) => Some(Int(i.checked_neg()?)),
        (UnaryOp::USub, Some(Num::F(f))) => Some(Float(-f)),
        (UnaryOp::Invert, Some(Num::I(i))) => Some(Int(!i)),
        _ => None,
    }
}

fn compare(op: CmpOp, a: &Val, b: &Val) -> Option<bool> {
    Some(match op {
        CmpOp::Eq => py_eq(a, b),
        CmpOp::NotEq => !py_eq(a, b),
        CmpOp::Lt => py_cmp(a, b)? == Ordering::Less,
        CmpOp::LtE => py_cmp(a, b)? != Ordering::Greater,
        CmpOp::Gt => py_cmp(a, b)? == Ordering::Greater,
        CmpOp::GtE => py_cmp(a, b)? != Ordering::Less,
        CmpOp::In => contains(b, a)?,
        CmpOp::NotIn => !contains(b, a)?,
        CmpOp::Is => identical(a, b)?,
        CmpOp::IsNot => !identical(a, b)?,
    })
}

fn literal(e: &Expr) -> Option<Val> {
    Some(match e {
        Expr::NoneLiteral(_) => Val::None,
        Expr::EllipsisLiteral(_) => Ellipsis,
        Expr::BooleanLiteral(b) => Bool(b.value),
        Expr::NumberLiteral(n) => match &n.value {
            Number::Int(i) => Int(i128::from(i.as_u64()?)),
            Number::Float(f) => Float(*f),
            Number::Complex { .. } => return None,
        },
        Expr::StringLiteral(s) => Str(s.value.to_str().to_owned()),
        Expr::BytesLiteral(b) => Bytes(b.value.bytes().collect()),
        // An f-string without interpolations is a constant string.
        Expr::FString(f) => {
            let mut text = String::new();
            for part in &f.value {
                match part {
                    ast::FStringPartRef::Literal(l) => text.push_str(&l.value),
                    ast::FStringPartRef::FString(fs) => {
                        for el in &fs.elements {
                            text.push_str(&el.as_literal()?.value);
                        }
                    }
                }
            }
            Str(text)
        }
        _ => return None,
    })
}

fn elements(items: &[Expr], depth: u32) -> Option<Vec<Val>> {
    items.iter().map(|x| const_value(x, depth + 1)).collect()
}

/// `(known, value)` of the oracle's `_const_value`, as `Option`.
fn const_value(e: &Expr, depth: u32) -> Option<Val> {
    if depth > 25 {
        return None;
    }
    match e {
        Expr::Tuple(t) => Some(Tuple(elements(&t.elts, depth)?)),
        Expr::List(l) => Some(List(elements(&l.elts, depth)?)),
        Expr::Set(s) => {
            let items = elements(&s.elts, depth)?;
            if !items.iter().all(is_hashable) {
                return None;
            }
            let mut unique: Vec<Val> = Vec::new();
            for v in items {
                if !unique.iter().any(|u| py_eq(u, &v)) {
                    unique.push(v);
                }
            }
            Some(Set(unique))
        }
        Expr::UnaryOp(u) => unary(u.op, &const_value(&u.operand, depth + 1)?),
        Expr::BinOp(b) => {
            let left = const_value(&b.left, depth + 1)?;
            let right = const_value(&b.right, depth + 1)?;
            bin_op(b.op, &left, &right)
        }
        Expr::BoolOp(b) => {
            let values = elements(&b.values, depth)?;
            Some(Bool(match b.op {
                BoolOp::And => values.iter().all(truthy),
                BoolOp::Or => values.iter().any(truthy),
            }))
        }
        Expr::Compare(c) => match (&*c.ops, &*c.operands) {
            ([op], [l, r]) => Some(Bool(compare(
                *op,
                &const_value(l, depth + 1)?,
                &const_value(r, depth + 1)?,
            )?)),
            _ => None,
        },
        _ => literal(e),
    }
}

fn is_inert(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Pass(_) => true,
        Stmt::Expr(e) => literal_expr(&e.value),
        Stmt::AnnAssign(a) => a.value.is_none(),
        _ => false,
    }
}

fn literal_expr(e: &Expr) -> bool {
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

#[derive(Default)]
struct Walk<'a> {
    functions: Vec<(&'a StmtFunctionDef, (u32, u32))>,
    asserts: Vec<(&'a ast::StmtAssert, (u32, u32))>,
    depth: u32,
}

impl<'a> Visitor<'a> for Walk<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        let key = (self.depth, stmt.start().to_u32());
        match stmt {
            Stmt::FunctionDef(f) => self.functions.push((f, key)),
            Stmt::Assert(a) => self.asserts.push((a, key)),
            _ => {}
        }
        self.depth += 1;
        visitor::walk_stmt(self, stmt);
        self.depth -= 1;
    }
}

pub fn findings(
    file: &str,
    module: &ast::ModModule,
    tokens: &Tokens,
    index: &LineIndex,
) -> Vec<Finding> {
    let mut walk = Walk::default();
    walk.visit_body(&module.body);
    let mut functions = walk.functions;
    functions.sort_by_key(|&(_, key)| key);
    let mut out = Vec::new();
    for (f, _) in functions {
        if !f.name.as_str().starts_with("test") {
            continue;
        }
        let def_line = function_line(f, tokens, index);
        if !f.body.is_empty() && f.body.iter().all(is_inert) {
            let mut lines: Vec<u32> = std::iter::once(def_line)
                .chain(f.body.iter().map(|s| index.line(s.start())))
                .collect();
            lines.sort_unstable();
            lines.dedup();
            let mut finding = Finding::new(file, "empty-test", lines);
            finding.function = Some(f.name.to_string());
            finding.line = Some(def_line);
            out.push(finding);
            continue;
        }
        let mut inner = Walk::default();
        inner.visit_body(&f.body);
        let mut asserts = inner.asserts;
        asserts.sort_by_key(|&(_, key)| key);
        for (a, _) in asserts {
            if const_value(&a.test, 0).is_some_and(|v| truthy(&v)) {
                let (first, last) = (index.line(a.start()), index.end_line(a.end()));
                let mut finding = Finding::new(file, "constant-assert", (first..=last).collect());
                finding.function = Some(f.name.to_string());
                finding.line = Some(first);
                out.push(finding);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> Option<bool> {
        let parsed = ruff_python_parser::parse_expression(src).ok()?;
        const_value(parsed.expr(), 0).map(|v| truthy(&v))
    }

    #[test]
    fn evaluates_like_python() {
        assert_eq!(eval("1 + 1 == 2"), Some(true));
        assert_eq!(eval("-7 // 2 == -4"), Some(true));
        assert_eq!(eval("-7 % 2 == 1"), Some(true));
        assert_eq!(eval("7.5 // -2 == -4.0"), Some(true));
        assert_eq!(eval("'ab' * 2 == 'abab'"), Some(true));
        assert_eq!(eval("(1, 2) < (1, 3)"), Some(true));
        assert_eq!(eval("{1, 1.0, True} == {1}"), Some(true));
        assert_eq!(eval("'a' in 'cat'"), Some(true));
        assert_eq!(eval("None is None"), Some(true));
        assert_eq!(eval("1 < 'a'"), None);
        assert_eq!(eval("x == 1"), None);
        assert_eq!(eval("2 ** 1000 > 0"), None);
        assert_eq!(eval("1 / 0"), None);
    }
}
