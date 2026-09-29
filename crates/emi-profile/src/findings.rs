//! One function's profile entry, keeping three kinds of statement apart:
//! **facts** (measured or read from history), **inferences** (what those
//! facts suggest, never measured themselves) and **unknowns** (what the
//! profile could not establish, and why).

use serde_json::{Value, json};

use crate::functions::{Coverage, Mutation, Record};
use crate::readiness::{Evidence, MIN_MUTANTS, Readiness, readiness};

/// Cognitive complexity from which a function counts as hard to follow
/// (the same threshold this repository holds its own functions to).
pub const HARD_TO_FOLLOW: u32 = 15;

/// Files a scope needs before "among the most changed tenth" means
/// anything.
pub const MIN_FILES_FOR_RANK: usize = 10;

/// Scope-wide values an inference compares a function against.
pub struct Context {
    /// Revisions reached by the most changed tenth of the scope's files
    /// (`None` in a scope too small to rank).
    pub revisions_p90: Option<u32>,
}

impl Context {
    pub fn of(records: &[Record]) -> Context {
        let per_file: std::collections::BTreeMap<&str, u32> = records
            .iter()
            .map(|r| (r.file.as_str(), r.revisions))
            .collect();
        let mut revs: Vec<u32> = per_file.into_values().collect();
        revs.sort_unstable();
        let at = (revs.len() * 9) / 10;
        Context {
            revisions_p90: (revs.len() >= MIN_FILES_FOR_RANK)
                .then(|| revs.get(at).copied())
                .flatten(),
        }
    }
}

type Test = fn(&Record, &Context) -> bool;
type Say = fn(&Record) -> String;

fn hard_to_follow(r: &Record) -> String {
    let f = &r.e.function;
    match r.e.steps.first() {
        Some(s) => format!(
            "hard to follow: cognitive complexity {}; the largest step is {} at line {} (emi explain)",
            f.cognitive,
            s.kind,
            s.lines.first().copied().unwrap_or(f.start)
        ),
        None => format!("hard to follow: cognitive complexity {}", f.cognitive),
    }
}

fn changes_often(r: &Record) -> String {
    format!(
        "changes often: {} revisions in the window, among the most changed tenth of the scope's files",
        r.revisions
    )
}

fn changes_with(r: &Record) -> String {
    let partners: Vec<String> = r
        .coupled
        .iter()
        .map(|(f, shared, degree)| format!("{f} ({shared} shared commits, {degree:.0}%)"))
        .collect();
    format!(
        "its file changes together with {}; a change here may need one there",
        partners.join(", ")
    )
}

/// `(when it applies, what it suggests)`.
const INFERENCES: &[(Test, Say)] = &[
    (
        |r, _| r.e.function.cognitive >= HARD_TO_FOLLOW,
        hard_to_follow,
    ),
    (
        |r, c| r.revisions >= 3 && c.revisions_p90.is_some_and(|p| r.revisions >= p),
        changes_often,
    ),
    (|r, _| !r.coupled.is_empty(), changes_with),
];

fn coverage_unknown(c: &Coverage) -> Option<&'static str> {
    match c {
        Coverage::NoReport => Some("coverage: no report given"),
        Coverage::NotInReport => Some("coverage: the report does not include this file"),
        Coverage::RunFailed => Some("coverage: the coverage run failed (see coverage.error)"),
        Coverage::Stale => Some(
            "coverage: the report does not match this version of the file (it names lines past its end)",
        ),
        Coverage::Lines { executable: 0, .. } => {
            Some("coverage: the report marks no executable line in this function")
        }
        Coverage::Lines { .. } => None,
    }
}

fn mutation_unknown(r: &Record) -> Option<String> {
    let unexecuted =
        matches!(r.coverage, Coverage::Lines { executable, executed: 0 } if executable > 0);
    match &r.mutation {
        Mutation::NotRequested => Some("mutation: not sampled (no test command given)".into()),
        Mutation::NotSelected => {
            Some("mutation: not sampled (outside the most exposed functions sampled)".into())
        }
        Mutation::NoMutants if unexecuted => None,
        Mutation::NoMutants => {
            Some("mutation: no operator to mutate on the lines the tests run".into())
        }
        Mutation::BaselineFailed => {
            Some("mutation: the unmutated tests did not pass, so no mutant result counts".into())
        }
        Mutation::Tally(t) if t.errors > 0 => {
            Some(format!("mutation: {} mutant runs could not run", t.errors))
        }
        Mutation::Tally(t) if t.resolved() < MIN_MUTANTS && t.survived == 0 => Some(format!(
            "mutation: only {} mutants got a verdict; {MIN_MUTANTS} are needed to call the tests strong",
            t.resolved()
        )),
        Mutation::Tally(_) => None,
    }
}

pub fn readiness_of(r: &Record) -> Readiness {
    let mutation = match &r.mutation {
        Mutation::Tally(t) => Some(t),
        _ => None,
    };
    readiness(&Evidence {
        lines: r.lines(),
        mutation,
    })
}

fn facts(r: &Record) -> Value {
    let f = &r.e.function;
    let coverage = r
        .lines()
        .map(|(executable, executed)| json!({"executable": executable, "executed": executed}));
    let mutation = match &r.mutation {
        Mutation::Tally(t) => t.to_json(),
        _ => Value::Null,
    };
    json!({"sloc": f.sloc, "cognitive": f.cognitive, "graded": r.e.mass.total,
           "refactoring_steps": r.e.steps.len(),
           "revisions": r.revisions, "authors": r.authors, "ai_share": r.ai_share,
           "coverage": coverage, "mutation": mutation})
}

/// The profile entry for `r`.
pub fn entry(r: &Record, ctx: &Context) -> Value {
    let inferences: Vec<String> = INFERENCES
        .iter()
        .filter(|(applies, _)| applies(r, ctx))
        .map(|(_, say)| say(r))
        .collect();
    let mut unknowns: Vec<String> = coverage_unknown(&r.coverage)
        .map(str::to_owned)
        .into_iter()
        .collect();
    unknowns.extend(mutation_unknown(r));
    let level = readiness_of(r);
    json!({"file": r.file, "function": r.e.function.name, "start": r.e.function.start,
           "end": r.e.function.end, "exposure": r.exposure(),
           "facts": facts(r), "inferences": inferences, "unknowns": unknowns,
           "readiness": level.as_str(), "delegation": level.delegation(), "advice": level.advice()})
}
