# Verification receipt: rc-accepted

## Decision

| | |
|---|---|
| Decision | **accepted** |
| Candidate | `<sha>` |
| Content verified | `<sha>` |
| Contract | `<digest>` (revision 1) |
| Recorded at | <time> |

Reasons:

- every requirement verified by its frozen checks; all gates pass

## What was verified

| Requirement | Acceptance | State | Check | Check status |
|---|---|---|---|---|
| R1 | add(1, 2) == 3 | verified | `unit` | passed |

- Trusted files restored: not measured (the contract has no verification section)
- Held-out files injected: not measured (the contract has no verification section)
- Test inventory: not measured (the contract has no verification section)
- Shaken reruns: not measured (the contract has no verification section)

## Evidence kind

- Catch kind: none
- Structural signals: none
- Pattern signals: none
- Isolation: sandboxed

## Escalations

none

## Scope

Envelope violations: none

- Test-runner configuration added: none (policy: review, as the contract has no verification section)
- Test files outside the trusted set changed: not measured (the contract has no verification section)

## How much the tests pin the change

### Mutation

not measured (the contract does not set verification.mutation)

### Drift

not measured (the contract does not set verification.drift)

## Places to look

- Public surfaces: none
- Schema boundaries: none
- Usually changed together, not touched: could not be read (1 commit(s) up to the base in the window; change coupling needs at least 5)
- Importers of what changed: none

## Not verified

| What | State | What it means |
|---|---|---|
| verification | not_required | the contract has no verification section; trusted tests were not restored and no held-out test ran |
| test inventory | not_required | the contract has no test inventory; each check passed on its exit code alone |
| shaken reruns | not_required | not requested by the contract; code that depends on how it is tested was not looked for |
| refactor | not_required | not a refactoring task; simplification was not measured |
| dependency pins | not_required | the contract links no dependency directory; there was nothing to pin |
| mutation | not_required | not requested by the contract; the tests' strength on the changed lines is unknown |
| drift | not_required | not requested by the contract; whether the change answers differently for the tests' inputs is unknown |
| behaviour outside the tests' inputs | not_measured | the checks exercise only the inputs they contain; other inputs may behave differently |
| performance | not_measured | no check measures speed or resource use |
| security properties beyond the guardrails | not_measured | nothing here looks for vulnerabilities; the guardrails only flag known risky edits |
