//! JavaScript/TypeScript adapter. Reproduces the region, SLOC and complexity
//! semantics of the v0.5 `js_probe` (profile `js-probe-0.10`) on the same
//! pinned tree-sitter grammars.

mod eligible;
mod names;
mod parse;

use std::collections::BTreeSet;

use emi_ir::regions::{self, Candidate, Region};
use emi_ir::{Evidence, FunctionRecord, RegionRecord, burden};
use tree_sitter::Tree;

use crate::parse::{push_children, row};

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

fn candidates(tree: &Tree, source: &[u8]) -> Vec<Candidate> {
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if names::FUNCTIONS.contains(&node.kind()) {
            found.push(node);
        }
        push_children(&mut stack, node);
    }
    found
        .into_iter()
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
            }
        })
        .collect()
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
    let regions = regions::assign(candidates(tree, bytes), &eligible, &module_qname);
    let cc = regions::complexity(&regions, &branch_events(tree));
    Some(records(
        file,
        source,
        &eligible,
        &regions,
        &cc,
        parsed.fallback,
        bad,
    ))
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
    }
}

#[cfg(test)]
mod tests;
