//! Rust adapter (profile `emi-rust-0.1`, native: no v0.5 oracle exists).
//!
//! - **SLOC**: rows covered by a non-comment token (doc comments are
//!   comments); code inside `#[cfg(test)]` modules and `#[test]` functions
//!   is test code and never product SLOC.
//! - **Regions**: one per `fn` item (free, inherent, trait, nested);
//!   closures belong to their enclosing function. Ids qualify the function
//!   with its inline modules and impl (`Type.method`, `<Type as Trait>.method`).
//! - **Complexity**: 1 + `if` (including `if let` and `else if`),
//!   `while`, `for`, each `match` arm after the first, `?`, `&&`, `||`.
//! - **Redundancy**: whole-body clones (exact and identifier/literal-
//!   normalized, 6+ lines, 50+ leaves) and repeated long lines.
//! - **Coupling**: see [`coupling`].

pub mod coupling;
mod names;

use std::collections::BTreeSet;
use std::fmt::Write as _;

use emi_ir::regions::{self, Candidate, Region};
use emi_ir::{Evidence, Finding, FunctionRecord, RegionRecord, burden, pytext};
use tree_sitter::{Node, Parser, Tree};

pub const PROFILE: &str = "emi-rust-0.1:tree-sitter-0.26.2:grammar-rust-0.24.2";

pub fn parse(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

pub(crate) fn row(r: usize) -> u32 {
    u32::try_from(r + 1).unwrap_or(u32::MAX)
}

pub(crate) fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()]).unwrap_or("")
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

/// Rows covered by a non-comment token, minus test code.
pub fn eligible_lines(tree: &Tree, src: &[u8]) -> BTreeSet<u32> {
    let tests = test_rows(tree, src);
    let mut rows = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "line_comment" | "block_comment") {
            continue;
        }
        if node.child_count() == 0 {
            let (s, e) = (row(node.start_position().row), row(node.end_position().row));
            if node.end_byte() > node.start_byte() {
                rows.extend((s..=e).filter(|r| !tests.contains(r)));
            }
        } else {
            stack.extend(children(node));
        }
    }
    rows
}

/// Complexity events `(row, count)`.
fn branch_events(tree: &Tree, src: &[u8]) -> Vec<(u32, u32)> {
    let mut events = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
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
        stack.extend(children(node));
    }
    events
}

/// Items that own their lines: functions, and declarations (so a file of
/// struct definitions is not read as one long eroded block). Functions
/// nested in impls, traits and inline modules still own their own lines.
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

/// Region candidates outside test code.
pub(crate) fn candidates<'t>(
    tree: &'t Tree,
    src: &[u8],
    tests: &BTreeSet<u32>,
) -> (Vec<Candidate>, Vec<Node<'t>>) {
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_region(node) && !tests.contains(&row(node.start_position().row)) {
            found.push(node);
        }
        stack.extend(children(node));
    }
    found.sort_by_key(|n| (n.start_byte(), n.end_byte()));
    let candidates = found
        .iter()
        .enumerate()
        .map(|(order, f)| {
            let (s, e) = (f.start_position(), f.end_position());
            let span = u32::try_from(e.row - s.row).unwrap_or(u32::MAX);
            let position = (u64::try_from(s.row).unwrap_or(u64::MAX) << 32)
                | u64::try_from(s.column).unwrap_or(0);
            Candidate {
                name: names::item_name(*f, src),
                qname: names::qualified_name(*f, src),
                line: row(s.row),
                start: row(s.row),
                end: row(e.row),
                key: (span, position, u64::try_from(order).unwrap_or(u64::MAX)),
                origin: order,
            }
        })
        .collect();
    (candidates, found)
}

#[derive(Debug, Clone)]
pub struct CloneCandidate {
    pub body_lines: BTreeSet<u32>,
    pub exact: [u8; 32],
    pub normalized: [u8; 32],
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

fn signature(node: Node, src: &[u8], normalized: bool, out: &mut String) -> u32 {
    let (mut stack, mut leaves) = (vec![node], 0);
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "line_comment" | "block_comment") {
            continue;
        }
        let count = n.child_count();
        if count > 0 {
            let _ = writeln!(out, "{}/{count}", n.kind());
            let mut kids = children(n);
            kids.reverse();
            stack.extend(kids);
            continue;
        }
        leaves += 1;
        match placeholder(n.kind()).filter(|_| normalized) {
            Some(p) => out.push_str(p),
            None => {
                let _ = write!(out, "{}/0/{}", n.kind(), text(n, src));
            }
        }
        out.push('\n');
    }
    leaves
}

fn clone_candidates(regions: &[Region], nodes: &[Node], src: &[u8]) -> Vec<CloneCandidate> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    for region in regions {
        let Some(func) = region
            .origin
            .and_then(|i| nodes.get(i))
            .filter(|n| n.kind() == "function_item")
        else {
            continue;
        };
        let Some(body) = func.child_by_field_name("body") else {
            continue;
        };
        if region.lines.len() < 6 {
            continue;
        }
        let (mut exact, mut norm) = (String::new(), String::new());
        let leaves = signature(body, src, false, &mut exact);
        signature(body, src, true, &mut norm);
        if leaves < 50 {
            continue;
        }
        out.push(CloneCandidate {
            body_lines: region.lines.clone(),
            exact: Sha256::digest(exact.as_bytes()).into(),
            normalized: Sha256::digest(norm.as_bytes()).into(),
        });
    }
    out
}

/// Eligible lines of 25+ stripped characters occurring more than once.
fn repeated_lines(source: &str, eligible: &BTreeSet<u32>) -> Vec<u32> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut seen: std::collections::HashMap<&str, Vec<u32>> = std::collections::HashMap::new();
    for &ln in eligible {
        let t = lines.get(ln as usize - 1).map_or("", |t| pytext::strip(t));
        if t.chars().count() >= 25 {
            seen.entry(t).or_default().push(ln);
        }
    }
    let mut out: Vec<u32> = seen
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();
    out.sort_unstable();
    out
}

/// Result of measuring one Rust file.
#[derive(Debug)]
pub struct FileMeasure {
    pub sloc: u32,
    pub erosion: f64,
    pub functions: Vec<FunctionRecord>,
    pub regions: Vec<RegionRecord>,
    pub clones: Vec<CloneCandidate>,
    pub findings: Vec<Finding>,
    pub rule_lines: BTreeSet<u32>,
    pub error_lines: BTreeSet<u32>,
}

fn error_rows(tree: &Tree) -> BTreeSet<u32> {
    let mut rows = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.is_error() || n.is_missing() {
            rows.extend(row(n.start_position().row)..=row(n.end_position().row));
        }
        if n.has_error() {
            stack.extend(children(n));
        }
    }
    rows
}

pub fn measure(file: &str, source: &str) -> Option<FileMeasure> {
    let tree = parse(source)?;
    let src = source.as_bytes();
    let bad = if tree.root_node().has_error() {
        error_rows(&tree)
    } else {
        BTreeSet::new()
    };
    let tests = test_rows(&tree, src);
    let eligible = eligible_lines(&tree, src);
    let (cands, nodes) = candidates(&tree, src, &tests);
    let regions = regions::assign(cands, &eligible, &names::module_qname(file));
    let cc = regions::complexity(&regions, &branch_events(&tree, src));
    let src_lines: Vec<&str> = source.split('\n').collect();
    let (mut functions, mut inventory, mut erosion) = (Vec::new(), Vec::new(), 0.0);
    for (region, &cc) in regions.iter().zip(&cc) {
        let sloc = u32::try_from(region.lines.len()).unwrap_or(u32::MAX);
        let unknown = region.lines.iter().any(|l| bad.contains(l));
        let mass = burden::erosion_mass(sloc, cc);
        if !unknown {
            erosion += mass;
        }
        inventory.push(RegionRecord {
            file: file.to_owned(),
            id: region.qname.clone(),
            sloc,
            sha: regions::content_sha(&src_lines, &region.lines),
            erosion: if unknown {
                Evidence::Partial {
                    known_mass: 0.0,
                    unknown_sloc: sloc,
                }
            } else {
                Evidence::Exact(mass)
            },
            in_cycle: None,
            coupling: None,
        });
        functions.push(FunctionRecord {
            file: file.to_owned(),
            function: region.name.clone(),
            line: region.line,
            sloc,
            cc: (!unknown).then_some(cc),
            erosion_mass: (!unknown).then_some(mass),
            erosion_exact: Some(!unknown),
            lines: Some(region.lines.iter().copied().collect()),
            coupling: None,
        });
    }
    let repeated = repeated_lines(source, &eligible);
    let findings = if repeated.is_empty() {
        Vec::new()
    } else {
        vec![Finding::new(file, "rep-line", repeated.clone())]
    };
    Some(FileMeasure {
        sloc: u32::try_from(eligible.len()).unwrap_or(u32::MAX),
        erosion,
        functions,
        regions: inventory,
        clones: clone_candidates(&regions, &nodes, src),
        findings,
        rule_lines: repeated.into_iter().collect(),
        error_lines: bad,
    })
}
