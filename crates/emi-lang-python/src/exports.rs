//! A module's public names: its `__all__` when that is a literal list or
//! tuple of strings, otherwise the module-level `def`, `class` and assigned
//! names that do not start with `_`.

use emi_ir::exports::{Export, signature};
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

/// What `__all__` says about the public names.
enum All {
    Absent,
    Literal(Vec<String>),
    /// Assigned from an expression, assigned twice, or modified in place:
    /// the exported names cannot be read without running the module.
    Dynamic,
}

/// Public names of a module, each function with its parameter and return
/// annotation text. `None` when the source does not parse or `__all__` is
/// not a literal: the names are then unknown, not empty.
pub fn exports(source: &str) -> Option<Vec<Export>> {
    let parsed = ruff_python_parser::parse_module(source).ok()?;
    let body = &parsed.syntax().body;
    let defined = defined(body, source);
    match declared_all(body) {
        All::Absent => Some(
            defined
                .into_iter()
                .filter(|e| !e.name.starts_with('_'))
                .collect(),
        ),
        All::Literal(names) => Some(
            names
                .into_iter()
                .map(|n| {
                    // the last binding of a name is the one importers get
                    let found = defined.iter().rev().find(|e| e.name == n);
                    found.cloned().unwrap_or_else(|| Export::new(n, None))
                })
                .collect(),
        ),
        All::Dynamic => None,
    }
}

fn function_signature(f: &ruff_python_ast::StmtFunctionDef, source: &str) -> String {
    let mut raw = source[f.parameters.range()].to_owned();
    if let Some(returns) = &f.returns {
        raw.push_str(" -> ");
        raw.push_str(&source[returns.range()]);
    }
    signature(&raw)
}

/// Names bound by an assignment target (`a`, `a, b = ...`).
fn target_names(target: &Expr, out: &mut Vec<String>) {
    match target {
        Expr::Name(n) => out.push(n.id.to_string()),
        Expr::Tuple(t) => t.elts.iter().for_each(|e| target_names(e, out)),
        Expr::List(l) => l.elts.iter().for_each(|e| target_names(e, out)),
        Expr::Starred(s) => target_names(&s.value, out),
        _ => {}
    }
}

/// Module-level bindings in source order.
fn defined(body: &[Stmt], source: &str) -> Vec<Export> {
    let mut out = Vec::new();
    for stmt in body {
        let mut names = Vec::new();
        match stmt {
            Stmt::FunctionDef(f) => {
                out.push(Export::new(
                    f.name.to_string(),
                    Some(function_signature(f, source)),
                ));
            }
            Stmt::ClassDef(c) => out.push(Export::new(c.name.to_string(), None)),
            Stmt::Assign(a) => a.targets.iter().for_each(|t| target_names(t, &mut names)),
            Stmt::AnnAssign(a) => target_names(&a.target, &mut names),
            Stmt::TypeAlias(t) => target_names(&t.name, &mut names),
            _ => {}
        }
        out.extend(names.into_iter().map(|n| Export::new(n, None)));
    }
    out
}

fn is_all(e: &Expr) -> bool {
    matches!(e, Expr::Name(n) if n.id.as_str() == "__all__")
}

/// The strings of a literal list or tuple, `None` for anything else.
fn literal_strings(value: &Expr) -> Option<Vec<String>> {
    let elts = match value {
        Expr::List(l) => &l.elts,
        Expr::Tuple(t) => &t.elts,
        _ => return None,
    };
    elts.iter()
        .map(|e| match e {
            Expr::StringLiteral(s) => Some(s.value.to_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// `__all__.extend(...)`, `__all__.append(...)` and the like.
fn mutates_all(e: &Expr) -> bool {
    match e {
        Expr::Call(c) => matches!(&*c.func, Expr::Attribute(a) if is_all(&a.value)),
        _ => false,
    }
}

fn declared_all(body: &[Stmt]) -> All {
    let mut all = All::Absent;
    for stmt in body {
        let assigned = match stmt {
            Stmt::Assign(a) if a.targets.iter().any(is_all) => Some(Some(&*a.value)),
            Stmt::AnnAssign(a) if is_all(&a.target) => Some(a.value.as_deref()),
            Stmt::AugAssign(a) if is_all(&a.target) => return All::Dynamic,
            Stmt::Expr(e) if mutates_all(&e.value) => return All::Dynamic,
            _ => None,
        };
        let Some(value) = assigned else { continue };
        all = match (all, value.and_then(literal_strings)) {
            (All::Absent, Some(names)) => All::Literal(names),
            _ => return All::Dynamic,
        };
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str) -> Option<Vec<(String, Option<String>)>> {
        exports(source).map(|v| v.into_iter().map(|e| (e.name, e.signature)).collect())
    }

    #[test]
    fn public_names_and_signatures() {
        let src = "import os\nX = 1\n_y = 2\ndef f(a,\n      b=1) -> int:\n    pass\ndef _g(): pass\nclass C: pass\n";
        assert_eq!(
            names(src),
            Some(vec![
                ("X".into(), None),
                ("f".into(), Some("(a, b=1) -> int".into())),
                ("C".into(), None),
            ])
        );
    }

    #[test]
    fn literal_all_is_exact_and_dynamic_all_is_unknown() {
        let src = "__all__ = ['f', 'os']\nimport os\ndef f(a): pass\ndef g(): pass\n";
        assert_eq!(
            names(src),
            Some(vec![("f".into(), Some("(a)".into())), ("os".into(), None)])
        );
        assert_eq!(names("__all__ = ['f']\n__all__ += ['g']\n"), None);
        assert_eq!(names("__all__ = base.__all__\n"), None);
        assert_eq!(names("def f(:\n"), None);
    }
}
