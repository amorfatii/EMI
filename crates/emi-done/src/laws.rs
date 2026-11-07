//! The laws part of the check: which law files count, measuring them, and
//! what they mean for the receipt and the proposer.

use std::collections::BTreeSet;
use std::path::Path;

use emi_driver::{Driver, LawEvidence, LawFiles};

use crate::Config;
use crate::oldtests::{Entry, What};

/// Most stand-in runs one check spends on proposed laws.
const MAX_STAND_IN_RUNS: usize = 12;

/// Whether the baseline has any file in the laws directory.
fn baseline_has_laws(view: &Path, baseline: &str, dir: &str) -> bool {
    emi_git::run(view, &["ls-tree", "-r", "--name-only", baseline, "--", dir])
        .is_ok_and(|out| !out.iter().all(u8::is_ascii_whitespace))
}

/// Measure the laws for this check. Nothing runs when there are none at
/// the baseline and the candidate proposes none.
pub fn measure(
    run: &emi_driver::Run,
    config: &Config,
    entries: &[Entry],
    driver: &dyn Driver,
) -> LawEvidence {
    let laws: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.changed.what == What::Law)
        .collect();
    if laws.is_empty() && !baseline_has_laws(run.view, run.baseline, &config.laws.dir) {
        return LawEvidence::default();
    }
    let (approved, proposed): (Vec<&Entry>, Vec<&Entry>) =
        laws.into_iter().partition(|e| e.approved);
    let paths = |list: Vec<&Entry>| -> BTreeSet<String> {
        list.into_iter().map(|e| e.changed.path.clone()).collect()
    };
    let (approved, proposed) = (paths(approved), paths(proposed));
    let files = LawFiles {
        dir: &config.laws.dir,
        approved: &approved,
        proposed: &proposed,
        max_stand_in_runs: MAX_STAND_IN_RUNS,
    };
    driver.laws(run, &files)
}

/// One instruction per law the proposer must make hold.
pub fn repair_lines(laws: &LawEvidence) -> Vec<String> {
    let said = |c: &Option<String>| {
        c.as_deref()
            .map_or(String::new(), |c| format!(" It says: {c}"))
    };
    let broken = laws.laws.iter().filter(|l| l.status == "broken").map(|l| {
        format!(
            "{} is a law that held at the baseline and fails on your change; the laws are the \
             person's and are restored for every check, so fix the code.{}",
            l.id,
            said(&l.counterexample)
        )
    });
    let unmet = laws.laws.iter().filter(|l| l.status == "unmet").map(|l| {
        format!(
            "{} is a law that does not hold yet; make the code satisfy it.{}",
            l.id,
            said(&l.counterexample)
        )
    });
    broken.chain(unmet).collect()
}

/// One instruction per law file the proposer added that a stand-in
/// survives: a person approving it would approve a law that pins nothing
/// down, so the proposer strengthens it before anyone is asked.
pub fn vacuous_proposals(laws: &LawEvidence) -> Vec<String> {
    laws.proposals
        .iter()
        .filter(|p| !p.vacuous_for.is_empty())
        .map(|p| {
            format!(
                "The law file you propose, {}, still holds when {}; make it fail on that, so it pins the behaviour down.",
                p.file,
                p.vacuous_for.join("; ")
            )
        })
        .collect()
}

/// What the receipt does not establish about the laws.
pub fn limits(laws: &LawEvidence, dir: &str) -> Vec<String> {
    if laws.laws.is_empty() && laws.proposals.is_empty() && laws.fault.is_none() {
        return vec![format!(
            "No laws: what the change should do is shown only by the proposer's own tests. \
             Laws the person writes in {dir} hold every change to them."
        )];
    }
    let mut out = laws.not_measured.clone();
    out.extend(sampled(laws));
    out.extend(laws.proposals.iter().filter(|p| !p.vacuous_for.is_empty()).map(|p| {
        format!(
            "The proposed law file {} still holds when {}: it does not pin that behaviour down.",
            p.file,
            p.vacuous_for.join("; ")
        )
    }));
    out
}

/// Most laws named in one line.
const NAMED: usize = 3;

/// `ids` named, the rest counted.
fn named(ids: &[String]) -> String {
    let mut out = ids
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > NAMED {
        out.push_str(&format!(" and {} more", ids.len() - NAMED));
    }
    out
}

/// What a law that holds on drawn inputs does not establish. Where its
/// inputs were every one it can draw, it holds for all of them; where they
/// were a sample, no counterexample among them is not a proof. A law whose
/// assumptions threw away more inputs than it ran on was shown little.
fn sampled(laws: &LawEvidence) -> Vec<String> {
    let held = laws
        .laws
        .iter()
        .filter(|l| matches!(l.status, "holds" | "now_holds"))
        .filter_map(|l| l.examples.as_ref().map(|e| (l, e)));
    let mut sample = Vec::new();
    let mut out = Vec::new();
    for (law, e) in held {
        if !e.every_input {
            sample.push(format!("{} ({})", law.id, e.words()));
        }
        if e.invalid > e.valid {
            out.push(format!(
                "{} ran on {} input(s) and its assumptions threw away {}: it was shown little.",
                law.id, e.valid, e.invalid
            ));
        }
    }
    if !sample.is_empty() {
        out.insert(
            0,
            format!(
                "{} law(s) hold on sampled inputs, not on every input: no counterexample among \
                 them is evidence, not a proof: {}.",
                sample.len(),
                named(&sample)
            ),
        );
    }
    out
}
