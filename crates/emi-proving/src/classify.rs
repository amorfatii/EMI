//! What an added test is, from how it behaved: pure, so the whole table is
//! tested without running anything.
//!
//! A test is proving when it fails or errors at the baseline and passes on
//! the candidate, redundant when it already passes at the baseline, and
//! otherwise unknown with the reason. Nothing is guessed: a skipped test, a
//! run that did not finish and a report that did not line up are unknown.

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
    Unknown(String),
}

/// Whether the baseline run says the test could be proving, so that it is
/// worth running on the candidate.
pub fn failed_at_baseline(baseline: &Observed) -> bool {
    matches!(baseline, Observed::Failed | Observed::Errored)
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

/// Judge one added test. `candidate` is its result on the candidate; it is
/// only looked at when the test failed at the baseline.
pub fn classify(baseline: &Observed, candidate: Option<&Observed>) -> Verdict {
    let result = match baseline {
        Observed::Passed => return Verdict::Redundant,
        Observed::Skipped => {
            return Verdict::Unknown("skipped or expected to fail at the baseline".into());
        }
        Observed::Unknown(why) => return Verdict::Unknown(format!("at the baseline: {why}")),
        Observed::Failed => BaselineResult::Failed,
        Observed::Errored => BaselineResult::Errored,
    };
    match candidate {
        Some(Observed::Passed) => Verdict::Proving(result),
        Some(Observed::Unknown(why)) => Verdict::Unknown(format!("on the candidate: {why}")),
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
        ];
        for (baseline, candidate, want) in cases {
            assert_eq!(
                classify(&baseline, candidate.as_ref()),
                want,
                "{baseline:?} / {candidate:?}"
            );
        }
        // everything else is unknown, with a reason
        let others = [
            (Skipped, None),
            (unknown(), None),
            (unknown(), Some(Passed)),
            (Failed, None),
            (Failed, Some(Failed)),
            (Failed, Some(Errored)),
            (Failed, Some(Skipped)),
            (Failed, Some(unknown())),
            (Errored, Some(Failed)),
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
    fn only_failures_at_the_baseline_go_on_to_the_candidate() {
        assert!(failed_at_baseline(&Observed::Failed));
        assert!(failed_at_baseline(&Observed::Errored));
        for seen in [Observed::Passed, Observed::Skipped, unknown()] {
            assert!(!failed_at_baseline(&seen), "{seen:?}");
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
