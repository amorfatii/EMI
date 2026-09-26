//! The measurement engine native adapters share. A language supplies a
//! [`Spec`] (comment kinds, which nodes own regions, how to name them,
//! its branch events, clone leaf placeholders) plus its test-code rows;
//! the engine derives eligible SLOC, regions, complexity, erosion, clone
//! candidates and repeated lines exactly as for the other adapters.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use emi_ir::regions::{self, Candidate, Region};
use emi_ir::{Evidence, Finding, FunctionRecord, RegionRecord, burden, pytext};
use tree_sitter::{Node, Tree};

pub fn row(r: usize) -> u32 {
    u32::try_from(r + 1).unwrap_or(u32::MAX)
}

pub fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

pub fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// Complexity events `(row, count)` one node contributes.
pub type Events = Vec<(u32, u32)>;

/// A language's branch-event extractor.
pub type BranchFn = fn(Node, &[u8], &mut Events);

/// What a language contributes to the engine.
pub struct Spec {
    pub comments: &'static [&'static str],
    /// Nodes that own the lines they span (functions and declarations).
    pub is_region: fn(Node) -> bool,
    /// Region nodes eligible as whole-body clones (functions).
    pub is_function: fn(Node) -> bool,
    pub name: fn(Node, &[u8]) -> String,
    pub qualified_name: fn(Node, &[u8]) -> String,
    /// Complexity events a node contributes: `(row, count)`.
    pub branches: BranchFn,
    /// Leaf kinds blanked in normalized clone signatures.
    pub placeholder: fn(&str) -> Option<&'static str>,
}

/// Rows covered by a non-comment token, minus `tests`.
pub fn eligible_lines(tree: &Tree, spec: &Spec, tests: &BTreeSet<u32>) -> BTreeSet<u32> {
    let mut rows = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if spec.comments.contains(&node.kind()) {
            continue;
        }
        if node.child_count() == 0 {
            if node.end_byte() > node.start_byte() {
                let (s, e) = (row(node.start_position().row), row(node.end_position().row));
                rows.extend((s..=e).filter(|r| !tests.contains(r)));
            }
        } else {
            stack.extend(children(node));
        }
    }
    rows
}

fn branch_events(tree: &Tree, spec: &Spec, src: &[u8]) -> Vec<(u32, u32)> {
    let mut events = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        (spec.branches)(node, src, &mut events);
        stack.extend(children(node));
    }
    events
}

/// Region candidates outside test rows, in source order.
pub fn candidates<'t>(
    tree: &'t Tree,
    spec: &Spec,
    src: &[u8],
    tests: &BTreeSet<u32>,
) -> (Vec<Candidate>, Vec<Node<'t>>) {
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if (spec.is_region)(node) && !tests.contains(&row(node.start_position().row)) {
            found.push(node);
        }
        stack.extend(children(node));
    }
    found.sort_by_key(|n| (n.start_byte(), n.end_byte()));
    let cands = found
        .iter()
        .enumerate()
        .map(|(order, f)| {
            let (s, e) = (f.start_position(), f.end_position());
            let span = u32::try_from(e.row - s.row).unwrap_or(u32::MAX);
            let position = (u64::try_from(s.row).unwrap_or(u64::MAX) << 32)
                | u64::try_from(s.column).unwrap_or(0);
            Candidate {
                name: (spec.name)(*f, src),
                qname: (spec.qualified_name)(*f, src),
                line: row(s.row),
                start: row(s.row),
                end: row(e.row),
                key: (span, position, u64::try_from(order).unwrap_or(u64::MAX)),
                origin: order,
            }
        })
        .collect();
    (cands, found)
}

/// Regions of a file: the same assignment every adapter uses.
pub fn regions(
    tree: &Tree,
    spec: &Spec,
    src: &[u8],
    tests: &BTreeSet<u32>,
) -> (Vec<Region>, BTreeSet<u32>) {
    let eligible = eligible_lines(tree, spec, tests);
    let (cands, _) = candidates(tree, spec, src, tests);
    (regions::assign(cands, &eligible, "<module>"), eligible)
}

#[derive(Debug, Clone)]
pub struct CloneCandidate {
    pub body_lines: BTreeSet<u32>,
    pub exact: [u8; 32],
    pub normalized: [u8; 32],
}

fn signature(node: Node, spec: &Spec, src: &[u8], normalized: bool, out: &mut String) -> u32 {
    let (mut stack, mut leaves) = (vec![node], 0);
    while let Some(n) = stack.pop() {
        if spec.comments.contains(&n.kind()) {
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
        match (spec.placeholder)(n.kind()).filter(|_| normalized) {
            Some(p) => out.push_str(p),
            None => {
                let _ = write!(out, "{}/0/{}", n.kind(), text(n, src));
            }
        }
        out.push('\n');
    }
    leaves
}

fn clone_candidates(
    regions: &[Region],
    nodes: &[Node],
    spec: &Spec,
    src: &[u8],
) -> Vec<CloneCandidate> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    for region in regions.iter().filter(|r| r.lines.len() >= 6) {
        let Some(func) = region
            .origin
            .and_then(|i| nodes.get(i))
            .filter(|n| (spec.is_function)(**n))
        else {
            continue;
        };
        let Some(body) = func.child_by_field_name("body") else {
            continue;
        };
        let (mut exact, mut norm) = (String::new(), String::new());
        if signature(body, spec, src, false, &mut exact) < 50 {
            continue;
        }
        signature(body, spec, src, true, &mut norm);
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
    let mut seen: HashMap<&str, Vec<u32>> = HashMap::new();
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

/// Result of measuring one file.
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

pub fn error_rows(tree: &Tree) -> BTreeSet<u32> {
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

/// Measure a parsed file under `spec`, with `tests` rows excluded.
pub fn measure(
    file: &str,
    source: &str,
    tree: &Tree,
    spec: &Spec,
    tests: &BTreeSet<u32>,
) -> FileMeasure {
    let src = source.as_bytes();
    let bad = if tree.root_node().has_error() {
        error_rows(tree)
    } else {
        BTreeSet::new()
    };
    let eligible = eligible_lines(tree, spec, tests);
    let (cands, nodes) = candidates(tree, spec, src, tests);
    let regions = regions::assign(cands, &eligible, "<module>");
    let cc = regions::complexity(&regions, &branch_events(tree, spec, src));
    let is_fn: Vec<bool> = regions
        .iter()
        .map(|r| {
            r.origin
                .and_then(|i| nodes.get(i))
                .is_some_and(|n| (spec.is_function)(*n))
        })
        .collect();
    let src_lines: Vec<&str> = source.split('\n').collect();
    let (mut functions, mut inventory, mut erosion) = (Vec::new(), Vec::new(), 0.0);
    for ((region, &cc), &function) in regions.iter().zip(&cc).zip(&is_fn) {
        let sloc = u32::try_from(region.lines.len()).unwrap_or(u32::MAX);
        let unknown = region.lines.iter().any(|l| bad.contains(l));
        // Declarations (types, constant tables) erode through branching
        // only: a long data table is not long code.
        let mass = if function {
            burden::erosion_mass(sloc, cc)
        } else {
            f64::from(sloc) * burden::severity(f64::from(cc), 10.0, 20.0)
        };
        if !unknown {
            erosion += mass;
        }
        let evidence = if unknown {
            Evidence::Partial {
                known_mass: 0.0,
                unknown_sloc: sloc,
            }
        } else {
            Evidence::Exact(mass)
        };
        inventory.push(RegionRecord {
            file: file.to_owned(),
            id: region.qname.clone(),
            sloc,
            sha: regions::content_sha(&src_lines, &region.lines),
            erosion: evidence,
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
    FileMeasure {
        sloc: u32::try_from(eligible.len()).unwrap_or(u32::MAX),
        erosion,
        functions,
        regions: inventory,
        clones: clone_candidates(&regions, &nodes, spec, src),
        findings,
        rule_lines: repeated.into_iter().collect(),
        error_lines: bad,
    }
}

/// Row -> index of the region owning it.
pub fn owner_index(regions: &[Region]) -> HashMap<u32, usize> {
    regions
        .iter()
        .enumerate()
        .flat_map(|(i, r)| r.lines.iter().map(move |l| (*l, i)))
        .collect()
}
