//! How far a change to a function could be verified, from the test
//! evidence the profile has. This is not a prediction of bugs: it says
//! whether the tests would notice this function going wrong, which decides
//! whether a change here (an agent's, or anyone's) can be accepted on a
//! green run.
//!
//! The thresholds are defaults, not calibrated values: a function counts
//! as strongly tested when at least [`MIN_MUTANTS`] sampled mutants got a
//! verdict and at least [`STRONG_SCORE`] of them were caught. One surviving
//! mutant is enough to call it weak: it is a wrong change the tests let
//! through.

use crate::mutation::Tally;

pub const STRONG_SCORE: f64 = 0.8;
pub const MIN_MUTANTS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Readiness {
    /// Sampled wrong changes were caught.
    Strong,
    /// Sampled wrong changes got through the tests.
    Weak,
    /// No test runs this function.
    Unexecuted,
    /// Tests run it; how well they check it was not sampled.
    Executed,
    /// No coverage or mutation evidence.
    Unknown,
}

/// What the profile knows about one function's tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Evidence<'a> {
    /// `(executable, executed)` lines of the function, when a report
    /// covers its file.
    pub lines: Option<(u32, u32)>,
    pub mutation: Option<&'a Tally>,
}

pub fn readiness(e: &Evidence) -> Readiness {
    if let Some(t) = e.mutation {
        let enough = t.resolved() >= MIN_MUTANTS;
        if enough && t.score().is_some_and(|s| s >= STRONG_SCORE) {
            return Readiness::Strong;
        }
        if enough || t.survived > 0 {
            return Readiness::Weak;
        }
    }
    match e.lines {
        Some((executable, 0)) if executable > 0 => Readiness::Unexecuted,
        Some((executable, _)) if executable > 0 => Readiness::Executed,
        _ => Readiness::Unknown,
    }
}

impl Readiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Readiness::Strong => "strong",
            Readiness::Weak => "weak",
            Readiness::Unexecuted => "unexecuted",
            Readiness::Executed => "executed",
            Readiness::Unknown => "unknown",
        }
    }

    /// What to do before delegating a change here.
    pub fn delegation(self) -> &'static str {
        match self {
            Readiness::Strong => "delegate",
            Readiness::Weak => "held_out_tests_first",
            Readiness::Unexecuted => "tests_or_human_review_first",
            Readiness::Executed | Readiness::Unknown => "unknown",
        }
    }

    pub fn advice(self) -> &'static str {
        match self {
            Readiness::Strong => {
                "sampled wrong changes here were caught by the tests; a change can be judged by its test run"
            }
            Readiness::Weak => {
                "sampled wrong changes here got through the tests; add held-out tests before accepting changes here on a green run"
            }
            Readiness::Unexecuted => {
                "no test runs this function, so any change passes the suite; add tests or have a person review changes here"
            }
            Readiness::Executed => {
                "tests run it, but how well they check it is not established (see unknowns)"
            }
            Readiness::Unknown => {
                "no test evidence; pass --coverage (and a test command to sample mutants)"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(killed: u32, survived: u32, errors: u32) -> Tally {
        Tally {
            killed,
            survived,
            errors,
            ..Tally::default()
        }
    }

    #[test]
    fn mutation_evidence_decides_when_there_is_enough() {
        let caught = tally(4, 1, 0);
        let missed = tally(1, 3, 0);
        let too_few = tally(2, 0, 5);
        let one_missed = tally(1, 1, 0);
        let with = |t| Evidence {
            lines: Some((10, 8)),
            mutation: Some(t),
        };
        assert_eq!(readiness(&with(&caught)), Readiness::Strong);
        assert_eq!(readiness(&with(&missed)), Readiness::Weak);
        // errors are not verdicts: falls back to what coverage says
        assert_eq!(readiness(&with(&too_few)), Readiness::Executed);
        // a wrong change that got through is evidence enough
        assert_eq!(readiness(&with(&one_missed)), Readiness::Weak);
    }

    #[test]
    fn coverage_alone_separates_unexecuted_from_unknown() {
        let lines = |l| Evidence {
            lines: l,
            mutation: None,
        };
        assert_eq!(readiness(&lines(Some((6, 0)))), Readiness::Unexecuted);
        assert_eq!(readiness(&lines(Some((6, 2)))), Readiness::Executed);
        assert_eq!(readiness(&lines(Some((0, 0)))), Readiness::Unknown);
        assert_eq!(readiness(&lines(None)), Readiness::Unknown);
    }
}
