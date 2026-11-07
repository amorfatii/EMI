//! The decision: what the proposer or the person should do next, from the
//! evidence counted into [`Facts`]. One pure, total function, evaluated
//! top to bottom, first matching row wins, so the whole table is tested
//! here without running anything.
//!
//! | # | condition | action |
//! |---|---|---|
//! | 1 | an old test or a law that held at the baseline fails, reproducibly | repair |
//! | 2 | a part could not be measured (an environment fault) | incomplete |
//! | 3 | a stable test's or law's outcome did not reproduce under the same pins | incomplete |
//! | 4 | a failing added test, an open law still failing, a divergence or a new timeout under preserve intent, tests that show nothing, or a proposed law a stand-in survives | repair |
//! | 5 | redundant tests, unexercised functions or surviving mutants, each when its policy says repair | repair |
//! | 6 | an unapproved changed old test, runner configuration or law file | ask_person |
//! | 7 | surviving mutants when their policy says ask_person | ask_person |
//! | 8 | the checks ran without the sandbox the configuration asks for where it exists | ask_person |
//! | 9 | otherwise | stop |
//!
//! Row 1 comes before rows 2 and 3: a break that shows twice against a
//! baseline that passed is established whatever else could not be measured
//! or reproduced, and the proposer can act on it. Both rows block `stop`
//! alike; once the break is repaired, the fault decides the next check.
//!
//! Row 8: under the default isolation (`best`), a machine without
//! bubblewrap runs the checks in process groups. What they show was not
//! kept from the host, so it is not the proposer's to accept: a failure
//! is still a failure (rows 1 to 5 come first), a pass goes to a person.
//! An explicit `"isolation": "none"` is the person's own opt-out and does
//! not count here.
//!
//! "Tests that show nothing" (row 4) are tests the proposer offers as
//! evidence that cannot be any: a test that is skipped, or that passes on
//! the old code too, offered for a change no test proves. Counted as
//! [`Facts::tests_show_nothing`]; when to count them is the measuring
//! side's, since only it knows which tests the change added.

/// Any change to [`decide`] or to what [`Facts`] holds bumps this, so a
/// receipt names the rules it was decided under.
pub const POLICY: &str = "emi-policy-4";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Stop,
    Repair,
    AskPerson,
    Incomplete,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Stop => "stop",
            Action::Repair => "repair",
            Action::AskPerson => "ask_person",
            Action::Incomplete => "incomplete",
        }
    }

    /// The process exit status `emi done` ends with.
    pub fn exit_code(self) -> u8 {
        match self {
            Action::Stop => 0,
            Action::Repair => 3,
            Action::AskPerson => 4,
            Action::Incomplete => 5,
        }
    }
}

/// What a kind of finding the proposer could address does to the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnFinding {
    /// It is stated; the proposer may stop.
    Report,
    /// It sends the proposer back.
    Repair,
    /// It waits for a person.
    AskPerson,
}

impl OnFinding {
    pub fn as_str(self) -> &'static str {
        match self {
            OnFinding::Report => "report",
            OnFinding::Repair => "repair",
            OnFinding::AskPerson => "ask_person",
        }
    }

    pub fn from_name(name: &str) -> Option<OnFinding> {
        match name {
            "report" => Some(OnFinding::Report),
            "repair" => Some(OnFinding::Repair),
            "ask_person" => Some(OnFinding::AskPerson),
            _ => None,
        }
    }
}

/// The configured treatment of each finding the proposer could address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub on_redundant: OnFinding,
    pub on_unexercised: OnFinding,
    pub on_survivors: OnFinding,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            on_redundant: OnFinding::Report,
            on_unexercised: OnFinding::Report,
            on_survivors: OnFinding::Report,
        }
    }
}

/// Everything the table reads, already counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Parts that could not be measured for an environment reason.
    pub faults: Vec<String>,
    /// Old tests that passed at the baseline and fail again on a second look.
    pub regression_broken: bool,
    /// Approved laws that held at the baseline and fail on the candidate.
    pub laws_broken: usize,
    /// Stable tests or laws whose outcome differed between two runs under
    /// the same pins.
    pub nondeterministic: usize,
    /// Tests the candidate adds that do not pass on it.
    pub failing_tests: usize,
    /// Approved laws that failed at the baseline and still fail.
    pub laws_unmet: usize,
    /// Inputs on which the candidate answers differently from the baseline
    /// while the person asked for the behaviour to be kept, timeouts
    /// included.
    pub divergences: usize,
    pub redundant_tests: usize,
    /// Changed functions no proving test executes, less those whose
    /// behaviour the person asked to keep and the comparison found
    /// unchanged (no test can fail without such a change).
    pub unexercised_functions: usize,
    /// Mutants of the changed lines no law or proving test kills.
    pub survivors: usize,
    /// Changed or deleted old tests, runner configuration or law files
    /// without an approval.
    pub unapproved: usize,
    /// Tests the proposer offers as evidence that show nothing: skipped,
    /// or passing on the old code too, where no test proves the change.
    pub tests_show_nothing: usize,
    /// Law files the proposer added that still hold when a stand-in
    /// replaces the code they check: the person would be asked to approve
    /// a law that pins nothing down.
    pub vacuous_proposals: usize,
    /// The checks ran without the sandbox the configuration asks for where
    /// it exists (`best` on a machine without bubblewrap).
    pub unsandboxed: bool,
}

fn when(policy: OnFinding, count: usize, wanted: OnFinding) -> bool {
    count > 0 && policy == wanted
}

/// Evaluated top to bottom; the first row that matches wins.
pub fn decide(policy: &Policy, f: &Facts) -> Action {
    use OnFinding::{AskPerson, Repair};
    let rows: [(bool, Action); 8] = [
        (f.regression_broken || f.laws_broken > 0, Action::Repair),
        (!f.faults.is_empty(), Action::Incomplete),
        (f.nondeterministic > 0, Action::Incomplete),
        (
            f.failing_tests > 0
                || f.laws_unmet > 0
                || f.divergences > 0
                || f.tests_show_nothing > 0
                || f.vacuous_proposals > 0,
            Action::Repair,
        ),
        (
            when(policy.on_redundant, f.redundant_tests, Repair)
                || when(policy.on_unexercised, f.unexercised_functions, Repair)
                || when(policy.on_survivors, f.survivors, Repair),
            Action::Repair,
        ),
        (
            f.unapproved > 0
                || when(policy.on_redundant, f.redundant_tests, AskPerson)
                || when(policy.on_unexercised, f.unexercised_functions, AskPerson),
            Action::AskPerson,
        ),
        (
            when(policy.on_survivors, f.survivors, AskPerson),
            Action::AskPerson,
        ),
        (f.unsandboxed, Action::AskPerson),
    ];
    rows.into_iter()
        .find(|(matches, _)| *matches)
        .map_or(Action::Stop, |(_, action)| action)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Edit = fn(&mut Facts);

    /// One edit per count the table reads, each making it nonzero.
    const FINDINGS: &[(&str, Edit)] = &[
        ("fault", |f| f.faults.push("sandbox".into())),
        ("broken", |f| f.regression_broken = true),
        ("law broken", |f| f.laws_broken = 1),
        ("nondeterministic", |f| f.nondeterministic = 1),
        ("failing", |f| f.failing_tests = 1),
        ("law unmet", |f| f.laws_unmet = 1),
        ("divergence", |f| f.divergences = 1),
        ("redundant", |f| f.redundant_tests = 1),
        ("unexercised", |f| f.unexercised_functions = 1),
        ("survivors", |f| f.survivors = 1),
        ("unapproved", |f| f.unapproved = 1),
        ("show nothing", |f| f.tests_show_nothing = 1),
        ("vacuous proposal", |f| f.vacuous_proposals = 1),
        ("unsandboxed", |f| f.unsandboxed = true),
    ];

    const TREATMENTS: [OnFinding; 3] = [OnFinding::Report, OnFinding::Repair, OnFinding::AskPerson];

    fn policies() -> Vec<Policy> {
        let mut out = Vec::new();
        for on_redundant in TREATMENTS {
            for on_unexercised in TREATMENTS {
                for on_survivors in TREATMENTS {
                    out.push(Policy {
                        on_redundant,
                        on_unexercised,
                        on_survivors,
                    });
                }
            }
        }
        out
    }

    /// Every combination of findings, as a bit set over [`FINDINGS`].
    fn all_facts() -> Vec<(u32, Facts)> {
        (0..1u32 << FINDINGS.len())
            .map(|bits| {
                let mut f = Facts::default();
                for (i, (_, edit)) in FINDINGS.iter().enumerate() {
                    if bits & (1 << i) != 0 {
                        edit(&mut f);
                    }
                }
                (bits, f)
            })
            .collect()
    }

    #[test]
    fn each_finding_alone_under_the_default_policy() {
        let p = Policy::default();
        let want = [
            Action::Incomplete,
            Action::Repair,
            Action::Repair,
            Action::Incomplete,
            Action::Repair,
            Action::Repair,
            Action::Repair,
            Action::Stop,
            Action::Stop,
            Action::Stop,
            Action::AskPerson,
            Action::Repair,
            Action::Repair,
            Action::AskPerson,
        ];
        assert_eq!(want.len(), FINDINGS.len());
        for ((name, edit), want) in FINDINGS.iter().zip(want) {
            let mut f = Facts::default();
            edit(&mut f);
            assert_eq!(decide(&p, &f), want, "{name}");
        }
        assert_eq!(decide(&p, &Facts::default()), Action::Stop);
    }

    #[test]
    fn policies_move_reported_findings() {
        let f = Facts {
            survivors: 2,
            ..Facts::default()
        };
        let mut p = Policy::default();
        assert_eq!(decide(&p, &f), Action::Stop);
        p.on_survivors = OnFinding::Repair;
        assert_eq!(decide(&p, &f), Action::Repair);
        p.on_survivors = OnFinding::AskPerson;
        assert_eq!(decide(&p, &f), Action::AskPerson);
    }

    #[test]
    fn a_reproduced_break_outranks_nondeterminism_and_a_fault() {
        let p = Policy::default();
        let mut f = Facts {
            regression_broken: true,
            nondeterministic: 3,
            ..Facts::default()
        };
        assert_eq!(decide(&p, &f), Action::Repair);
        f.faults.push("the laws could not be measured".into());
        assert_eq!(decide(&p, &f), Action::Repair);
        f.regression_broken = false;
        assert_eq!(decide(&p, &f), Action::Incomplete);
    }

    /// Totality and the property that matters most: once anything is
    /// wrong, no further finding can turn the decision into `stop`, and
    /// every input has exactly one decision under every policy.
    #[test]
    fn a_finding_never_turns_a_decision_into_stop() {
        let facts = all_facts();
        for p in policies() {
            for (bits, f) in &facts {
                let before = decide(&p, f);
                for (i, (name, edit)) in FINDINGS.iter().enumerate() {
                    if bits & (1 << i) != 0 {
                        continue;
                    }
                    let mut more = f.clone();
                    edit(&mut more);
                    let after = decide(&p, &more);
                    assert!(
                        !(after == Action::Stop && before != Action::Stop),
                        "adding {name} to {f:?} under {p:?} gave stop"
                    );
                    if before == Action::Incomplete && !f.faults.is_empty() {
                        let breaks = more.regression_broken || more.laws_broken > 0;
                        let want = if breaks {
                            Action::Repair
                        } else {
                            Action::Incomplete
                        };
                        assert_eq!(after, want, "a fault is decisive below a break");
                    }
                }
            }
        }
    }

    #[test]
    fn a_hard_finding_is_never_stop_whatever_the_policy() {
        let hard = [0usize, 1, 2, 3, 4, 5, 6, 10, 11, 12, 13];
        for p in policies() {
            for (bits, f) in all_facts() {
                if hard.iter().any(|i| bits & (1 << i) != 0) {
                    assert_ne!(decide(&p, &f), Action::Stop, "{f:?} under {p:?}");
                }
            }
        }
    }

    #[test]
    fn an_unsandboxed_pass_goes_to_a_person_and_a_failure_stays_a_failure() {
        let p = Policy::default();
        let mut f = Facts {
            unsandboxed: true,
            ..Facts::default()
        };
        assert_eq!(decide(&p, &f), Action::AskPerson);
        f.failing_tests = 1;
        assert_eq!(decide(&p, &f), Action::Repair);
    }

    #[test]
    fn exit_codes_follow_the_interface() {
        let codes: Vec<u8> = [
            Action::Stop,
            Action::Repair,
            Action::AskPerson,
            Action::Incomplete,
        ]
        .into_iter()
        .map(Action::exit_code)
        .collect();
        assert_eq!(codes, [0, 3, 4, 5]);
    }
}
