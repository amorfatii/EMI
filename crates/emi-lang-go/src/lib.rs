//! Go adapter (profile `emi-go-0.1`, native).
//!
//! - **SLOC**: rows covered by a non-comment token. `_test.go` files are
//!   test code; generated files (`// Code generated ... DO NOT EDIT.`) are
//!   excluded by the scan.
//! - **Regions**: functions, methods (`T.m`), and top-level `type`,
//!   `const` and `var` declarations; function literals belong to their
//!   enclosing function.
//! - **Complexity**: 1 + `if`, `for`, each non-default `case` (expression,
//!   type, select), `&&`, `||`.
//! - **Coupling**: package level, see [`coupling`].

pub mod coupling;

use std::collections::BTreeSet;

use emi_lang_common::{Spec, children, row, text};
use tree_sitter::{Node, Parser, Tree};

pub use emi_lang_common::{CloneCandidate, FileMeasure};

pub const PROFILE: &str = "emi-go-0.1:tree-sitter-0.26.2:grammar-go-0.25.0";

pub fn parse(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into()).ok()?;
    parser.parse(source, None)
}

/// A `//go:build ignore` (or `// +build ignore`) file: never built,
/// typically a generator program living next to the package it generates.
pub fn is_build_ignored(source: &str) -> bool {
    source
        .lines()
        .take_while(|l| !l.trim_start().starts_with("package "))
        .map(str::trim)
        .any(|l| l == "//go:build ignore" || l == "// +build ignore")
}

/// `// Code generated ... DO NOT EDIT.` before the package clause.
pub fn is_generated(source: &str) -> bool {
    source
        .lines()
        .take_while(|l| !l.trim_start().starts_with("package "))
        .any(|l| l.starts_with("// Code generated ") && l.trim_end().ends_with("DO NOT EDIT."))
}

fn is_function(node: Node) -> bool {
    matches!(node.kind(), "function_declaration" | "method_declaration")
}

fn is_region(node: Node) -> bool {
    is_function(node)
        || (matches!(
            node.kind(),
            "type_declaration" | "const_declaration" | "var_declaration"
        ) && node.parent().is_some_and(|p| p.kind() == "source_file"))
}

/// Receiver base type of a method (`(s *Server)` -> `Server`).
fn receiver_type(method: Node, src: &[u8]) -> String {
    let Some(recv) = method.child_by_field_name("receiver") else {
        return String::new();
    };
    let mut stack = vec![recv];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            return text(n, src).to_owned();
        }
        let mut kids = children(n);
        kids.reverse();
        stack.extend(kids);
    }
    String::new()
}

fn first_name(decl: Node, src: &[u8]) -> Option<String> {
    let mut stack = vec![decl];
    while let Some(n) = stack.pop() {
        if let Some(name) = n
            .child_by_field_name("name")
            .filter(|_| n.id() != decl.id() || is_function(decl))
        {
            return Some(text(name, src).to_owned());
        }
        let mut kids = children(n);
        kids.reverse();
        stack.extend(kids);
    }
    None
}

pub fn name(node: Node, src: &[u8]) -> String {
    match node.kind() {
        "method_declaration" => {
            let m = node
                .child_by_field_name("name")
                .map_or("", |n| text(n, src));
            format!("{}.{m}", receiver_type(node, src))
        }
        "function_declaration" => node
            .child_by_field_name("name")
            .map_or("<anonymous>", |n| text(n, src))
            .to_owned(),
        kind => {
            let keyword = kind.trim_end_matches("_declaration");
            first_name(node, src).map_or_else(|| keyword.to_owned(), |n| format!("{keyword} {n}"))
        }
    }
}

fn branches(node: Node, src: &[u8], events: &mut Vec<(u32, u32)>) {
    let r = row(node.start_position().row);
    match node.kind() {
        "if_statement" | "for_statement" | "expression_case" | "type_case"
        | "communication_case" => events.push((r, 1)),
        "binary_expression"
            if node
                .child_by_field_name("operator")
                .is_some_and(|o| matches!(text(o, src), "&&" | "||")) =>
        {
            events.push((r, 1));
        }
        _ => {}
    }
}

fn placeholder(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "identifier" | "field_identifier" | "type_identifier" | "package_identifier"
        | "label_name" => "$ID",
        "interpreted_string_literal_content"
        | "raw_string_literal_content"
        | "rune_literal"
        | "escape_sequence" => "$STR",
        "int_literal" | "float_literal" | "imaginary_literal" => "$NUM",
        "true" | "false" | "nil" | "iota" => "$KWD",
        _ => return None,
    })
}

pub(crate) const SPEC: Spec = Spec {
    comments: &["comment"],
    is_region,
    is_function,
    name,
    qualified_name: name,
    branches,
    placeholder,
};

pub fn measure(file: &str, source: &str) -> Option<FileMeasure> {
    let tree = parse(source)?;
    Some(emi_lang_common::measure(
        file,
        source,
        &tree,
        &SPEC,
        &BTreeSet::new(),
    ))
}
