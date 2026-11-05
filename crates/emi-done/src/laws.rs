//! The laws part of the check: which law files count, measuring them, and
//! what they mean for the receipt and the proposer.

use std::collections::BTreeSet;
use std::path::Path;

use emi_python::proving::laws::{LawEvidence, LawFiles, measure_laws};

use crate::Config;
use crate::baseline::Baseline;
use crate::candidate::Candidate;
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
    repo: &Path,
    view: &Path,
    (config, baseline, cand): (&Config, &Baseline, &Candidate),
    entries: &[Entry],
) -> LawEvidence {
    let laws: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.changed.what == What::Law)
        .collect();
    if laws.is_empty() && !baseline_has_laws(view, &baseline.sha, &config.laws.dir) {
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
    let req = crate::run::proving_request(repo, config, baseline, cand);
    measure_laws(&req, view, &files)
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

/// What the receipt does not establish about the laws.
pub fn limits(laws: &LawEvidence, dir: &str) -> Vec<String> {
    if laws.laws.is_empty() && laws.proposals.is_empty() && laws.fault.is_none() {
        return vec![format!(
            "No laws: what the change should do is shown only by the proposer's own tests. \
             Laws the person writes in {dir} hold every change to them."
        )];
    }
    let mut out = laws.not_measured.clone();
    out.extend(laws.proposals.iter().filter(|p| !p.vacuous_for.is_empty()).map(|p| {
        format!(
            "The proposed law file {} still holds when {}: it does not pin that behaviour down.",
            p.file,
            p.vacuous_for.join("; ")
        )
    }));
    out
}
