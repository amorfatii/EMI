//! Old against new outcomes, input by input. An input counts as compared
//! only when both sides produced a comparable outcome and the base side gave
//! the same outcome twice; everything else is counted by why it was not
//! compared, never folded into "no drift".

use emi_kernel::json::canonical;
use serde_json::{Value, json};

/// Drifts listed per function; the rest are counted.
pub const MAX_DRIFTS: usize = 10;

/// What happened to one function's inputs.
#[derive(Default)]
pub struct Tally {
    pub compared: u64,
    pub drifts: Vec<Value>,
    pub more_drifts: u64,
    pub timeouts: u64,
    pub unencodable_result: u64,
    /// The base side answered differently on two calls with the same input.
    pub unstable: u64,
    /// Inputs the replay ran out of time for, or that a side did not answer.
    pub not_replayed: u64,
}

fn canon(v: &Value) -> String {
    let mut s = String::new();
    canonical(v, &mut s);
    s
}

fn has(outcome: &Value, key: &str) -> bool {
    outcome.get(key).is_some()
}

/// The first outcome of each repeated call list, and whether all agree.
fn settled(calls: &Value) -> Option<(&Value, bool)> {
    let calls = calls.as_array()?;
    let first = calls.first()?;
    let same = calls.iter().all(|c| canon(c) == canon(first));
    Some((first, same))
}

impl Tally {
    fn add(&mut self, input: &Value, base: &Value, head: &Value) {
        let (Some((old, stable)), Some((new, _))) = (settled(base), settled(head)) else {
            self.not_replayed += 1;
            return;
        };
        let either = |k: &str| has(old, k) || has(new, k);
        // an input the baseline answered and the candidate did not finish:
        // the change made the function run past its bound, a divergence
        if stable && has(new, "timeout") && !has(old, "timeout") {
            self.compared += 1;
            self.push(input, old, new);
        } else if either("timeout") {
            self.timeouts += 1;
        } else if either("unencodable_result") {
            self.unencodable_result += 1;
        } else if !stable {
            self.unstable += 1;
        } else {
            self.compared += 1;
            if canon(old) != canon(new) {
                self.push(input, old, new);
            }
        }
    }

    fn push(&mut self, input: &Value, old: &Value, new: &Value) {
        if self.drifts.len() < MAX_DRIFTS {
            self.drifts
                .push(json!({"input": input, "old": old, "new": new}));
        } else {
            self.more_drifts += 1;
        }
    }

    pub fn drift_count(&self) -> u64 {
        self.drifts.len() as u64 + self.more_drifts
    }
}

/// Compare the replay outcomes of one function on the base and head side.
pub fn compare(inputs: &[Value], base: &[Value], head: &[Value]) -> Tally {
    let mut t = Tally::default();
    for (i, input) in inputs.iter().enumerate() {
        match (base.get(i), head.get(i)) {
            (Some(b), Some(h)) => t.add(input, b, h),
            _ => t.not_replayed += 1,
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::compare;

    #[test]
    fn outcomes_are_compared_only_when_both_sides_answered_stably() {
        let inputs = [json!(1), json!(2), json!(3), json!(4), json!(5)];
        let base = [
            json!([{"returned": 0}, {"returned": 0}]),
            json!([{"returned": 1}, {"returned": 1}]),
            json!([{"timeout": 2.0}, {"timeout": 2.0}]),
            json!([{"returned": 0.5}, {"returned": 0.7}]),
        ];
        let head = [
            json!([{"raised": "ValueError"}]),
            json!([{"returned": 1}]),
            json!([{"returned": 1}]),
            json!([{"returned": 0.5}]),
            json!([{"returned": 5}]),
        ];
        let t = compare(&inputs, &base, &head);
        assert_eq!((t.compared, t.drift_count()), (2, 1));
        assert_eq!(t.drifts[0]["new"], json!({"raised": "ValueError"}));
        assert_eq!((t.timeouts, t.unstable, t.not_replayed), (1, 1, 1));
    }

    #[test]
    fn a_call_that_now_runs_past_its_bound_is_a_divergence() {
        let inputs = [json!(1), json!(2)];
        let base = [
            json!([{"returned": 1}, {"returned": 1}]),
            json!([{"timeout": 2.0}, {"timeout": 2.0}]),
        ];
        let head = [json!([{"timeout": 2.0}]), json!([{"timeout": 2.0}])];
        let t = compare(&inputs, &base, &head);
        assert_eq!((t.compared, t.drift_count(), t.timeouts), (1, 1, 1));
        assert_eq!(t.drifts[0]["new"], json!({"timeout": 2.0}));
    }
}
