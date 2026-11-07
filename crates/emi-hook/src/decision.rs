//! The loop logic, independent of any harness and of how `emi done` runs.
//!
//! Given the session state, the identity of the working tree and the result
//! of the done check, decide whether to stay silent, send the agent back, or
//! let it stop and tell the person what the receipt rests on. Everything
//! here is a pure function so the whole table can be tested.

/// What `emi done` asked for, as the hook understands it.
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
}

/// The done check's result reduced to what the loop needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoneResult {
    pub action: Action,
    /// The receipt's `repair` lines: all the proposer is told.
    pub repair: Vec<String>,
    /// What a person reads: lines rendered from the receipt, or the reason
    /// the check could not be interpreted.
    pub summary: Vec<String>,
    /// Review items only a person can approve.
    pub approve: Vec<String>,
    /// What the receipt showed, in counts, for the journal; `null` when
    /// there was no receipt to read.
    pub evidence: serde_json::Value,
}

impl DoneResult {
    /// A check that could not be interpreted. Never a pass.
    pub fn incomplete(reason: impl Into<String>) -> Self {
        DoneResult {
            action: Action::Incomplete,
            repair: Vec::new(),
            summary: vec![reason.into()],
            approve: Vec::new(),
            evidence: serde_json::Value::Null,
        }
    }
}

/// What the hook remembers between events of one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Repair rounds spent since the last user message.
    pub rounds_used: u32,
    /// The working tree of the last check whose result let the agent stop.
    pub last_checked: Option<String>,
    /// HEAD when the session's first user message arrived.
    pub baseline: Option<String>,
    /// Working tree content IDs that already triggered a repair block during
    /// this turn. Used to detect stagnation or oscillation.
    pub rejected_trees: Vec<String>,
}

/// Why nothing was said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The working tree is the one already checked.
    Unchanged,
}

/// How the harness should present a message to the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// The check ran and nothing stands in the way of stopping.
    Verified,
    /// The person has something to look at or decide.
    Attention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Skip(SkipReason),
    /// Keep the agent working; `reason` is all it is told.
    Block {
        reason: String,
    },
    /// Let the agent stop; `message` is for the person.
    Allow {
        message: String,
        tone: Tone,
    },
}

/// The facts of one stop event.
pub struct StopInput<'a> {
    /// The identity of the working tree now.
    pub content_id: &'a str,
    /// Repair rounds allowed per user message.
    pub rounds_limit: u32,
    /// `emi done` with the arguments the check ran with, minus the output
    /// format, so a person can add `--approve ITEM` and run it.
    pub done_command: &'a str,
}

/// Longest repair text sent back: the agent gets the first failing kind of
/// thing (up to five items and a count of the rest), not a report.
const MAX_REPAIR_LINES: usize = 6;
const MAX_LINE_CHARS: usize = 300;

/// The first line of the message shown when repair rounds ran out.
pub const NOT_VERIFIED: &str = "NOT VERIFIED";

fn clip(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_owned();
    }
    let head: String = line.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}…")
}

fn repair_reason(done: &DoneResult, round: u32, limit: u32, repeated: bool) -> String {
    let note = if repeated {
        format!(
            " (repair round {round} of {limit}; this working tree already failed an earlier check)"
        )
    } else {
        format!(" (repair round {round} of {limit})")
    };
    let mut text = format!("EMI: this change is not done{note}. Fix this, then finish again:");
    for line in done.repair.iter().take(MAX_REPAIR_LINES) {
        text.push_str("\n- ");
        text.push_str(&clip(line));
    }
    text
}

fn indented(lines: &[String]) -> String {
    lines.join("\n")
}

fn ask_message(done: &DoneResult, done_command: &str) -> String {
    let mut text = indented(&done.summary);
    text.push_str("\nA person has to decide. To approve, run:\n  ");
    text.push_str(done_command);
    for item in &done.approve {
        text.push_str(" --approve ");
        text.push_str(&shlex::try_quote(item).map_or_else(|_| item.clone(), |q| q.into_owned()));
    }
    text
}

fn rounds_out_message(done: &DoneResult, limit: u32) -> String {
    let mut text = format!(
        "{NOT_VERIFIED}\nEMI sent this change back {limit} time(s) and it still fails. The agent was allowed to stop."
    );
    for line in done.repair.iter().take(MAX_REPAIR_LINES) {
        text.push_str("\nStill failing: ");
        text.push_str(&clip(line));
    }
    text.push('\n');
    text.push_str(&indented(&done.summary));
    text
}

/// The message for a change that was not checked, whatever the reason.
pub fn incomplete_notice(detail: &str) -> String {
    format!("EMI INCOMPLETE: this change was not checked.\n{detail}")
}

fn incomplete_message(done: &DoneResult) -> String {
    incomplete_notice(&indented(&done.summary))
}

fn allow(message: String, tone: Tone) -> Decision {
    Decision::Allow { message, tone }
}

/// Decide a stop event. `run_done` is called only when the working tree
/// differs from the last one checked, and exactly once.
pub fn on_stop(
    state: &State,
    input: &StopInput<'_>,
    run_done: impl FnOnce() -> DoneResult,
) -> (Decision, State) {
    if state.last_checked.as_deref() == Some(input.content_id) {
        return (Decision::Skip(SkipReason::Unchanged), state.clone());
    }
    let done = run_done();
    let mut next = state.clone();
    let blocked = done.action == Action::Repair && state.rounds_used < input.rounds_limit;
    if blocked {
        // not recorded as checked: stopping again on the same tree must
        // run the check again instead of passing unseen
        next.rounds_used += 1;
        let repeated = state.rejected_trees.iter().any(|t| t == input.content_id);
        if !repeated {
            next.rejected_trees.push(input.content_id.to_owned());
        }
        let reason = repair_reason(&done, next.rounds_used, input.rounds_limit, repeated);
        return (Decision::Block { reason }, next);
    }
    next.last_checked = Some(input.content_id.to_owned());
    let decision = match done.action {
        Action::Stop => allow(indented(&done.summary), Tone::Verified),
        Action::AskPerson => allow(ask_message(&done, input.done_command), Tone::Attention),
        Action::Incomplete => allow(incomplete_message(&done), Tone::Attention),
        Action::Repair => allow(
            rounds_out_message(&done, input.rounds_limit),
            Tone::Attention,
        ),
    };
    (decision, next)
}

/// A user message resets the round count and records the session baseline
/// the first time. `head` is HEAD now, when it could be resolved.
pub fn on_user_message(state: &State, head: Option<&str>) -> State {
    let mut next = state.clone();
    next.rounds_used = 0;
    next.rejected_trees.clear();
    if next.baseline.is_none() {
        next.baseline = head.map(str::to_owned);
    }
    next
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    const LIMIT: u32 = 3;

    fn done(action: Action) -> DoneResult {
        DoneResult {
            action,
            repair: vec!["tests/test_a.py::test_x fails".into()],
            summary: vec!["EMI receipt".into(), "Regression: held".into()],
            approve: vec!["old-test:tests/test_a.py".into()],
            evidence: serde_json::Value::Null,
        }
    }

    fn input<'a>(content_id: &'a str) -> StopInput<'a> {
        StopInput {
            content_id,
            rounds_limit: LIMIT,
            done_command: "emi done --repo /r",
        }
    }

    fn state(rounds: u32, last: Option<&str>) -> State {
        State {
            rounds_used: rounds,
            last_checked: last.map(str::to_owned),
            baseline: Some("base".into()),
            rejected_trees: Vec::new(),
        }
    }

    fn stop(state: &State, tree: &str, action: Action) -> (Decision, State, bool) {
        let mut ran = false;
        let (d, s) = on_stop(state, &input(tree), || {
            ran = true;
            done(action)
        });
        (d, s, ran)
    }

    #[test]
    fn an_unchanged_tree_is_skipped_without_running_the_check() {
        for action in [
            Action::Stop,
            Action::Repair,
            Action::AskPerson,
            Action::Incomplete,
        ] {
            let before = state(2, Some("t1"));
            let (d, s, ran) = stop(&before, "t1", action);
            assert_eq!(d, Decision::Skip(SkipReason::Unchanged));
            assert!(!ran);
            assert_eq!(s, before);
        }
    }

    #[test]
    fn repair_with_rounds_left_blocks_and_counts_the_round() {
        for used in 0..LIMIT {
            let (d, s, ran) = stop(&state(used, Some("old")), "t2", Action::Repair);
            assert!(ran);
            let Decision::Block { reason } = d else {
                panic!("expected a block at {used} rounds used")
            };
            assert!(reason.contains("tests/test_a.py::test_x fails"));
            assert!(reason.contains(&format!("repair round {} of {LIMIT}", used + 1)));
            assert_eq!(s.rounds_used, used + 1);
            assert_eq!(s.last_checked.as_deref(), Some("old"));
        }
    }

    #[test]
    fn a_block_sends_back_only_the_repair_lines() {
        let mut result = done(Action::Repair);
        result.repair = (1..=9).map(|n| format!("problem {n}")).collect();
        result.summary = vec!["SECRET-RECEIPT-DETAIL".into()];
        let (d, _) = on_stop(&state(0, None), &input("t"), || result);
        let Decision::Block { reason } = d else {
            panic!("expected a block")
        };
        assert!(!reason.contains("SECRET-RECEIPT-DETAIL"));
        // five items and the line that counts the rest
        assert!(reason.contains("problem 6"));
        assert!(!reason.contains("problem 7"));
    }

    #[test]
    fn a_long_repair_line_is_clipped() {
        let mut result = done(Action::Repair);
        result.repair = vec!["x".repeat(2000)];
        let (d, _) = on_stop(&state(0, None), &input("t"), || result);
        let Decision::Block { reason } = d else {
            panic!("expected a block")
        };
        assert!(reason.chars().count() < 500);
        assert!(reason.ends_with('…'));
    }

    #[test]
    fn repair_with_no_rounds_left_allows_and_says_not_verified_first() {
        let (d, s, _) = stop(&state(LIMIT, None), "t3", Action::Repair);
        let Decision::Allow { message, tone } = d else {
            panic!("expected allow")
        };
        assert_eq!(message.lines().next(), Some("NOT VERIFIED"));
        assert!(message.contains("tests/test_a.py::test_x fails"));
        assert_eq!(tone, Tone::Attention);
        assert_eq!(s.last_checked.as_deref(), Some("t3"));
        assert_eq!(s.rounds_used, LIMIT);
    }

    #[test]
    fn a_limit_of_zero_never_blocks() {
        let zero = StopInput {
            rounds_limit: 0,
            ..input("t")
        };
        let (d, _) = on_stop(&state(0, None), &zero, || done(Action::Repair));
        assert!(
            matches!(d, Decision::Allow { ref message, .. } if message.starts_with("NOT VERIFIED"))
        );
    }

    #[test]
    fn stop_allows_with_the_receipt_and_is_remembered() {
        let (d, s, ran) = stop(&state(1, None), "t4", Action::Stop);
        assert!(ran);
        assert_eq!(
            d,
            Decision::Allow {
                message: "EMI receipt\nRegression: held".into(),
                tone: Tone::Verified
            }
        );
        assert_eq!(s.last_checked.as_deref(), Some("t4"));
        assert_eq!(s.rounds_used, 1);
    }

    #[test]
    fn ask_person_names_the_approval_arguments() {
        let (d, s, _) = stop(&state(0, None), "t5", Action::AskPerson);
        let Decision::Allow { message, tone } = d else {
            panic!("expected allow")
        };
        assert_eq!(tone, Tone::Attention);
        assert!(message.contains("emi done --repo /r --approve old-test:tests/test_a.py"));
        assert!(!message.starts_with("NOT VERIFIED"));
        assert_eq!(s.last_checked.as_deref(), Some("t5"));
    }

    #[test]
    fn an_item_with_spaces_is_quoted_so_it_can_be_pasted() {
        let mut result = done(Action::AskPerson);
        result.approve = vec!["old-test:tests/my test.py".into()];
        let (d, _) = on_stop(&state(0, None), &input("t"), || result);
        let Decision::Allow { message, .. } = d else {
            panic!("expected allow")
        };
        assert!(message.contains("--approve 'old-test:tests/my test.py'"));
    }

    #[test]
    fn incomplete_allows_and_is_never_presented_as_a_pass() {
        let (d, s, _) = stop(&state(0, None), "t6", Action::Incomplete);
        let Decision::Allow { message, tone } = d else {
            panic!("expected allow")
        };
        assert_eq!(tone, Tone::Attention);
        assert!(message.starts_with("EMI INCOMPLETE"));
        assert!(!message.starts_with("NOT VERIFIED"));
        assert_eq!(s.last_checked.as_deref(), Some("t6"));
    }

    #[test]
    fn an_uninterpretable_check_is_incomplete_whatever_the_state() {
        let result = DoneResult::incomplete("emi done printed no receipt");
        let (d, _) = on_stop(&state(LIMIT, None), &input("t"), || result);
        assert!(
            matches!(d, Decision::Allow { ref message, tone: Tone::Attention }
            if message.contains("emi done printed no receipt"))
        );
    }

    #[test]
    fn a_user_message_resets_rounds_and_keeps_the_last_checked_tree() {
        let before = state(3, Some("t1"));
        let after = on_user_message(&before, Some("head"));
        assert_eq!(after.rounds_used, 0);
        assert_eq!(after.last_checked.as_deref(), Some("t1"));
        assert_eq!(after.baseline.as_deref(), Some("base"));
    }

    #[test]
    fn the_first_user_message_records_the_baseline_once() {
        let first = on_user_message(&State::default(), Some("head1"));
        assert_eq!(first.baseline.as_deref(), Some("head1"));
        let second = on_user_message(&first, Some("head2"));
        assert_eq!(second.baseline.as_deref(), Some("head1"));
    }

    #[test]
    fn an_unresolvable_head_records_no_baseline() {
        let s = on_user_message(&State::default(), None);
        assert_eq!(s.baseline, None);
    }

    #[test]
    fn oscillating_or_stagnant_tree_is_flagged_in_repair_reason() {
        let s0 = State::default();
        // First failure on tree A: not repeated
        let (d1, s1, _) = stop(&s0, "treeA", Action::Repair);
        let Decision::Block { reason: r1 } = d1 else {
            panic!("expected block");
        };
        assert!(r1.contains("repair round 1 of 3"));
        assert!(!r1.contains("already failed an earlier check"));
        assert_eq!(s1.rejected_trees, vec!["treeA"]);

        // Second failure on tree B: not repeated
        let (d2, s2, _) = stop(&s1, "treeB", Action::Repair);
        let Decision::Block { reason: r2 } = d2 else {
            panic!("expected block");
        };
        assert!(r2.contains("repair round 2 of 3"));
        assert!(!r2.contains("already failed an earlier check"));
        assert_eq!(s2.rejected_trees, vec!["treeA", "treeB"]);

        // Third failure oscillating back to tree A: repeated!
        let (d3, s3, _) = stop(&s2, "treeA", Action::Repair);
        let Decision::Block { reason: r3 } = d3 else {
            panic!("expected block");
        };
        assert!(
            r3.contains("repair round 3 of 3; this working tree already failed an earlier check")
        );
        assert_eq!(s3.rejected_trees, vec!["treeA", "treeB"]);

        // New user message clears rejected_trees
        let s4 = on_user_message(&s3, Some("head"));
        assert!(s4.rejected_trees.is_empty());
    }
}
