---
status: accepted
---

# Laws are the person's and are restored for every check

The evidence that a change does what was asked came from tests the
proposer wrote itself: a test that fails before and passes after shows the
test discriminates the change, not that it says what the person wanted. We
decided to add laws: pytest files in a directory the person owns
(`.emi/laws`), which every check lays over the candidate as the baseline
has them. A law that held must keep holding (a `broken` law is a repair),
and an open law must come to hold (an `unmet` law is a repair).

## Considered options

- **Reject every edit to the laws.** Rejected: an agent that finds a law
  wrong, or drafts one, should be able to propose it. Restoring the laws
  makes an edit harmless; the person approves a change like any changed
  old test (`law:PATH`). An optional lock refuses edits in the harness as
  fast feedback.
- **Agent-written proofs per changed function.** Rejected: in Python no
  checker makes a "proof" more than another test, and a mandatory artifact
  per function costs repair rounds. The proposer's proving tests are its
  witnesses already.
- **Mutation testing of the candidate against the laws as a gate.**
  Rejected as a gate: equivalent mutants make zero survivors unreachable,
  and mutants of a trivial candidate do not reveal that it is trivial.

## Consequences

- A proposed law is checked for vacuity before a person approves it:
  trivial stand-in bodies for each function it exercises (returns its
  argument, returns None, returns the annotation's default, raises). A law
  that still holds for one is reported.
- The laws' digest is one of a decision's inputs.
- Counterexamples (Hypothesis's falsifying example, or the assertion) are
  what the proposer is told.
