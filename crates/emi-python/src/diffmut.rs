//! Mutation restricted to what a change wrote. Only tokens on the lines a
//! change added inside a function are mutated, in copies of the change's
//! head, and the tests run against each: a mutant that survives is a line
//! of the change the tests would not notice going wrong.
//!
//! It is a report, never a gate: a surviving mutant may behave exactly like
//! the original (an equivalent mutant), so survivors are places to look,
//! not defects.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::functions::{ChangeKind, ChangedFn, ChangedFunctions};
use crate::mutate::{Mutant, mutants};
use serde_json::{Value, json};

use crate::Result;
use crate::mutation::{self, Plan, Tally, Target};

pub const NOTE: &str = "a surviving mutant may be equivalent to the original; \
                        treat survivors as places to look, not defects";

/// What the measurement could say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Measured,
    /// The unmutated head's tests did not pass: no mutant result means
    /// anything, so none is reported.
    Unverified,
    /// The change added no mutable token inside a function of a supported
    /// language.
    NothingToMutate,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Measured => "measured",
            State::Unverified => "unverified",
            State::NothingToMutate => "nothing_to_mutate",
        }
    }
}

/// A mutant of a changed line the tests did not notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Survivor {
    pub file: String,
    pub function: String,
    pub line: u32,
    pub original: String,
    pub replacement: String,
}

/// The parts of the change no mutant was drawn from, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotMutated {
    /// Changed files in a supported language that did not parse on a side.
    pub unparsed: Vec<String>,
    /// Changed files in no supported language.
    pub unsupported: Vec<String>,
    /// Files whose head version could not be read: `file: why`.
    pub unreadable: Vec<String>,
    /// Changed functions to which the change added no line (removed, or
    /// only lines taken out).
    pub removed_only: usize,
}

/// The outcome for one change.
#[derive(Clone, Debug)]
pub struct DiffMutation {
    pub state: State,
    pub base: String,
    pub head: String,
    /// Mutants on added lines before sampling.
    pub candidates: usize,
    /// Mutants run: `Some(0)` when there was nothing to mutate, `None`
    /// when the baseline failed and none ran.
    pub run: Option<usize>,
    /// Summed over functions; only when measured.
    pub tally: Option<Tally>,
    /// Only when measured.
    pub survivors: Option<Vec<Survivor>>,
    pub not_mutated: NotMutated,
    /// The unmutated run (`null` when nothing ran).
    pub baseline: Value,
    /// The isolation the runs actually had.
    pub isolation: Option<String>,
}

/// Mutate the lines `changed` added and run `plan`'s tests against each
/// mutant, at most `max_mutants` of them. `plan.runner.rev` is the head
/// commit (a sha) the change was read at; `base` is recorded as given.
pub fn diff_mutation(
    plan: &Plan,
    base: &str,
    changed: &ChangedFunctions,
    max_mutants: usize,
) -> Result<DiffMutation> {
    if max_mutants == 0 {
        return Err("max_mutants must be at least 1".into());
    }
    let mut not_mutated = NotMutated {
        unparsed: changed.unparsed.clone(),
        unsupported: changed.unsupported.clone(),
        ..NotMutated::default()
    };
    let all = {
        let tree = plan.runner.copy("head")?;
        candidates(&tree.dir, changed, &mut not_mutated)
    };
    let mut out = DiffMutation {
        state: State::NothingToMutate,
        base: base.to_owned(),
        head: plan.runner.rev.to_owned(),
        candidates: all.iter().map(|t| t.mutants.len()).sum(),
        run: Some(0),
        tally: None,
        survivors: None,
        not_mutated,
        baseline: Value::Null,
        isolation: None,
    };
    if out.candidates == 0 {
        return Ok(out);
    }
    let targets = sample(all, max_mutants);
    let results = mutation::run(plan, &targets)?;
    out.baseline = results.baseline;
    out.isolation = Some(results.isolation);
    if !results.passed_baseline {
        out.state = State::Unverified;
        out.run = None;
        return Ok(out);
    }
    out.state = State::Measured;
    out.run = Some(targets.iter().map(|t| t.mutants.len()).sum());
    let (tally, survivors) = gather(&targets, &results.tallies);
    out.tally = Some(tally);
    out.survivors = Some(survivors);
    Ok(out)
}

/// The mutants of each changed function's added lines, in the order of
/// `changed` (file, then line). A line inside nested functions belongs to
/// the innermost one, so no mutant is drawn twice. `tree` holds the head's
/// files.
pub fn candidates(tree: &Path, changed: &ChangedFunctions, not: &mut NotMutated) -> Vec<Target> {
    let wanted: Vec<&ChangedFn> = changed
        .functions
        .iter()
        .filter(|f| f.kind != ChangeKind::Removed && !f.added_lines.is_empty())
        .collect();
    not.removed_only = changed.functions.len() - wanted.len();
    let by_file = head_mutants(tree, &wanted, &mut not.unreadable);
    let mut innermost_first: Vec<usize> = (0..wanted.len()).collect();
    innermost_first.sort_by_key(|&i| wanted[i].new.map_or(0, |(a, b)| b - a));
    let mut claimed: BTreeSet<(&str, usize)> = BTreeSet::new();
    let mut drawn: Vec<Vec<Mutant>> = vec![Vec::new(); wanted.len()];
    for i in innermost_first {
        let f = wanted[i];
        let all = by_file.get(f.file.as_str()).map_or(&[][..], Vec::as_slice);
        drawn[i] = all
            .iter()
            .filter(|m| f.added_lines.contains(&m.line))
            .filter(|m| claimed.insert((f.file.as_str(), m.start)))
            .cloned()
            .collect();
    }
    wanted
        .iter()
        .zip(drawn)
        .filter(|(_, m)| !m.is_empty())
        .map(|(f, mutants)| Target {
            key: (f.file.clone(), f.name.clone(), f.new.map_or(0, |s| s.0)),
            mutants,
        })
        .collect()
}

/// Every mutant of each file the functions are in, read from the head's
/// tree. A file that cannot be read is recorded, never skipped silently.
fn head_mutants<'a>(
    tree: &Path,
    fns: &[&'a ChangedFn],
    unreadable: &mut Vec<String>,
) -> BTreeMap<&'a str, Vec<Mutant>> {
    let mut out = BTreeMap::new();
    for f in fns {
        if out.contains_key(f.file.as_str()) || unreadable.iter().any(|u| u == &f.file) {
            continue;
        }
        match std::fs::read(tree.join(&f.file)) {
            Ok(src) => {
                out.insert(f.file.as_str(), mutants(&src));
            }
            Err(e) => unreadable.push(format!("{}: {e}", f.file)),
        }
    }
    unreadable.sort();
    out
}

/// How many mutants each function gets from `budget`. Every function gets
/// one before any gets a second, and so on, until the budget or its
/// mutants run out; with fewer mutants than functions, the functions are
/// spread evenly over the list (the first always included).
fn quotas(sizes: &[usize], budget: usize) -> Vec<usize> {
    let n = sizes.len();
    let mut q = vec![0; n];
    if budget < n {
        for i in 0..budget {
            q[i * n / budget] = 1;
        }
        return q;
    }
    let mut left = budget;
    while left > 0 {
        let before = left;
        for (i, size) in sizes.iter().enumerate() {
            if left > 0 && q[i] < *size {
                q[i] += 1;
                left -= 1;
            }
        }
        if left == before {
            break;
        }
    }
    q
}

/// At most `budget` mutants in all, spread over each function's lines.
/// Deterministic: the same change and budget always pick the same mutants.
pub fn sample(all: Vec<Target>, budget: usize) -> Vec<Target> {
    let sizes: Vec<usize> = all.iter().map(|t| t.mutants.len()).collect();
    all.into_iter()
        .zip(quotas(&sizes, budget))
        .filter(|(_, q)| *q > 0)
        .map(|(t, q)| Target {
            key: t.key,
            mutants: spread(t.mutants, q),
        })
        .collect()
}

/// The functions' tallies summed, and every survivor with where it is.
pub fn gather(
    targets: &[Target],
    tallies: &BTreeMap<mutation::FunctionKey, Tally>,
) -> (Tally, Vec<Survivor>) {
    let mut total = Tally::default();
    let mut survivors = Vec::new();
    for t in targets {
        let Some(tally) = tallies.get(&t.key) else {
            continue;
        };
        total.killed += tally.killed;
        total.survived += tally.survived;
        total.timed_out += tally.timed_out;
        total.errors += tally.errors;
        total.survivors.extend(tally.survivors.iter().cloned());
        survivors.extend(tally.survivors.iter().map(|m| Survivor {
            file: t.key.0.clone(),
            function: t.key.1.clone(),
            line: m.line,
            original: m.original.clone(),
            replacement: m.replacement.clone(),
        }));
    }
    (total, survivors)
}

impl Survivor {
    pub fn to_json(&self) -> Value {
        json!({"file": self.file, "function": self.function, "line": self.line,
               "original": self.original, "replacement": self.replacement})
    }
}

impl DiffMutation {
    /// Render with the canonical encoder (`emi_kernel::json::dumps_pretty`).
    /// Counts are `null` unless measured: a failed baseline has no results.
    pub fn to_json(&self) -> Value {
        let count = |f: fn(&Tally) -> u32| self.tally.as_ref().map(f);
        let n = &self.not_mutated;
        json!({
            "state": self.state.as_str(),
            "base": self.base,
            "head": self.head,
            "candidates": self.candidates,
            "run": self.run,
            "killed": count(|t| t.killed),
            "survived": count(|t| t.survived),
            "timed_out": count(|t| t.timed_out),
            "errors": count(|t| t.errors),
            "survivors": self.survivors.as_ref().map(|s| s.iter().map(Survivor::to_json).collect::<Vec<_>>()),
            "not_mutated": {"unparsed": n.unparsed, "unsupported": n.unsupported,
                            "unreadable": n.unreadable, "removed_only": n.removed_only},
            "baseline": self.baseline,
            "isolation": self.isolation,
            "note": NOTE,
        })
    }

    /// One line per survivor, then a summary line.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for s in self.survivors.iter().flatten() {
            out.push_str(&format!(
                "survived {}:{} {}: `{}` -> `{}`\n",
                s.file, s.line, s.function, s.original, s.replacement
            ));
        }
        out.push_str(&self.summary());
        out.push('\n');
        out
    }

    fn summary(&self) -> String {
        let n = &self.not_mutated;
        let skipped = format!(
            "not mutated: {} unparsed, {} unsupported, {} unreadable, {} with no added line",
            n.unparsed.len(),
            n.unsupported.len(),
            n.unreadable.len(),
            n.removed_only
        );
        match (&self.tally, self.state) {
            (Some(t), State::Measured) => format!(
                "measured: {} of {} candidate mutants run: {} killed, {} survived, {} timed out, {} errors; {skipped}. {NOTE}",
                self.run.unwrap_or_default(),
                self.candidates,
                t.killed,
                t.survived,
                t.timed_out,
                t.errors
            ),
            (_, State::Unverified) => format!(
                "unverified: the unmutated head's tests did not pass (status {}, exit code {}); no mutant results; {skipped}",
                self.baseline["status"], self.baseline["exit_code"]
            ),
            _ => format!(
                "nothing to mutate: the change added no mutable line in a function; {skipped}"
            ),
        }
    }
}

/// Up to `k` of `all`, spread evenly over them (keeps order; the first is
/// always taken).
pub fn spread(all: Vec<Mutant>, k: usize) -> Vec<Mutant> {
    if all.len() <= k {
        return all;
    }
    let n = all.len();
    (0..k).filter_map(|i| all.get(i * n / k).cloned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotas_cover_every_function_before_any_gets_more() {
        assert_eq!(quotas(&[5, 1, 5], 5), vec![2, 1, 2]);
        assert_eq!(quotas(&[5, 1, 5], 3), vec![1, 1, 1]);
        assert_eq!(quotas(&[2, 2], 10), vec![2, 2]);
        // fewer mutants than functions: spread over the list
        assert_eq!(quotas(&[1, 1, 1, 1], 2), vec![1, 0, 1, 0]);
    }
}
