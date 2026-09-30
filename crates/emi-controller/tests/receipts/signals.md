# Verification receipt: rc-signals

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
| R1 | clamp clamps | verified | `tests` | passed |

- Trusted files restored: none
- Held-out files injected: none
- Test inventory: not measured (the contract has no test inventory)
- Shaken reruns: not measured (the contract asks for no shaken reruns)

## Evidence kind

- Catch kind: none
- Structural signals: none
- Pattern signals: none
- Isolation: sandboxed

## Escalations

none

## Scope

Envelope violations: none

- Test-runner configuration added: none (policy: reject)
- Test files outside the trusted set changed: none

## How much the tests pin the change

### Mutation

- State: measured
- Mutants run: 1 of 1 candidates; killed 0, survived 1, timed out 0, errors 0

| File | Line | Function | Original | Mutant |
|---|---|---|---|---|
| `app.py` | 4 | `clamp` | `*` | `/` |

Note: a surviving mutant may be equivalent to the original; treat survivors as places to look, not defects.

### Drift

- State: measured
- Functions: 1 modified, 1 compared on the tests' inputs, 1 with drift

| Function | Input | Old → new |
|---|---|---|
| `app.py` `clamp` | `{"args": [-2], "kwargs": {}}` | `{"returned": 0}` → `{"raised": "ValueError"}` |

Note: inputs come from the test run on the base commit; drift on inputs the tests never pass is not seen.

## Places to look

- Public surfaces: none
- Schema boundaries: none
- Usually changed together, not touched: could not be read (1 commit(s) up to the base in the window; change coupling needs at least 5)
- Importers of what changed: none

## Not verified

| What | State | What it means |
|---|---|---|
| structural gate | not_required | the contract has no structural policy; growth in maintenance burden was not measured |
| test inventory | not_required | the contract has no test inventory; each check passed on its exit code alone |
| shaken reruns | not_required | not requested by the contract; code that depends on how it is tested was not looked for |
| refactor | not_required | not a refactoring task; simplification was not measured |
| dependency pins | not_required | the contract links no dependency directory; there was nothing to pin |
| behaviour outside the tests' inputs | not_measured | the checks exercise only the inputs they contain; other inputs may behave differently |
| performance | not_measured | no check measures speed or resource use |
| security properties beyond the guardrails | not_measured | nothing here looks for vulnerabilities; the guardrails only flag known risky edits |
