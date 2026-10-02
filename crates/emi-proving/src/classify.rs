//! What an added test is, from how it behaved: pure, so the whole table is
//! tested without running anything.
//!
//! A test is proving when it fails or errors at the baseline and passes on
//! the candidate, redundant when it already passes at the baseline, skipped
//! when it runs at neither, and otherwise unknown with the reason. Nothing
//! is guessed: a run that did not finish and a report that did not line up
//! are unknown.

use crate::junit::Observed;

/// How a proving test behaved at the baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaselineResult {
    Failed,
    Errored,
}

impl BaselineResult {
    pub fn as_str(self) -> &'static str {
        match self {
            BaselineResult::Failed => "failed",
            BaselineResult::Errored => "errored",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Proving(BaselineResult),
    Redundant,
    /// It fails on the candidate (`failed` or `errored`): the proposer's
    /// own test does not pass on the proposer's own change.
    Failing(&'static str),
    /// Skipped or expected to fail at both revisions: it does not run
    /// here (an optional dependency is absent, a marker says so), so it
    /// says nothing about the change and is not a measurement that failed.
    Skipped,
    Unknown(String),
}

/// Whether the baseline run leaves the verdict to the candidate's: a test
/// that failed there could be proving, and one skipped there is skipped
/// only if the candidate skips it too.
pub fn needs_candidate(baseline: &Observed) -> bool {
    matches!(
        baseline,
        Observed::Failed | Observed::Errored | Observed::Skipped
    )
}

fn shown(seen: &Observed) -> &'static str {
    match seen {
        Observed::Passed => "passed",
        Observed::Failed => "failed",
        Observed::Errored => "errored",
        Observed::Skipped => "was skipped",
        Observed::Unknown(_) => "gave no result",
    }
}

/// A test the baseline skipped: skipped if the candidate skips it too.
fn skipped_at_baseline(candidate: Option<&Observed>) -> Verdict {
    match candidate {
        Some(Observed::Skipped) => Verdict::Skipped,
        Some(Observed::Unknown(why)) => Verdict::Unknown(format!("on the candidate: {why}")),
        Some(other) => Verdict::Unknown(format!(
            "it was skipped or expected to fail at the baseline and {} on the candidate",
            shown(other)
        )),
        None => Verdict::Unknown("it was not run on the candidate".into()),
    }
}

/// Judge one added test. `candidate` is its result on the candidate; it is
/// only looked at when the test did not pass at the baseline.
pub fn classify(baseline: &Observed, candidate: Option<&Observed>) -> Verdict {
    let result = match baseline {
        Observed::Passed => return Verdict::Redundant,
        Observed::Skipped => return skipped_at_baseline(candidate),
        Observed::Unknown(why) => return Verdict::Unknown(format!("at the baseline: {why}")),
        Observed::Failed => BaselineResult::Failed,
        Observed::Errored => BaselineResult::Errored,
    };
    match candidate {
        Some(Observed::Passed) => Verdict::Proving(result),
        Some(Observed::Unknown(why)) => Verdict::Unknown(format!("on the candidate: {why}")),
        Some(Observed::Failed) => Verdict::Failing("failed"),
        Some(Observed::Errored) => Verdict::Failing("errored"),
        Some(other) => Verdict::Unknown(format!(
            "it {} at the baseline and {} on the candidate",
            shown(baseline),
            shown(other)
        )),
        None => Verdict::Unknown("it was not run on the candidate".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknown() -> Observed {
        Observed::Unknown("timed out".into())
    }

    #[test]
    fn truth_table() {
        use Observed::{Errored, Failed, Passed, Skipped};
        let proving = |r| Verdict::Proving(r);
        let cases: Vec<(Observed, Option<Observed>, Verdict)> = vec![
            (Failed, Some(Passed), proving(BaselineResult::Failed)),
            (Errored, Some(Passed), proving(BaselineResult::Errored)),
            // passes at the baseline: redundant whatever the candidate does
            (Passed, None, Verdict::Redundant),
            (Passed, Some(Failed), Verdict::Redundant),
            (Passed, Some(unknown()), Verdict::Redundant),
            // fails at the baseline and still fails: the proposer's own test
            // does not pass on the proposer's own change
            (Failed, Some(Failed), Verdict::Failing("failed")),
            (Failed, Some(Errored), Verdict::Failing("errored")),
            (Errored, Some(Failed), Verdict::Failing("failed")),
        ];
        for (baseline, candidate, want) in cases {
            assert_eq!(
                classify(&baseline, candidate.as_ref()),
                want,
                "{baseline:?} / {candidate:?}"
            );
        }
        assert_eq!(classify(&Skipped, Some(&Skipped)), Verdict::Skipped);
        // everything else is unknown, with a reason
        let others = [
            (Skipped, None),
            (Skipped, Some(Passed)),
            (Skipped, Some(Failed)),
            (Skipped, Some(unknown())),
            (unknown(), None),
            (unknown(), Some(Passed)),
            (Failed, None),
            (Failed, Some(Skipped)),
            (Failed, Some(unknown())),
            (Errored, Some(Skipped)),
        ];
        for (baseline, candidate) in others {
            match classify(&baseline, candidate.as_ref()) {
                Verdict::Unknown(why) => assert!(!why.is_empty()),
                v => panic!("{baseline:?} / {candidate:?} gave {v:?}"),
            }
        }
    }

    #[test]
    fn tests_that_did_not_pass_at_the_baseline_go_on_to_the_candidate() {
        for seen in [Observed::Failed, Observed::Errored, Observed::Skipped] {
            assert!(needs_candidate(&seen), "{seen:?}");
        }
        for seen in [Observed::Passed, unknown()] {
            assert!(!needs_candidate(&seen), "{seen:?}");
        }
    }

    #[test]
    fn the_reason_says_where_the_test_gave_no_result() {
        let Verdict::Unknown(at_base) = classify(&unknown(), None) else {
            panic!("expected unknown");
        };
        assert!(at_base.contains("baseline") && at_base.contains("timed out"));
        let Verdict::Unknown(on_cand) = classify(&Observed::Failed, Some(&unknown())) else {
            panic!("expected unknown");
        };
        assert!(on_cand.contains("candidate") && on_cand.contains("timed out"));
    }
}
