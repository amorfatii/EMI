//! JavaScript/TypeScript adapter. Reproduces the region, SLOC and complexity
//! semantics of the v0.5 `js_probe` (profile `js-probe-0.10`) on the same
//! pinned tree-sitter grammars.

mod clones;
mod eligible;
mod names;
mod parse;
pub mod rules;

use std::collections::BTreeSet;

use emi_ir::regions::{self, Candidate, Region};
use emi_ir::{Evidence, Finding, FunctionRecord, RegionRecord, burden, pytext};
use tree_sitter::{Node, Tree};

use crate::parse::{push_children, row};

pub use crate::clones::CloneCandidate;
pub use crate::parse::error_lines;

/// Measurement profile of this adapter. It reproduces `js-probe-0.10`
/// region/SLOC/CC semantics; grammar pins are part of the identity.
pub const PROFILE: &str =
    "emi-js-0.1:parity-js-probe-0.10:tree-sitter-0.26.2:grammar-js-0.25.0:grammar-ts-0.23.2";

/// Result of measuring one JS/TS file.
#[derive(Debug)]
pub struct FileMeasure {
    pub sloc: u32,
    pub erosion: Evidence,
    pub functions: Vec<FunctionRecord>,
    pub regions: Vec<RegionRecord>,
    /// Parsed with the TypeScript grammar after the JS grammar failed.
    pub fallback: bool,
    /// Lines touched by syntax errors (regions on them report unknown erosion).
    pub error_lines: BTreeSet<u32>,
    /// Whole-body clone candidates in region order.
    pub clones: Vec<CloneCandidate>,
    /// Waste, masking and repetition findings in the oracle's order.
    pub findings: Vec<Finding>,
    /// Eligible lines carrying V mass from scored rules.
    pub rule_lines: BTreeSet<u32>,
}

const BRANCH_SINGLE: [&str; 8] = [
    "if_statement",
    "for_statement",
    "for_in_statement",
    "while_statement",
    "do_statement",
    "catch_clause",
    "ternary_expression",
    "switch_case",
];

/// Complexity events: branch nodes and `&&`/`||`/`??` binary expressions.
fn branch_events(tree: &Tree) -> Vec<(u32, u32)> {
    let mut events = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        let logical = kind == "binary_expression"
            && node.child_count() == 3
            && node
                .child(1)
                .is_some_and(|op| matches!(op.kind(), "&&" | "||" | "??"));
        if BRANCH_SINGLE.contains(&kind) || logical {
            events.push((row(node.start_position().row), 1));
        }
        push_children(&mut stack, node);
    }
    events
}

/// Region candidates and the function nodes their `origin` indexes.
fn candidates<'t>(tree: &'t Tree, source: &[u8]) -> (Vec<Candidate>, Vec<Node<'t>>) {
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if names::FUNCTIONS.contains(&node.kind()) {
            found.push(node);
        }
        push_children(&mut stack, node);
    }
    let candidates = found
        .iter()
        .copied()
        .enumerate()
        .map(|(order, f)| {
            let (s, e) = (f.start_position(), f.end_position());
            let start = names::start_row(f);
            let span = u32::try_from(e.row - s.row).unwrap_or(u32::MAX);
            let position = (u64::try_from(s.row).unwrap_or(u64::MAX) << 32)
                | u64::try_from(s.column).unwrap_or(0);
            Candidate {
                name: names::region_name(f, source),
                qname: names::qualified_name(f, source),
                line: row(start),
                start: row(start),
                end: row(e.row),
                key: (span, position, u64::try_from(order).unwrap_or(u64::MAX)),
                origin: order,
            }
        })
        .collect();
    (candidates, found)
}

/// Eligible lines of 25+ stripped characters whose text occurs more than once
/// (lines split on `\n`, as the oracle does here).
fn repeated_lines(source: &str, eligible: &BTreeSet<u32>) -> Vec<u32> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut seen: std::collections::HashMap<&str, Vec<u32>> = std::collections::HashMap::new();
    for &ln in eligible {
        let text = lines.get(ln as usize - 1).map_or("", |t| pytext::strip(t));
        if text.chars().count() >= 25 {
            seen.entry(text).or_default().push(ln);
        }
    }
    let mut repeated: Vec<u32> = seen
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();
    repeated.sort_unstable();
    repeated
}

/// Rule findings plus `rep-line`, and the eligible lines scored rules flag.
fn findings(
    file: &str,
    source: &str,
    tree: &Tree,
    eligible: &BTreeSet<u32>,
) -> (Vec<Finding>, BTreeSet<u32>) {
    let mut hits = rules::findings(tree, source.as_bytes());
    let repeated = repeated_lines(source, eligible);
    if !repeated.is_empty() {
        hits.push(rules::RuleHit {
            rule: "rep-line",
            lines: repeated,
        });
    }
    let rule_lines = hits
        .iter()
        .filter(|h| rules::SCORING_RULES.contains(&h.rule))
        .flat_map(|h| h.lines.iter().copied())
        .filter(|l| eligible.contains(l))
        .collect();
    let findings = hits
        .into_iter()
        .map(|h| Finding::new(file, h.rule, h.lines))
        .collect();
    (findings, rule_lines)
}

/// Measure one file. `source` must already be universal-newline decoded,
/// as the oracle's `Path.read_text()` does.
pub fn measure(file: &str, source: &str) -> Option<FileMeasure> {
    let bytes = source.as_bytes();
    let parsed = parse::parse(file, bytes)?;
    let tree = &parsed.tree;
    let bad = if tree.root_node().has_error() {
        error_lines(tree)
    } else {
        BTreeSet::new()
    };
    let eligible = eligible::eligible_lines(source, tree);
    // The root is `program` except after catastrophic error recovery, where
    // the oracle's naming yields `<anonymous>`; derive it the same way.
    let module_qname = names::qualified_name(tree.root_node(), bytes);
    let (candidates, nodes) = candidates(tree, bytes);
    let regions = regions::assign(candidates, &eligible, &module_qname);
    let cc = regions::complexity(&regions, &branch_events(tree));
    let clones = if eligible.is_empty() {
        Vec::new()
    } else {
        clones::candidates(&regions, &nodes, &bad, bytes)
    };
    let mut measure = records(file, source, &eligible, &regions, &cc, parsed.fallback, bad);
    measure.clones = clones;
    if !eligible.is_empty() {
        (measure.findings, measure.rule_lines) = findings(file, source, tree, &eligible);
    }
    Some(measure)
}

fn records(
    file: &str,
    source: &str,
    eligible: &BTreeSet<u32>,
    regions: &[Region],
    cc: &[u32],
    fallback: bool,
    bad: BTreeSet<u32>,
) -> FileMeasure {
    let src_lines: Vec<&str> = source.split('\n').collect();
    let (mut erosion, mut unknown_sloc) = (0.0, 0u32);
    let mut functions = Vec::with_capacity(regions.len());
    let mut inventory = Vec::with_capacity(regions.len());
    for (region, &cc) in regions.iter().zip(cc) {
        let sloc = u32::try_from(region.lines.len()).unwrap_or(u32::MAX);
        let unknown = region.lines.iter().any(|ln| bad.contains(ln));
        let (cc, mass, evidence) = if unknown {
            unknown_sloc += sloc;
            (
                None,
                None,
                Evidence::Partial {
                    known_mass: 0.0,
                    unknown_sloc: sloc,
                },
            )
        } else {
            let mass = burden::erosion_mass(sloc, cc);
            erosion += mass;
            (Some(cc), Some(mass), Evidence::Exact(mass))
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
            cc,
            erosion_mass: mass,
            erosion_exact: Some(!unknown),
            lines: Some(region.lines.iter().copied().collect()),
            coupling: None,
        });
    }
    let erosion = if unknown_sloc == 0 {
        Evidence::Exact(erosion)
    } else {
        Evidence::Partial {
            known_mass: erosion,
            unknown_sloc,
        }
    };
    FileMeasure {
        sloc: u32::try_from(eligible.len()).unwrap_or(u32::MAX),
        erosion,
        functions,
        regions: inventory,
        fallback,
        error_lines: bad,
        clones: Vec::new(),
        findings: Vec::new(),
        rule_lines: BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests;
