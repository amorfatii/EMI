# EMI

Verification of code changes made by AI coding agents: a controller decides,
from evidence it gathered itself, whether a change satisfies its task.

## Language

### Roles

**Controller**:
The party that owns the task contract and issues acceptance decisions.
_Avoid_: supervisor, judge, verifier agent

**Proposer**:
The coding agent, treated as an untrusted source of candidates.
_Avoid_: writer, author, worker

### Task and evidence

**Task contract**:
The authoritative requirements and constraints for one task.
_Avoid_: task record, task file, spec

**Derived contract**:
A task contract EMI builds from the repository when the user supplies none.
_Avoid_: default spec, auto contract

**Baseline**:
The revision a task starts from, against which a candidate is compared.
_Avoid_: base, before, original

**Candidate**:
One immutable source revision submitted for verification.
_Avoid_: diff, patch, attempt output

**Completion claim**:
The proposer's own statement that a task is finished. It is never evidence.
_Avoid_: done, success report, agent summary

**Check definition**:
A controller-owned verification procedure with expected results.
_Avoid_: test command, script

**Check run**:
One execution of a check definition against a candidate.
_Avoid_: test result, log

**Requirement ledger**:
The verification state and evidence for each requirement.
_Avoid_: burden ledger, score report

**Acceptance decision**:
The controller result for a candidate: accepted, rejected, needs_review, incomplete.
_Avoid_: gate verdict, pass, proceed

**Receipt**:
The record of what one acceptance decision rests on and what it did not verify.
_Avoid_: report, summary, proof

**Execution evidence**:
What a check run itself reported about each test it ran, beyond its exit status.
_Avoid_: test output, test log, exit code

**Regression evidence**:
Evidence that behaviour present at the baseline still holds in the candidate.
_Avoid_: old tests pass, no breakage

**Change evidence**:
Evidence that the behaviour the task asked for exists in the candidate.
_Avoid_: feature works, new tests pass

**Proving test**:
A test the candidate adds that fails at the baseline, passes on the candidate, and executes code the candidate changed.
_Avoid_: fail-to-pass test, new test

**Redundant test**:
A test the candidate adds that would not notice the change: it passes at the baseline, or it executes none of the changed code.
_Avoid_: useless test, extra test, low-quality test

**Unproven function**:
A function the candidate changed that no proving test executes.
_Avoid_: untested code, uncovered function, unverified claim

**Approval**:
A person's recorded decision to allow one named review item for one candidate.
_Avoid_: sign-off, override, waiver

**Repair round**:
One cycle in which the controller returns a failed acceptance decision to the proposer and receives a new candidate.
_Avoid_: retry, attempt, iteration

**Assurance**:
The statement of which kind of evidence, and which execution setting, an acceptance decision rests on.
_Avoid_: confidence, trust score

**Cooperative verification**:
Verification run where the proposer could alter the controller's inputs. It catches a careless proposer, not a cheating one.
_Avoid_: local mode, soft mode

**Enforced verification**:
Verification run where the proposer cannot reach the task contract, the checks or the controller's records.
_Avoid_: CI mode, strict mode

**Deterministic acceptance**:
The property that the same candidate under the same task contract always yields the same acceptance decision.
_Avoid_: deterministic agent, reproducible generation

### Measurement

**Structural burden**:
A description of how large, branchy, repeated and tangled code is. It is not a quality score.
_Avoid_: code health, quality score, maintainability index

**Measurement contract**:
The adapter, runtime, parser and scope rules that define a measurement.
_Avoid_: profile string

**Snapshot input**:
The exact source, configuration and inventory used by a measurement.
_Avoid_: checkout, tree

**Gate policy**:
The versioned rules, allowances and uncertainty treatment for a decision.
_Avoid_: threshold, allowance
