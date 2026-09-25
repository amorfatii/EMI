//! Whole-body clone candidates of `js_probe._collect_clone_lines`: pre-order
//! (type, arity, leaf text) signatures of a function's statements, exact and
//! with leaf spellings blanked.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use emi_ir::regions::Region;
use tree_sitter::Node;

use crate::parse::{push_children, row};

const MIN_LINES: usize = 6;
const MIN_LEAVES: u32 = 50;

#[derive(Debug, Clone)]
pub struct CloneCandidate {
    pub body_lines: BTreeSet<u32>,
    pub exact: [u8; 32],
    pub normalized: [u8; 32],
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Statements of a function body; an expression-bodied arrow is its own body.
fn body_of(func: Node) -> Vec<Node> {
    match children(func)
        .into_iter()
        .find(|c| c.kind() == "statement_block")
    {
        Some(block) => children(block).into_iter().filter(Node::is_named).collect(),
        None => vec![func],
    }
}

fn leaf_placeholder(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "identifier"
        | "property_identifier"
        | "private_property_identifier"
        | "shorthand_property_identifier"
        | "type_identifier"
        | "predefined_type" => "$ID",
        "string_fragment" => "$STR",
        "number" => "$NUM",
        "true" | "false" | "null" | "undefined" => "$KWD",
        "comment" => "$COMMENT",
        "jsx_text" => "$JSXTEXT",
        "regex" => "$REGEX",
        _ => return None,
    })
}

/// Pre-order `type/arity` for inner nodes and `type/0/text` for leaves,
/// newline-joined; `normalized` blanks leaf spellings.
fn signature(node: Node, src: &[u8], normalized: bool, out: &mut String) {
    let mut stack = vec![node];
    let mut first = true;
    while let Some(current) = stack.pop() {
        if !first {
            out.push('\n');
        }
        first = false;
        let count = current.child_count();
        if count > 0 {
            let _ = write!(out, "{}/{count}", current.kind());
            push_children(&mut stack, current);
            continue;
        }
        match leaf_placeholder(current.kind()).filter(|_| normalized) {
            Some(p) => out.push_str(p),
            None => {
                let text = String::from_utf8_lossy(&src[current.start_byte()..current.end_byte()]);
                let _ = write!(out, "{}/0/{text}", current.kind());
            }
        }
    }
}

fn body_signature(statements: &[Node], src: &[u8], normalized: bool) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut out = String::new();
    for (i, s) in statements.iter().enumerate() {
        if i > 0 {
            out.push('\0');
        }
        signature(*s, src, normalized, &mut out);
    }
    Sha256::digest(out.as_bytes()).into()
}

/// Leaves (outside comments, shebang and JSX text) starting on body lines.
fn body_leaves(statements: &[Node], body_lines: &BTreeSet<u32>) -> u32 {
    let mut count = 0;
    let mut stack: Vec<Node> = statements.to_vec();
    while let Some(current) = stack.pop() {
        if current.child_count() == 0 {
            let excluded = matches!(current.kind(), "comment" | "hash_bang_line" | "jsx_text");
            if !excluded && body_lines.contains(&row(current.start_position().row)) {
                count += 1;
            }
        } else {
            stack.extend(children(current));
        }
    }
    count
}

/// Candidates for every function region not touching syntax-error lines.
pub fn candidates(
    regions: &[Region],
    nodes: &[Node],
    bad: &BTreeSet<u32>,
    src: &[u8],
) -> Vec<CloneCandidate> {
    let mut out = Vec::new();
    for region in regions {
        let Some(origin) = region.origin else {
            continue;
        };
        if region.lines.iter().any(|l| bad.contains(l)) {
            continue;
        }
        let statements = body_of(nodes[origin]);
        let mut rows = BTreeSet::new();
        for s in &statements {
            rows.extend(row(s.start_position().row)..=row(s.end_position().row));
        }
        let body_lines: BTreeSet<u32> = rows.intersection(&region.lines).copied().collect();
        if body_lines.len() >= MIN_LINES && body_leaves(&statements, &body_lines) >= MIN_LEAVES {
            out.push(CloneCandidate {
                exact: body_signature(&statements, src, false),
                normalized: body_signature(&statements, src, true),
                body_lines,
            });
        }
    }
    out
}
