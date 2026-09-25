//! Python adapter. Reproduces the region, SLOC and complexity semantics of
//! the v0.5 `python_probe` (profile `python-probe-0.12`) on ruff's parser.

mod collect;
mod lines;

use std::collections::BTreeSet;

use emi_ir::regions::{self, Candidate, Region};
use emi_ir::{Evidence, FunctionRecord, RegionRecord, burden, pytext};
use ruff_python_ast::token::{Token, TokenKind};
use ruff_text_size::Ranged;

use crate::collect::Span;
use crate::lines::LineIndex;

/// Measurement profile of this adapter. It reproduces `python-probe-0.12`
/// region/SLOC/CC semantics; the parser pin is part of the identity.
pub const PROFILE: &str = "emi-python-0.1:parity-python-probe-0.12:ruff-parser-0.16.9";

/// Result of measuring one Python file.
#[derive(Debug)]
pub struct FileMeasure {
    pub sloc: u32,
    pub erosion: f64,
    pub functions: Vec<FunctionRecord>,
    pub regions: Vec<RegionRecord>,
}

#[derive(Debug)]
pub struct ParseFailure {
    pub error: &'static str,
    pub message: String,
}

fn skipped(token: &Token) -> bool {
    matches!(
        token.kind(),
        TokenKind::Comment
            | TokenKind::Newline
            | TokenKind::NonLogicalNewline
            | TokenKind::Indent
            | TokenKind::Dedent
            | TokenKind::EndOfFile
            | TokenKind::Semi
    )
}

/// Containment test for token spans against exclusion spans, as the oracle's
/// `_prepare_spans`/`_in_excluded`: sort by start, keep prefix-max ends.
struct Exclusions {
    starts: Vec<(u32, usize)>,
    max_ends: Vec<(u32, usize)>,
}

impl Exclusions {
    fn new(mut spans: Vec<Span>) -> Self {
        spans.sort_by_key(|s| s.0);
        let mut max_ends = Vec::with_capacity(spans.len());
        let mut current: Option<(u32, usize)> = None;
        for &(_, end) in &spans {
            let next = current.map_or(end, |c| c.max(end));
            max_ends.push(next);
            current = Some(next);
        }
        Exclusions {
            starts: spans.into_iter().map(|s| s.0).collect(),
            max_ends,
        }
    }

    fn contains(&self, start: (u32, usize), end: (u32, usize)) -> bool {
        let idx = self.starts.partition_point(|&s| s <= start);
        idx > 0 && end <= self.max_ends[idx - 1]
    }
}

fn eligible_lines(tokens: &[Token], index: &LineIndex, excluded: Vec<Span>) -> BTreeSet<u32> {
    let exclusions = Exclusions::new(excluded);
    let mut lines = BTreeSet::new();
    for token in tokens.iter().filter(|t| !skipped(t)) {
        let start = index.token_pos(token.start());
        let end = index.token_pos(token.end());
        if !exclusions.contains(start, end) {
            lines.extend(start.0..=end.0);
        }
    }
    lines
}

fn candidates(functions: Vec<collect::FunctionInfo>) -> Vec<Candidate> {
    functions
        .into_iter()
        .map(|f| Candidate {
            key: (
                f.end_line - f.def_line,
                u64::from(f.depth),
                u64::from(f.offset),
            ),
            name: f.name,
            qname: f.qname,
            line: f.def_line,
            start: f.start_line,
            end: f.end_line,
        })
        .collect()
}

/// Measure one file. `file` is the scope-relative posix path used in records.
pub fn measure(file: &str, source: &str) -> Result<FileMeasure, ParseFailure> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| ParseFailure {
        error: "SyntaxError",
        message: e.to_string(),
    })?;
    let index = LineIndex::new(source);
    let collected = collect::collect(parsed.syntax(), parsed.tokens(), &index);
    let eligible = eligible_lines(parsed.tokens(), &index, collected.excluded);
    let regions = regions::assign(candidates(collected.functions), &eligible, "<module>");
    let cc = regions::complexity(&regions, &collected.events);
    Ok(records(file, source, &eligible, &regions, &cc))
}

fn records(
    file: &str,
    source: &str,
    eligible: &BTreeSet<u32>,
    regions: &[Region],
    cc: &[u32],
) -> FileMeasure {
    let src_lines = pytext::splitlines(source);
    let mut erosion = 0.0;
    let mut functions = Vec::with_capacity(regions.len());
    let mut inventory = Vec::with_capacity(regions.len());
    for (region, &cc) in regions.iter().zip(cc) {
        let sloc = u32::try_from(region.lines.len()).unwrap_or(u32::MAX);
        let mass = burden::erosion_mass(sloc, cc);
        erosion += mass;
        inventory.push(RegionRecord {
            file: file.to_owned(),
            id: region.qname.clone(),
            sloc,
            sha: regions::content_sha(&src_lines, &region.lines),
            erosion: Evidence::Exact(mass),
        });
        functions.push(FunctionRecord {
            file: file.to_owned(),
            function: region.name.clone(),
            line: region.line,
            sloc,
            cc: Some(cc),
            erosion_mass: Some(mass),
            erosion_exact: None,
            lines: None,
        });
    }
    FileMeasure {
        sloc: u32::try_from(eligible.len()).unwrap_or(u32::MAX),
        erosion,
        functions,
        regions: inventory,
    }
}

#[cfg(test)]
mod tests;
