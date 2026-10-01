//! Whether a proving test executed a changed function: pure, from the lines
//! the change wrote and the lines coverage says ran.
//!
//! The question is about what this change wrote, so the lines that count
//! are those the change added inside the function. The header does not
//! count: a definition (and its decorators) runs when its module is
//! imported, so coverage marks it executed whether or not anything called
//! the function.

use std::collections::BTreeSet;

use emi_diff::functions::ChangedFn;
use emi_profile::coverage::Lines;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnStatus {
    Proven,
    Unproven,
    Unknown,
}

impl FnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FnStatus::Proven => "proven",
            FnStatus::Unproven => "unproven",
            FnStatus::Unknown => "unknown",
        }
    }
}

/// Net bracket depth change of one line, ignoring text in strings and
/// comments well enough for a signature.
fn depth_change(line: &str) -> i32 {
    let (mut depth, mut quote) = (0, None);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '#') => break,
            (None, '\'' | '"') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The last line of the header that starts at `start` (1-based): decorators
/// and a signature that may span several lines. `start` itself when the
/// text does not say.
pub fn header_end(source: &[&str], start: u32) -> u32 {
    let (mut depth, mut continued, mut decorator) = (0i32, false, false);
    for (n, line) in source
        .iter()
        .enumerate()
        .skip(start.saturating_sub(1) as usize)
    {
        let text = line.trim();
        if depth <= 0 && !continued {
            decorator = text.starts_with('@');
        }
        depth += depth_change(line);
        continued = text.ends_with('\\');
        if depth <= 0 && !continued && !decorator {
            return u32::try_from(n + 1).unwrap_or(u32::MAX);
        }
    }
    start
}

/// The executable lines of the body that the change wrote, or, when it
/// wrote none there (it removed lines, changed only the signature, or
/// wrote only comments), every executable line of the body: the function
/// ran or it did not.
fn target_lines(f: &ChangedFn, body: (u32, u32), executable: &BTreeSet<u32>) -> BTreeSet<u32> {
    let in_body = |l: &u32| (body.0..=body.1).contains(l) && executable.contains(l);
    let wrote: BTreeSet<u32> = f.added_lines.iter().copied().filter(in_body).collect();
    if wrote.is_empty() {
        executable.iter().copied().filter(in_body).collect()
    } else {
        wrote
    }
}

/// Judge one changed function against the lines of its file. `source` is
/// the file at the candidate; `lines` is what coverage says of it, `None`
/// when the report has no entry for the file.
pub fn judge(f: &ChangedFn, source: &str, lines: Option<&Lines>) -> (FnStatus, Option<String>) {
    let unknown = |why: &str| (FnStatus::Unknown, Some(format!("{}: {why}", f.name)));
    let Some(lines) = lines else {
        return unknown("coverage has no entry for the file");
    };
    let Some((start, end)) = f.new else {
        return unknown("the function has no span in the candidate");
    };
    let text: Vec<&str> = source.lines().collect();
    if lines.executable.last().copied().unwrap_or(0) as usize > text.len() {
        return unknown("coverage is of another version of the file");
    }
    let header = header_end(&text, start);
    let body = if header >= end {
        (start, end)
    } else {
        (header + 1, end)
    };
    let targets = target_lines(f, body, &lines.executable);
    if targets.is_empty() {
        return unknown("no executable line in the function body");
    }
    if targets.iter().any(|l| lines.executed.contains(l)) {
        (FnStatus::Proven, None)
    } else {
        (FnStatus::Unproven, None)
    }
}

#[cfg(test)]
mod tests;
