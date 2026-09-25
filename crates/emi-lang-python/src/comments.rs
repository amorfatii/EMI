//! Comment and repetition diagnostics of `python_probe`: comment bloat,
//! commented-out code, and repeated lines (`rep-line`, which carries V mass).

use std::collections::{BTreeSet, HashMap};

use emi_ir::pytext;
use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_text_size::Ranged;

use crate::lines::LineIndex;
use crate::skipped;

const COMMENT_BLOAT_HEADER: usize = 15;
const COMMENT_BLOAT_FREE: usize = 10;
const COMMENTED_CODE_MIN_LINES: usize = 5;
const REP_LINE_MIN_CHARS: usize = 25;

/// Lines where a `#` comment starts and no code token starts; the line-1
/// shebang is functional and excluded.
pub fn comment_only_lines(source: &str, tokens: &Tokens, index: &LineIndex) -> Vec<u32> {
    let (mut code, mut comments) = (BTreeSet::new(), BTreeSet::new());
    for t in tokens.iter() {
        let line = index.line(t.start());
        if t.kind() == TokenKind::Comment {
            comments.insert(line);
        } else if !skipped(t) || t.kind() == TokenKind::Semi {
            code.insert(line);
        }
    }
    let shebang = source.starts_with("#!");
    comments
        .into_iter()
        .filter(|l| !code.contains(l) && !(*l == 1 && shebang))
        .collect()
}

pub struct Bloat {
    pub flagged: Vec<u32>,
    pub comment_lines: u32,
    pub header_free: u32,
    pub counted: u32,
    pub allowed: u32,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Comment-only lines beyond the free allowance (header lines above the
/// first code line, then 10 + SLOC/4 in file order). `eligible` is nonempty.
pub fn comment_bloat(ordered: &[u32], eligible: &BTreeSet<u32>) -> Bloat {
    let first_code = eligible.first().copied().unwrap_or(0);
    let header: BTreeSet<u32> = ordered
        .iter()
        .copied()
        .filter(|&l| l < first_code)
        .take(COMMENT_BLOAT_HEADER)
        .collect();
    let counted: Vec<u32> = ordered
        .iter()
        .copied()
        .filter(|l| !header.contains(l))
        .collect();
    let allowed = COMMENT_BLOAT_FREE + eligible.len() / 4;
    Bloat {
        flagged: counted.iter().skip(allowed).copied().collect(),
        comment_lines: count(ordered.len()),
        header_free: count(header.len()),
        counted: count(counted.len()),
        allowed: count(allowed),
    }
}

/// `textwrap.dedent` as implemented in CPython 3.14: the margin is the common
/// whitespace prefix of the lexicographically smallest and largest nonblank
/// lines; whitespace-only lines become empty.
fn dedent(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let is_space = |l: &str| !l.is_empty() && pytext::is_blank(l);
    let nonblank: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.is_empty() && !is_space(l))
        .collect();
    let l1 = nonblank.iter().min().copied().unwrap_or("");
    let l2 = nonblank.iter().max().copied().unwrap_or("");
    let mut margin = 0;
    for (i, (c, d)) in l1.chars().zip(l2.chars()).enumerate() {
        margin = i;
        if c != d || !matches!(c, ' ' | '\t') {
            break;
        }
    }
    lines
        .iter()
        .map(|l| {
            if is_space(l) {
                String::new()
            } else {
                l.chars().skip(margin).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs of 5+ consecutive comment-only lines whose uncommented, dedented
/// text parses as a nonempty module.
pub fn commented_code(source: &str, ordered: &[u32]) -> Vec<u32> {
    let raw_lines = pytext::splitlines(source);
    let text_of = |l: u32| raw_lines.get(l as usize - 1).copied().unwrap_or("");
    let mut flagged = Vec::new();
    let mut run: Vec<u32> = Vec::new();
    let flush = |run: &[u32], flagged: &mut Vec<u32>| {
        if run.len() < COMMENTED_CODE_MIN_LINES {
            return;
        }
        let joined: Vec<&str> = run
            .iter()
            .map(|&l| {
                let raw = text_of(l);
                raw.strip_prefix('#')
                    .map_or(raw, |r| r.strip_prefix(' ').unwrap_or(r))
            })
            .collect();
        let body = dedent(&joined.join("\n"));
        let body = body.trim_matches('\n');
        if let Ok(parsed) = ruff_python_parser::parse_module(body)
            && !parsed.syntax().body.is_empty()
        {
            flagged.extend_from_slice(run);
        }
    };
    for &line in ordered {
        if run.last().is_some_and(|&last| line == last + 1) {
            run.push(line);
        } else {
            flush(&run, &mut flagged);
            run = vec![line];
        }
    }
    flush(&run, &mut flagged);
    flagged.sort_unstable();
    flagged
}

/// Eligible lines of 25+ stripped characters whose text occurs more than once.
pub fn repeated_lines(source: &str, eligible: &BTreeSet<u32>) -> Vec<u32> {
    let raw_lines = pytext::splitlines(source);
    let mut seen: HashMap<&str, Vec<u32>> = HashMap::new();
    for &line in eligible {
        let text = raw_lines
            .get(line as usize - 1)
            .map_or("", |t| pytext::strip(t));
        if text.chars().count() >= REP_LINE_MIN_CHARS {
            seen.entry(text).or_default().push(line);
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

#[cfg(test)]
mod tests {
    use super::dedent;

    #[test]
    fn dedent_matches_cpython() {
        assert_eq!(dedent("  a\n    b\n  \n  c"), "a\n  b\n\nc");
        assert_eq!(dedent("\ta\n  b"), "\ta\n  b");
        assert_eq!(dedent("x = 1\n  y"), "x = 1\n  y");
    }
}
