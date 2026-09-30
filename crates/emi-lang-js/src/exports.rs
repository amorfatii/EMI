//! A module's `export` statements: declared names, `export { a as b }`
//! clauses, `export * as ns`, star re-exports (named `* from '<source>'`) and
//! the default export (named `default`). CommonJS `module.exports` is not
//! read.

use emi_ir::exports::{Export, signature};
use tree_sitter::Node;

use crate::rules::{children, text};

const FUNCTIONS: [&str; 6] = [
    "function_declaration",
    "generator_function_declaration",
    "function_signature",
    "function_expression",
    "generator_function",
    "arrow_function",
];

/// Parameter (and TypeScript return type) text of a function-like node.
fn function_signature(n: Node, src: &[u8]) -> Option<String> {
    if !FUNCTIONS.contains(&n.kind()) {
        return None;
    }
    let params = n
        .child_by_field_name("parameters")
        .or_else(|| n.child_by_field_name("parameter"))?;
    let mut raw = text(params, src).into_owned();
    if let Some(ret) = n.child_by_field_name("return_type") {
        raw.push_str(&text(ret, src));
    }
    Some(signature(&raw))
}

/// Identifiers a declarator binds (`a`, or every name of a pattern).
fn bound_names(pattern: Node, src: &[u8], out: &mut Vec<String>) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            out.push(text(pattern, src).into_owned());
        }
        _ => children(pattern)
            .into_iter()
            .filter(|c| c.kind() != "property_identifier")
            .for_each(|c| bound_names(c, src, out)),
    }
}

fn declaration(decl: Node, src: &[u8], out: &mut Vec<Export>) {
    match decl.kind() {
        "lexical_declaration" | "variable_declaration" => {
            for d in children(decl) {
                let Some(name) = d.child_by_field_name("name") else {
                    continue;
                };
                let value = d.child_by_field_name("value");
                let sig = value.and_then(|v| function_signature(v, src));
                let mut names = Vec::new();
                bound_names(name, src, &mut names);
                out.extend(names.into_iter().map(|n| Export::new(n, sig.clone())));
            }
        }
        "ambient_declaration" => children(decl)
            .into_iter()
            .for_each(|c| declaration(c, src, out)),
        _ => {
            if let Some(name) = decl.child_by_field_name("name") {
                let sig = function_signature(decl, src);
                out.push(Export::new(text(name, src), sig));
            }
        }
    }
}

fn unquote(s: &str) -> String {
    s.trim_matches(['"', '\'']).to_owned()
}

/// `export { a, b as c }`, `export * as ns from`, `export * from`.
fn clause(stmt: Node, src: &[u8], out: &mut Vec<Export>) -> bool {
    let before = out.len();
    for c in children(stmt) {
        match c.kind() {
            "export_clause" => {
                for spec in children(c)
                    .into_iter()
                    .filter(|s| s.kind() == "export_specifier")
                {
                    let named = spec
                        .child_by_field_name("alias")
                        .or_else(|| spec.child_by_field_name("name"));
                    out.extend(named.map(|n| Export::new(unquote(&text(n, src)), None)));
                }
            }
            "namespace_export" => {
                let ns = children(c).into_iter().find(|n| n.is_named());
                out.extend(ns.map(|n| Export::new(unquote(&text(n, src)), None)));
            }
            "*" => {
                if let Some(source) = stmt.child_by_field_name("source") {
                    let from = unquote(&text(source, src));
                    out.push(Export::new(format!("* from '{from}'"), None));
                }
            }
            _ => {}
        }
    }
    out.len() > before
}

fn statement(stmt: Node, src: &[u8], out: &mut Vec<Export>) {
    let default = children(stmt).iter().any(|c| c.kind() == "default");
    let decl = stmt.child_by_field_name("declaration");
    if default {
        let value = decl.or_else(|| stmt.child_by_field_name("value"));
        let sig = value.and_then(|v| function_signature(v, src));
        out.push(Export::new("default", sig));
    } else if let Some(decl) = decl {
        declaration(decl, src, out);
    } else if !clause(stmt, src, out) {
        // a shape not read above (`export = x`): its text names it, so
        // adding or removing it is still seen
        out.push(Export::new(signature(&text(stmt, src)), None));
    }
}

/// Exported names of a module in source order, each function with its
/// parameter (and TypeScript return type) text. `None` when the file does
/// not parse cleanly.
pub fn exports(path: &str, source: &str) -> Option<Vec<Export>> {
    let parsed = crate::parse::parse(path, source.as_bytes())?;
    if parsed.tree.root_node().has_error() {
        return None;
    }
    let src = source.as_bytes();
    let mut out = Vec::new();
    for stmt in children(parsed.tree.root_node()) {
        if stmt.kind() == "export_statement" {
            statement(stmt, src, &mut out);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(path: &str, src: &str) -> Vec<(String, Option<String>)> {
        exports(path, src)
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.name, e.signature))
            .collect()
    }

    #[test]
    fn export_shapes() {
        let src = "export function f(a, b) {}\nfunction g() {}\nexport const h = (x) => x, k = 1;\n\
                   export class C {}\nexport { g as gee };\nexport * from './x';\n\
                   export * as ns from './y';\nexport default function () {}\n";
        assert_eq!(
            names("m.js", src),
            vec![
                ("f".into(), Some("(a, b)".into())),
                ("h".into(), Some("(x)".into())),
                ("k".into(), None),
                ("C".into(), None),
                ("gee".into(), None),
                ("* from './x'".into(), None),
                ("ns".into(), None),
                ("default".into(), Some("()".into())),
            ]
        );
    }

    #[test]
    fn typescript_types_and_return_types() {
        let src = "export function f(a: number): string { return ''; }\n\
                   export interface I {}\nexport type T = number;\n";
        assert_eq!(
            names("m.ts", src),
            vec![
                ("f".into(), Some("(a: number): string".into())),
                ("I".into(), None),
                ("T".into(), None),
            ]
        );
        assert_eq!(exports("m.ts", "export function ("), None);
    }
}
