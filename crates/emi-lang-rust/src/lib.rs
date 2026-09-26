//! Rust adapter (profile `emi-rust-0.1`, native: no v0.5 oracle exists).
//!
//! - **SLOC**: rows covered by a non-comment token (doc comments are
//!   comments); code inside `#[cfg(test)]` modules and `#[test]` functions
//!   is test code and never product SLOC.
//! - **Regions**: every `fn` item and declaration (struct, enum, trait,
//!   impl, const, inline module...); closures belong to their enclosing
//!   function. Ids qualify with inline modules and impls (`Type.method`,
//!   `<Type as Trait>.method`).
//! - **Complexity**: 1 + `if` (including `if let` and `else if`),
//!   `while`, `for`, each `match` arm after the first, `?`, `&&`, `||`.
//! - **Redundancy**: whole-body clones and repeated long lines.
//! - **Coupling**: see [`coupling`].

pub mod coupling;
mod names;

use std::collections::BTreeSet;

use emi_lang_common::{Spec, children, row, text};
use tree_sitter::{Node, Parser, Tree};

pub use emi_lang_common::{CloneCandidate, FileMeasure};

pub const PROFILE: &str = "emi-rust-0.1:tree-sitter-0.26.2:grammar-rust-0.24.2";

pub fn parse(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

/// Attribute texts attached to an item (preceding `attribute_item` siblings).
fn attributes<'a>(item: Node, src: &'a [u8]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut prev = item.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => out.push(text(p, src)),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_sibling();
    }
    out
}

/// Test code: `#[cfg(test)]` items and `#[test]`-style functions.
pub fn is_test_item(item: Node, src: &[u8]) -> bool {
    attributes(item, src).iter().any(|a| {
        let a: String = a.chars().filter(|c| !c.is_whitespace()).collect();
        a == "#[cfg(test)]"
            || a == "#[test]"
            || a.starts_with("#[tokio::test")
            || a.ends_with("::test]")
    })
}

/// Rows inside test items (with their attributes).
pub(crate) fn test_rows(tree: &Tree, src: &[u8]) -> BTreeSet<u32> {
    let mut rows = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "mod_item" | "function_item" | "impl_item")
            && is_test_item(node, src)
        {
            let mut start = node.start_position().row;
            let mut prev = node.prev_sibling();
            while let Some(p) = prev.filter(|p| p.kind() == "attribute_item") {
                start = p.start_position().row;
                prev = p.prev_sibling();
            }
            rows.extend(row(start)..=row(node.end_position().row));
            continue;
        }
        stack.extend(children(node));
    }
    rows
}

const DECLARATIONS: [&str; 9] = [
    "struct_item",
    "enum_item",
    "union_item",
    "trait_item",
    "impl_item",
    "const_item",
    "static_item",
    "type_item",
    "macro_definition",
];

fn is_region(node: Node) -> bool {
    node.kind() == "function_item"
        || DECLARATIONS.contains(&node.kind())
        || (node.kind() == "mod_item" && node.child_by_field_name("body").is_some())
}

fn is_function(node: Node) -> bool {
    node.kind() == "function_item"
}

fn branches(node: Node, src: &[u8], events: &mut Vec<(u32, u32)>) {
    let r = row(node.start_position().row);
    match node.kind() {
        "if_expression" | "while_expression" | "for_expression" | "try_expression" => {
            events.push((r, 1))
        }
        "match_block" => {
            for arm in children(node)
                .into_iter()
                .filter(|c| c.kind() == "match_arm")
                .skip(1)
            {
                events.push((row(arm.start_position().row), 1));
            }
        }
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
        "identifier"
        | "field_identifier"
        | "type_identifier"
        | "primitive_type"
        | "shorthand_field_identifier" => "$ID",
        "string_content" | "char_literal" | "raw_string_literal" => "$STR",
        "integer_literal" | "float_literal" => "$NUM",
        "true" | "false" => "$KWD",
        _ => return None,
    })
}

pub(crate) const SPEC: Spec = Spec {
    comments: &["line_comment", "block_comment"],
    is_region,
    is_function,
    name: names::item_name,
    qualified_name: names::qualified_name,
    branches,
    placeholder,
};

pub fn measure(file: &str, source: &str) -> Option<FileMeasure> {
    let tree = parse(source)?;
    let tests = test_rows(&tree, source.as_bytes());
    Some(emi_lang_common::measure(file, source, &tree, &SPEC, &tests))
}
