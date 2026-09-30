# Verification receipt: rc-rejected

## Decision

| | |
|---|---|
| Decision | **rejected** |
| Candidate | `<sha>` |
| Content verified | `<sha>` |
| Contract | `<digest>` (revision 1) |
| Recorded at | <time> |

Reasons:

- 1 change(s) outside the task envelope
- test-runner configuration added (it runs in every check, and restoring cannot undo it): conftest.py

## What was verified

| Requirement | Acceptance | State | Check | Check status |
|---|---|---|---|---|
| R1 | add adds | verified | `visible` | passed |
| R1 | add adds | verified | `hidden` | passed |

- Trusted files restored: none
- Held-out files injected: `tests/test_hidden.py`
- Test inventory: not measured (the contract has no test inventory)
- Shaken reruns: not measured (the contract asks for no shaken reruns)

## Evidence kind

- Catch kind: structural
- Structural signals: `envelope`, `verification.runner_config`
- Pattern signals: none
- Isolation: sandboxed

## Escalations

none

## Scope

Envelope violations:

| Path | Change | Reason |
|---|---|---|
| `conftest.py` | add | outside the permitted task scope |

- Test-runner configuration added: `conftest.py` (policy: reject)
- Test files outside the trusted set changed: none

## How much the tests pin the change

### Mutation

not measured (the contract does not set verification.mutation)

### Drift

not measured (the contract does not set verification.drift)

## Places to look

- Public surfaces: `pytest_runtest_makereport` in `conftest.py` (added)
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
| mutation | not_required | not requested by the contract; the tests' strength on the changed lines is unknown |
| drift | not_required | not requested by the contract; whether the change answers differently for the tests' inputs is unknown |
| behaviour outside the tests' inputs | not_measured | the checks exercise only the inputs they contain; other inputs may behave differently |
| performance | not_measured | no check measures speed or resource use |
| security properties beyond the guardrails | not_measured | nothing here looks for vulnerabilities; the guardrails only flag known risky edits |
