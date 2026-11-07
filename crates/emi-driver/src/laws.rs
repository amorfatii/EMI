//! What laws showed: the evidence the laws stage returns, whatever the
//! language they are written in. How a driver turns a law's outcome at
//! the baseline and on the candidate into a status is its own; the
//! statuses are these: `holds`, `broken`, `nondeterministic`,
//! `now_holds`, `unmet`, `pending`, `unknown`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// What one law showed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Law {
    pub id: String,
    pub baseline: String,
    pub candidate: String,
    pub status: &'static str,
    /// What the failing law said: the falsifying example, or the failure.
    pub counterexample: Option<String>,
    /// For a law that draws its inputs (a property, a state machine): the
    /// inputs it ran on. None for a law with one fixed input.
    pub examples: Option<Examples>,
}

/// The inputs a law that draws them ran on, in its candidate run. A
/// property that held showed no counterexample among these, which is a
/// proof only when they were every input it can draw (`every_input`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Examples {
    /// Inputs the law ran on.
    pub valid: u64,
    /// Inputs its assumptions threw away.
    pub invalid: u64,
    /// Inputs that failed it.
    pub failing: u64,
    /// The generator stopped because nothing was left to try: the law ran
    /// on every input it can draw.
    pub every_input: bool,
    /// How many inputs each label the law gave its inputs reached: which
    /// cases the inputs reached, and which were never reached.
    pub events: BTreeMap<String, u64>,
}

impl Examples {
    pub fn to_json(&self) -> Value {
        json!({"valid": self.valid, "invalid": self.invalid, "failing": self.failing,
               "every_input": self.every_input, "events": self.events})
    }

    /// The inputs as the receipt words them: "every input (20)", or "100
    /// sampled inputs".
    pub fn words(&self) -> String {
        if self.every_input {
            format!("every input ({})", self.valid)
        } else {
            format!("{} sampled inputs", self.valid)
        }
    }
}

/// What a law the agent proposed would be worth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub file: String,
    /// Its laws' outcomes on the candidate.
    pub laws: Vec<Law>,
    /// "`f` returns None": stand-ins under which every one of its passing
    /// laws still held. A law that holds for a trivial body does not pin
    /// that function down.
    pub vacuous_for: Vec<String>,
    pub notes: Vec<String>,
}

/// The receipt's `laws` object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LawEvidence {
    /// The laws the candidate was held to.
    pub laws: Vec<Law>,
    /// Changed functions and the passing laws that execute what the
    /// change wrote in them: those functions are law-checked.
    pub checked: BTreeMap<String, Vec<String>>,
    pub proposals: Vec<Proposal>,
    /// Digest of the law files as they were applied.
    pub digest: Option<String>,
    pub not_measured: Vec<String>,
    /// Why the laws could not be measured at all.
    pub fault: Option<String>,
    pub isolation: Option<String>,
}

impl LawEvidence {
    /// Laws that could not be measured at all, and why.
    pub fn fault(why: impl Into<String>) -> Self {
        LawEvidence {
            fault: Some(why.into()),
            ..LawEvidence::default()
        }
    }

    pub fn count(&self, status: &str) -> usize {
        self.laws.iter().filter(|l| l.status == status).count()
    }

    pub fn to_json(&self) -> Value {
        let law = |l: &Law| {
            json!({"id": l.id, "baseline": l.baseline, "candidate": l.candidate,
                   "status": l.status, "counterexample": l.counterexample,
                   "examples": l.examples.as_ref().map(Examples::to_json)})
        };
        json!({
            "laws": self.laws.iter().map(law).collect::<Vec<_>>(),
            "checked_functions": self.checked,
            "proposals": self.proposals.iter().map(|p| json!({
                "file": p.file, "laws": p.laws.iter().map(law).collect::<Vec<_>>(),
                "vacuous_for": p.vacuous_for, "notes": p.notes})).collect::<Vec<_>>(),
            "digest": self.digest,
            "not_measured": self.not_measured,
            "fault": self.fault,
        })
    }
}

/// Which law files count, and how.
pub struct LawFiles<'a> {
    /// The laws directory, repository-relative.
    pub dir: &'a str,
    /// Changed law files a person approved: their candidate version (or
    /// their deletion) counts.
    pub approved: &'a BTreeSet<String>,
    /// Changed law files nobody approved: proposals.
    pub proposed: &'a BTreeSet<String>,
    /// Most stand-in runs for all proposals together.
    pub max_stand_in_runs: usize,
}

impl LawFiles<'_> {
    /// Whether `path` is inside the laws directory.
    pub fn holds(&self, path: &str) -> bool {
        path.strip_prefix(self.dir)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}
