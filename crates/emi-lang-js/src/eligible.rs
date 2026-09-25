//! Eligible lines of `js_probe.source_lines`: nonblank text outside comments,
//! the shebang and JSX text, and not only semicolons.
//!
//! Excluded ranges come from tree-sitter rows and byte columns and are applied
//! to lines indexed by code point, as in the oracle. Lines are split on `\n`
//! only, matching tree-sitter rows; the oracle used `str.splitlines()` here,
//! which also breaks on U+0085, U+2028, form feeds etc. and then crashed or
//! misattributed lines on such files (see docs/parity.md, divergence D1).

use std::collections::{BTreeSet, HashMap};

use emi_ir::pytext;
use tree_sitter::Tree;

use crate::parse::{push_children, row};

const EXCLUDED: [&str; 3] = ["comment", "hash_bang_line", "jsx_text"];

/// Per-line excluded column spans; `None` end means "to end of line".
fn excluded_spans(tree: &Tree) -> HashMap<u32, Vec<(usize, Option<usize>)>> {
    let mut per_line: HashMap<u32, Vec<(usize, Option<usize>)>> = HashMap::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if EXCLUDED.contains(&node.kind()) {
            let (s, e) = (node.start_position(), node.end_position());
            let (r0, r1) = (row(s.row), row(e.row));
            if r0 == r1 {
                per_line
                    .entry(r0)
                    .or_default()
                    .push((s.column, Some(e.column)));
            } else {
                per_line.entry(r0).or_default().push((s.column, None));
                for line in r0 + 1..r1 {
                    per_line.entry(line).or_default().push((0, None));
                }
                per_line.entry(r1).or_default().push((0, Some(e.column)));
            }
        }
        push_children(&mut stack, node);
    }
    per_line
}

/// `text.strip().strip(";").strip()` is nonempty.
fn has_code(text: &str) -> bool {
    !pytext::strip(pytext::strip(text).trim_matches(';')).is_empty()
}

/// Remainder of `text` after removing spans, slicing by code point.
fn remainder(text: &str, spans: &[(usize, Option<usize>)]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut spans: Vec<(usize, usize)> = spans.iter().map(|&(s, e)| (s, e.unwrap_or(n))).collect();
    spans.sort_unstable();
    let slice = |a: usize, b: usize| -> String {
        let (a, b) = (a.min(n), b.min(n));
        if a < b {
            chars[a..b].iter().collect()
        } else {
            String::new()
        }
    };
    let mut out = String::new();
    let mut pos = 0;
    for (start, end) in spans {
        if start > pos {
            out.push_str(&slice(pos, start));
        }
        if end > pos {
            pos = end;
        }
    }
    out.push_str(&slice(pos, n));
    out
}

pub fn eligible_lines(source: &str, tree: &Tree) -> BTreeSet<u32> {
    let per_line = excluded_spans(tree);
    let mut eligible = BTreeSet::new();
    for (idx, text) in source.split('\n').enumerate() {
        if pytext::is_blank(text) {
            continue;
        }
        let line = row(idx);
        let code = match per_line.get(&line) {
            Some(spans) => has_code(&remainder(text, spans)),
            None => has_code(text),
        };
        if code {
            eligible.insert(line);
        }
    }
    eligible
}
