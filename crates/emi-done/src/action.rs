//! The action table (plan.md): what the proposer or the person should do
//! next. One pure function, so the whole table is a unit test.

use crate::config::{OnRedundant, OnUnproven};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Everything the table reads.
#[derive(Clone, Debug)]
pub struct Facts {
    /// Parts that could not be measured for an environment reason.
    pub faults: Vec<String>,
    pub regression_broken: bool,
    /// Tests the candidate adds that do not pass on it.
    pub failing_tests: usize,
    pub redundant_tests: usize,
    pub on_redundant: OnRedundant,
    pub unproven_functions: usize,
    pub on_unproven: OnUnproven,
    /// Changed or deleted old tests without an approval.
    pub unapproved_old_tests: usize,
}

/// Evaluated top to bottom; the first row that matches wins.
pub fn decide(f: &Facts) -> Action {
    let rows: [(bool, Action); 6] = [
        (!f.faults.is_empty(), Action::Incomplete),
        (f.regression_broken, Action::Repair),
        (f.failing_tests > 0, Action::Repair),
        (
            f.on_redundant == OnRedundant::Repair && f.redundant_tests > 0,
            Action::Repair,
        ),
        (
            f.on_unproven == OnUnproven::Repair && f.unproven_functions > 0,
            Action::Repair,
        ),
        (f.unapproved_old_tests > 0, Action::AskPerson),
    ];
    rows.into_iter()
        .find(|(matches, _)| *matches)
        .map_or(Action::Stop, |(_, action)| action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> Facts {
        Facts {
            faults: Vec::new(),
            regression_broken: false,
            failing_tests: 0,
            redundant_tests: 0,
            on_redundant: OnRedundant::Report,
            unproven_functions: 0,
            on_unproven: OnUnproven::Report,
            unapproved_old_tests: 0,
        }
    }

    #[test]
    fn the_table_row_by_row() {
        type Edit = fn(&mut Facts);
        let cases: &[(&str, Edit, Action)] = &[
            ("nothing wrong", |_| {}, Action::Stop),
            (
                "fault",
                |f| f.faults.push("sandbox".into()),
                Action::Incomplete,
            ),
            ("broken", |f| f.regression_broken = true, Action::Repair),
            (
                "failing added test",
                |f| f.failing_tests = 1,
                Action::Repair,
            ),
            ("redundant, report", |f| f.redundant_tests = 1, Action::Stop),
            (
                "redundant, repair",
                |f| {
                    f.redundant_tests = 1;
                    f.on_redundant = OnRedundant::Repair;
                },
                Action::Repair,
            ),
            (
                "unproven, report",
                |f| f.unproven_functions = 2,
                Action::Stop,
            ),
            (
                "unproven, repair",
                |f| {
                    f.unproven_functions = 1;
                    f.on_unproven = OnUnproven::Repair;
                },
                Action::Repair,
            ),
            (
                "repair setting without unproven",
                |f| f.on_unproven = OnUnproven::Repair,
                Action::Stop,
            ),
            (
                "unapproved old test",
                |f| f.unapproved_old_tests = 1,
                Action::AskPerson,
            ),
        ];
        for (name, edit, want) in cases {
            let mut f = clean();
            edit(&mut f);
            assert_eq!(decide(&f), *want, "{name}");
        }
    }

    #[test]
    fn earlier_rows_win_over_later_ones() {
        let all = Facts {
            faults: vec!["x".into()],
            regression_broken: true,
            failing_tests: 1,
            redundant_tests: 1,
            on_redundant: OnRedundant::Repair,
            unproven_functions: 1,
            on_unproven: OnUnproven::Repair,
            unapproved_old_tests: 1,
        };
        assert_eq!(decide(&all), Action::Incomplete);
        let no_fault = Facts {
            faults: Vec::new(),
            ..all.clone()
        };
        assert_eq!(decide(&no_fault), Action::Repair);
        let only_person = Facts {
            regression_broken: false,
            failing_tests: 0,
            redundant_tests: 0,
            unproven_functions: 0,
            faults: Vec::new(),
            ..all
        };
        assert_eq!(decide(&only_person), Action::AskPerson);
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
