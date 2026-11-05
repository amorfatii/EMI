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
A test the candidate adds, or an old test it edited, that fails at the baseline and passes on the candidate. One that only errored at the baseline must also execute code the candidate changed.
_Avoid_: fail-to-pass test, new test

**Redundant test**:
A test the candidate adds that does not show the change: it passes at the baseline, or it errored there and executes none of the changed code. It may still guard behaviour the change must keep, so it is stated, not removed. A companion test is not redundant.
_Avoid_: useless test, extra test, low-quality test

**Companion test**:
An added case of a parametrized test that passes at the baseline while another case of the same test function is a proving test.
_Avoid_: redundant case, extra parameter

**Exercised function**:
A function the candidate changed whose changed lines a proving test executes. Exercised is not proved.
_Avoid_: proven function, verified function

**Unexercised function**:
A function the candidate changed that no proving test executes.
_Avoid_: unproven function, untested code, uncovered function

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
The property that the same candidate under the same inputs (configuration, laws, environment, pins, intent) always yields the same decision on the same evidence.
_Avoid_: deterministic agent, reproducible generation

### Laws

**Law**:
A test the person owns, in the laws directory, that says what the code must do. Every check runs it as the baseline has it.
_Avoid_: spec, contract test, invariant file

**Open law**:
A law that fails at the baseline: what the task must bring about.
_Avoid_: failing law, todo test

**Law-checked function**:
A changed function whose changed lines a passing law executes.
_Avoid_: verified function, proven function

**Proposal**:
A law file the proposer added or changed without an approval. It counts for nothing until a person approves it.
_Avoid_: draft law, suggested spec

**Stand-in**:
A trivial body for a function (returns its argument, returns None, returns its annotation's default, raises) that a law worth the name must reject.
_Avoid_: mock, stub

**Vacuous law**:
A proposed law that still holds when a function it exercises is replaced by a stand-in.
_Avoid_: weak law, useless law

### Determinism

**Pins**:
What every test run is held to, the same at the baseline and on the candidate: the hash seed, the time zone, the locale, `random` seeded per test, Hypothesis derandomized.
_Avoid_: seed, fixture, environment

**Quarantined test**:
A test whose outcome differed between the baseline's two pinned runs. It is regression evidence for neither side.
_Avoid_: flaky test, ignored test

**Nondeterministic outcome**:
A candidate failure, or a broken law, that passed when the same run was repeated under the same pins. It makes the check incomplete; it is never cleared as a pass.
_Avoid_: flake, intermittent failure

**Inputs** (of a decision):
The baseline, the candidate's tree, the configuration and pins, the environment, the laws, the intent, the setting, and the versions of EMI and its policy. `input_id` is their digest; `decision_id` is the digest of the receipt without where it ran.
_Avoid_: context, cache key

**Replay**:
Computing a receipt again from nothing cached, to see whether the decision id is the same.
_Avoid_: rerun, retry

### Intent

**Preserve intent**:
The person's statement that a change must keep the behaviour of the functions it modifies.
_Avoid_: refactor mode, equivalence mode

**Divergence**:
An input on which the candidate's version of a function answers differently from the baseline's, or no longer finishes in time.
_Avoid_: drift, regression, behaviour change
