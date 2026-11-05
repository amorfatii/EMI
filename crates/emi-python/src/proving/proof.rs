//! Whether a proving test executed a changed function: pure, from the lines
//! the change wrote and the lines coverage says ran.
//!
//! The question is about what this change wrote, so the lines that count
//! are those the change added inside the function. The header does not
//! count: a definition (and its decorators) runs when its module is
//! imported, so coverage marks it executed whether or not anything called
//! the function.

use std::collections::BTreeSet;

use crate::coverage::Lines;
use crate::functions::ChangedFn;

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

/// The lines of the body whose execution shows the function's change was
/// reached. Where the change only added lines to the function, those
/// lines: a test that never runs them did not meet what was written.
/// Where it also removed lines (a guard taken out, a statement rewritten),
/// or wrote nothing executable there, every executable line of the body:
/// a removed line cannot run, and what it used to do is changed for every
/// call of the function, whether or not the call reaches a line that was
/// added elsewhere in it.
fn target_lines(f: &ChangedFn, body: (u32, u32), executable: &BTreeSet<u32>) -> BTreeSet<u32> {
    let in_body = |l: &u32| (body.0..=body.1).contains(l) && executable.contains(l);
    let wrote: BTreeSet<u32> = f.added_lines.iter().copied().filter(in_body).collect();
    if wrote.is_empty() || !f.removed_lines.is_empty() {
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
    if lines.executable.last().copied().unwrap_or(0) as usize > source.lines().count() {
        return unknown("coverage is of another version of the file");
    }
    // the header (decorators, signature) runs on import; a body on the
    // definition's own line is all there is to run
    let body = match f.new_body {
        Some(b) if b > start && b <= end => (b, end),
        Some(_) => (start, end),
        None => return unknown("the function's body was not located in the candidate"),
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
