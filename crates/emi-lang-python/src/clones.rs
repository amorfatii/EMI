//! Whole-function clone candidates, as `python_probe` gathers them: the body
//! (docstring removed) of every function region that passes the size gate,
//! fingerprinted exactly and normalized.

use std::collections::{BTreeSet, HashMap};

use emi_ir::regions::Region;
use ruff_python_ast::token::Tokens;
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::collect::{FunctionInfo, stmt_line};
use crate::dump::fingerprint;
use crate::lines::LineIndex;
use crate::skipped;

/// Size gate of `whole-body-clones-50t-6l`.
const MIN_LINES: usize = 6;
const MIN_TOKENS: u32 = 50;

#[derive(Debug, Clone)]
pub struct CloneCandidate {
    pub body_lines: BTreeSet<u32>,
    pub exact: [u8; 32],
    pub normalized: [u8; 32],
}

/// Tokens counted by start line, including tokens inside docstrings and `;`.
pub fn line_token_counts(tokens: &Tokens, index: &LineIndex) -> HashMap<u32, u32> {
    let mut counts = HashMap::new();
    for t in tokens
        .iter()
        .filter(|t| !skipped(t) || t.kind() == ruff_python_ast::token::TokenKind::Semi)
    {
        *counts.entry(index.line(t.start())).or_insert(0) += 1;
    }
    counts
}

/// Function body without a leading docstring.
pub fn body_without_docstring(body: &[Stmt]) -> &[Stmt] {
    match body.first() {
        Some(Stmt::Expr(e)) if matches!(*e.value, Expr::StringLiteral(_)) => &body[1..],
        _ => body,
    }
}

pub fn candidates(
    regions: &[Region],
    functions: &[FunctionInfo],
    tokens: &Tokens,
    index: &LineIndex,
) -> Vec<CloneCandidate> {
    let counts = line_token_counts(tokens, index);
    let mut out = Vec::new();
    for region in regions {
        let Some(origin) = region.origin else {
            continue;
        };
        let body = body_without_docstring(&functions[origin].node.body);
        let mut spanned = BTreeSet::new();
        for stmt in body {
            spanned.extend(stmt_line(stmt, tokens, index)..=index.end_line(stmt.end()));
        }
        let body_lines: BTreeSet<u32> = spanned.intersection(&region.lines).copied().collect();
        let count: u32 = body_lines
            .iter()
            .map(|ln| counts.get(ln).copied().unwrap_or(0))
            .sum();
        if body_lines.len() >= MIN_LINES && count >= MIN_TOKENS {
            out.push(CloneCandidate {
                body_lines,
                exact: fingerprint(body, false),
                normalized: fingerprint(body, true),
            });
        }
    }
    out
}
