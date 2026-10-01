---
status: accepted
---

# No language model takes part in an acceptance decision

EMI exists because a proposer's completion claim is not evidence. A second
model judging that claim would be a second claim: it could be wrong in the
same ways, it would not give the same answer twice, and nobody could replay
it. So every decision and every receipt is computed from things EMI ran and
observed itself (test outcomes, test inventories, coverage, diffs), and the
same candidate under the same task contract always yields the same decision.

## Considered options

- **A model checks each sentence of the completion claim.** Rejected: it
  puts an unverifiable judge inside the verdict, and the unit of change
  evidence became the changed function instead, which is mechanical.
- **A model reviews the diff as an extra gate.** Rejected for decisions.
  A model may still draft tests or contracts for a person to approve, and
  may investigate a conflict, but its output is never an input a decision
  rests on.

## Consequences

- What EMI cannot observe mechanically is stated in `not_verified`, not
  estimated.
- Requirements written only as prose are not verified by EMI.
