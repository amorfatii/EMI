//! The receipt's `change` object: what was found, as data, and the one rule
//! that turns changed functions into a status.

use serde_json::{Value, json};

use crate::classify::BaselineResult;
use crate::proof::FnStatus;

/// How far the change is shown to do what the task asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Proven,
    PartlyProven,
    Unproven,
    /// The candidate changed no function outside the tests.
    None,
    /// Nothing could be measured.
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Proven => "proven",
            Status::PartlyProven => "partly_proven",
            Status::Unproven => "unproven",
            Status::None => "none",
            Status::Unknown => "unknown",
        }
    }
}

/// The status of a change from its functions. `unparsed` counts the changed
/// files in a supported language that could not be read on a side: they
/// hold changed functions nobody can list, so they are unknowns, and a
/// change with any is never `proven` or `none`.
pub fn status_of(functions: &[FnStatus], unparsed: usize) -> Status {
    let total = functions.len() + unparsed;
    let proven = functions.iter().filter(|s| **s == FnStatus::Proven).count();
    let unproven = functions
        .iter()
        .filter(|s| **s == FnStatus::Unproven)
        .count();
    match (total, proven, unproven) {
        (0, ..) => Status::None,
        (t, p, _) if p == t => Status::Proven,
        (_, 0, 0) => Status::Unknown,
        (_, 0, _) => Status::Unproven,
        _ => Status::PartlyProven,
    }
}

/// One changed function outside the tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionEvidence {
    pub file: String,
    /// Qualified name (`Class.method`).
    pub function: String,
    /// First line in the candidate.
    pub line: u32,
    pub status: FnStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvingTest {
    pub id: String,
    pub baseline_result: BaselineResult,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownTest {
    pub id: String,
    pub reason: String,
}

/// Why a test the candidate adds would not notice the change.
pub const PASSES_AT_BASELINE: &str = "passes at the baseline";
pub const RUNS_NO_CHANGED_LINE: &str = "executes no changed line";

/// A test the candidate adds that would not notice the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedundantTest {
    pub id: String,
    /// [`PASSES_AT_BASELINE`] or [`RUNS_NO_CHANGED_LINE`].
    pub reason: &'static str,
}

/// A test the candidate adds that does not pass on the candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailingTest {
    pub id: String,
    /// `failed` or `errored`.
    pub candidate_result: &'static str,
}

/// The `change` object of the receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeEvidence {
    pub status: Status,
    pub functions: Vec<FunctionEvidence>,
    pub proving_tests: Vec<ProvingTest>,
    pub redundant_tests: Vec<RedundantTest>,
    pub failing_tests: Vec<FailingTest>,
    pub unknown_tests: Vec<UnknownTest>,
    /// Everything the measurement could not establish, and why.
    pub not_measured: Vec<String>,
    /// The isolation the runs actually had (`bwrap`, or `process-group`
    /// when bubblewrap was unavailable); `None` when nothing ran. Not part
    /// of the receipt's `change` object: the receipt states isolation once.
    pub isolation: Option<String>,
}

impl ChangeEvidence {
    /// Nothing could be measured: `unknown`, with why. Functions the
    /// change touched may still be listed, as unknown.
    pub fn unknown(functions: Vec<FunctionEvidence>, reasons: Vec<String>) -> ChangeEvidence {
        ChangeEvidence {
            status: Status::Unknown,
            functions,
            proving_tests: Vec::new(),
            redundant_tests: Vec::new(),
            failing_tests: Vec::new(),
            unknown_tests: Vec::new(),
            not_measured: reasons,
            isolation: None,
        }
    }

    /// Sort every list so that the same findings always render the same.
    pub fn sorted(mut self) -> ChangeEvidence {
        self.functions
            .sort_by(|a, b| (&a.file, a.line, &a.function).cmp(&(&b.file, b.line, &b.function)));
        self.proving_tests.sort_by(|a, b| a.id.cmp(&b.id));
        self.redundant_tests.sort_by(|a, b| a.id.cmp(&b.id));
        self.failing_tests.sort_by(|a, b| a.id.cmp(&b.id));
        self.unknown_tests.sort_by(|a, b| a.id.cmp(&b.id));
        self.not_measured.sort();
        self.not_measured.dedup();
        self
    }

    /// The receipt's `change` object (docs/plan.md, interface 3).
    pub fn to_json(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "functions": self.functions.iter().map(|f| json!({
                "file": f.file,
                "function": f.function,
                "line": f.line,
                "status": f.status.as_str(),
            })).collect::<Vec<_>>(),
            "proving_tests": self.proving_tests.iter().map(|t| json!({
                "id": t.id,
                "baseline_result": t.baseline_result.as_str(),
            })).collect::<Vec<_>>(),
            "redundant_tests": self.redundant_tests.iter()
                .map(|t| json!({"id": t.id, "reason": t.reason})).collect::<Vec<_>>(),
            "failing_tests": self.failing_tests.iter()
                .map(|t| json!({"id": t.id, "candidate_result": t.candidate_result}))
                .collect::<Vec<_>>(),
            "unknown_tests": self.unknown_tests.iter()
                .map(|t| json!({"id": t.id, "reason": t.reason})).collect::<Vec<_>>(),
            "not_measured": self.not_measured,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FnStatus::{Proven, Unknown, Unproven};

    #[test]
    fn status_truth_table() {
        let table: &[(&[FnStatus], usize, Status)] = &[
            (&[], 0, Status::None),
            // an unreadable changed file may hold functions: not `none`
            (&[], 1, Status::Unknown),
            (&[Proven], 0, Status::Proven),
            (&[Proven, Proven], 0, Status::Proven),
            (&[Proven], 1, Status::PartlyProven),
            (&[Proven, Unproven], 0, Status::PartlyProven),
            (&[Proven, Unknown], 0, Status::PartlyProven),
            (&[Unproven], 0, Status::Unproven),
            (&[Unproven, Unproven], 0, Status::Unproven),
            (&[Unproven, Unknown], 0, Status::Unproven),
            (&[Unproven], 2, Status::Unproven),
            (&[Unknown], 0, Status::Unknown),
            (&[Unknown, Unknown], 1, Status::Unknown),
        ];
        for (functions, unparsed, want) in table {
            assert_eq!(
                status_of(functions, *unparsed),
                *want,
                "{functions:?} + {unparsed} unparsed"
            );
        }
    }

    #[test]
    fn json_has_the_receipt_shape_and_is_sorted() {
        let change = ChangeEvidence {
            status: Status::PartlyProven,
            functions: vec![
                FunctionEvidence {
                    file: "src/b.py".into(),
                    function: "g".into(),
                    line: 3,
                    status: Unproven,
                },
                FunctionEvidence {
                    file: "src/a.py".into(),
                    function: "f".into(),
                    line: 10,
                    status: Proven,
                },
            ],
            proving_tests: vec![ProvingTest {
                id: "tests/test_a.py::test_new".into(),
                baseline_result: BaselineResult::Errored,
            }],
            redundant_tests: vec![
                RedundantTest {
                    id: "tests/test_a.py::test_z".into(),
                    reason: PASSES_AT_BASELINE,
                },
                RedundantTest {
                    id: "tests/test_a.py::test_a".into(),
                    reason: RUNS_NO_CHANGED_LINE,
                },
            ],
            failing_tests: vec![FailingTest {
                id: "tests/test_a.py::test_w".into(),
                candidate_result: "failed",
            }],
            unknown_tests: vec![UnknownTest {
                id: "tests/test_a.py::test_s".into(),
                reason: "skipped".into(),
            }],
            not_measured: vec!["b".into(), "a".into(), "b".into()],
            isolation: Some("bwrap".into()),
        }
        .sorted();
        let v = change.to_json();
        assert_eq!(
            v,
            json!({
                "status": "partly_proven",
                "functions": [
                    {"file": "src/a.py", "function": "f", "line": 10, "status": "proven"},
                    {"file": "src/b.py", "function": "g", "line": 3, "status": "unproven"},
                ],
                "proving_tests": [
                    {"id": "tests/test_a.py::test_new", "baseline_result": "errored"},
                ],
                "redundant_tests": [
                    {"id": "tests/test_a.py::test_a", "reason": "executes no changed line"},
                    {"id": "tests/test_a.py::test_z", "reason": "passes at the baseline"},
                ],
                "failing_tests": [
                    {"id": "tests/test_a.py::test_w", "candidate_result": "failed"},
                ],
                "unknown_tests": [{"id": "tests/test_a.py::test_s", "reason": "skipped"}],
                "not_measured": ["a", "b"],
            })
        );
    }
}
