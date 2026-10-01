# Plan: when the agent says done, EMI says what was proven

This plan replaces "EMI production readiness plan" (2026-10-01, kept in
`~/Projects/emi-v0.4/reports/`). That report hardened the acceptance decision
for an unattended merge lane. Its audit facts stand; its direction does not.
The vocabulary used here is defined in `CONTEXT.md`; the two decisions that
contradict the report are `docs/adr/0001` and `docs/adr/0002`.

## Why the direction changed

- The report contains no work a user sees. All ten tickets concern whether a
  decision can be trusted, none whether anyone notices a difference.
- EMI is voluntary today: a proposer reaches it only through MCP tools it
  chooses to call. Nothing runs when it makes a completion claim.
- Every decision needs a task contract someone wrote by hand. Real use starts
  from "add feature X" with no contract.
- The lab's evidence: static scores do not beat lines of code (S1, S2a, S5a,
  S5r, G2); pattern blocking costs 8.5 false blocks per 100 pull requests
  (GV2); agents weaken tests no more than people do (S7); real agents made no
  bypass attempts (S12); but 6 of 19 runs passed by editing tests (S11).

## The promise

**When your agent says done, EMI tells you what was actually proven.**

"Deterministic" means deterministic acceptance: the same candidate under the
same task contract always yields the same decision. The proposer is an
untrusted source of candidates inside a bounded loop. No language model takes
part in a decision.

## Settled design

| # | Decision |
|---|---|
| 1 | EMI acts first inside a coding harness at the completion claim, and second as a required status in CI. |
| 2 | The headline failure is a false completion claim. Test bloat is second; token waste is treated as a consequence of rounds and tests. |
| 3 | With no contract from the user, EMI builds a derived contract from the repository. |
| 4 | A one-time `emi init` detects the test command; the user confirms; a small config file is committed. |
| 5 | A harness hook (Claude Code and opencode first) runs the check when the proposer tries to finish, only if the files changed since the last check. |
| 6 | The baseline is where the branch split from the default branch, or the commit the session started on when working directly on the default branch. |
| 7 | The receipt has two parts: regression evidence (hard) and change evidence (a level, counted in changed functions). |
| 8 | A test the candidate adds stays only if it is a proving test. A redundant test goes back to the proposer for removal. The stricter mutation rule is opt-in. |
| 9 | A changed or deleted old test is shown to a person for one approval; in CI it is a review item. |
| 10 | Three repair rounds by default, reset on each user message. When they run out the proposer stops and the person sees the receipt marked NOT VERIFIED. |
| 11 | Tokens are not measured in the product. `max_tokens` leaves the contract. Feedback to the proposer names the first failing thing only. |
| 12 | Cooperative verification on the user's machine; enforced verification in CI. Every receipt states which. |
| 13 | EMI reports a status and never merges (ADR 0001). |
| 14 | Python with pytest first, then JavaScript/TypeScript. |
| 15 | Burden features are frozen; the Python tool is archived; the golden check stays as a regression test (ADR 0002). |

## Architecture

```
harness (Claude Code / opencode)
   │ stop event, user-message event
   ▼
emi hook ──────── state: repair rounds, last checked content id
   │ runs
   ▼
emi done ──────── reads .emi/config.json, resolves the baseline
   ├── regression evidence   derived contract → emi-controller verify
   │                         (trusted tests restored, inventory compared)
   ├── changed old tests     review items, cleared by an approval
   └── change evidence       emi-proving: proving / redundant tests,
                             proven / unproven functions
   ▼
receipt (JSON or text) + exit code
```

New crates, each with one concern:

- `emi-done`: config, baseline resolution, the derived contract, regression
  evidence, changed old tests and approvals, the receipt and the action table.
- `emi-proving`: which added tests are proving or redundant, and which changed
  functions a proving test executes.
- `emi-hook`: harness adapters, repair-round state, installation.

They reuse `emi-controller` (verification), `emi-diff` (snapshots, merge base,
changed functions), `emi-profile` (coverage, sandboxed runs), `emi-exec` and
`emi-git`. No second process executor, hasher or JSON encoder.

### Interface 1: `.emi/config.json`

Committed. In enforced verification it is read from the baseline, never from
the candidate.

```json
{
  "schema": "emi-config-0.1",
  "language": "python",
  "source": "src",
  "default_branch": "main",
  "tests": {"command": ["python", "-m", "pytest", "-q"], "globs": ["tests/*"]},
  "link": [".venv"],
  "repair_rounds": 3,
  "on_unproven": "report"
}
```

`on_unproven` is `report` (default: unproven functions are stated, the
proposer may stop) or `repair` (they send the proposer back once). The default
follows decision 7; the experiment measures both.

### Interface 2: `emi done`

```
emi done [--repo .] [--baseline REF] [--setting cooperative|enforced]
         [--format json|text] [--approve ITEM]...
```

| Exit | Action | Meaning |
|---|---|---|
| 0 | `stop` | Regression evidence holds, no redundant test, no unapproved changed old test. Change evidence is whatever the receipt says. |
| 3 | `repair` | Something the proposer can fix: regression broken, a redundant test, or unproven functions under `on_unproven: repair`. |
| 4 | `ask_person` | Only a person can decide: an unapproved changed or deleted old test. |
| 5 | `incomplete` | EMI could not measure (environment fault, unsupported layout). Never reported as a pass. |
| 2 | — | Usage or configuration error. |

The top-level field is an `action`, not a verdict: the two evidence parts are
the truth, and "stop" does not mean "correct".

### Interface 3: the receipt (`emi-done-receipt-0.1`)

```json
{
  "schema": "emi-done-receipt-0.1",
  "repo": "/abs/path",
  "baseline": {"sha": "…", "source": "merge-base:main | session | explicit"},
  "candidate": {"id": "worktree | <sha>", "content_id": "<tree sha>"},
  "assurance": {"setting": "cooperative | enforced", "isolation": "sandboxed | unsandboxed (…)"},
  "regression": {
    "status": "held | broken | unknown",
    "tests_run": 212,
    "failed": ["tests/test_a.py::test_x"],
    "missing": ["tests/test_b.py::test_y"],
    "reasons": ["…"]
  },
  "changed_old_tests": [
    {"item": "old-test:tests/test_a.py", "path": "tests/test_a.py",
     "kind": "modified | deleted", "restored_result": "passes | fails | unknown",
     "approved": false}
  ],
  "change": {
    "status": "proven | partly_proven | unproven | none | unknown",
    "functions": [{"file": "src/a.py", "function": "f", "line": 10,
                   "status": "proven | unproven | unknown"}],
    "proving_tests": [{"id": "tests/test_a.py::test_new", "baseline_result": "failed | errored"}],
    "redundant_tests": [{"id": "tests/test_a.py::test_extra"}],
    "unknown_tests": [{"id": "…", "reason": "…"}],
    "not_measured": ["…"]
  },
  "action": "stop | repair | ask_person | incomplete",
  "repair": ["one short instruction: the first failing thing"],
  "not_verified": ["everything this receipt does not establish"]
}
```

Rules that follow from `CLAUDE.md`: an unresolved input is `unknown`, never a
pass; a failed measurement is an error or an explicit unknown, never an empty
list; `not_verified` is always present.

### The action table

Evaluated top to bottom; the first match wins.

| Condition | Action |
|---|---|
| Any part could not be measured for an environment reason | `incomplete` |
| `regression.status` is `broken` | `repair` |
| `change.redundant_tests` is not empty | `repair` |
| `on_unproven` is `repair` and a function is `unproven` | `repair` |
| A changed old test or changed runner configuration is not approved | `ask_person` |
| Otherwise | `stop` |

How `regression.status` is computed decides whether decision 9 works, so it
is fixed here:

- Old tests and runner configuration the candidate did not touch are the
  regression evidence. A failure among them is `broken`.
- An old test file the candidate modified or deleted is run in its baseline
  form. If that form fails, the failure is **not** `broken`: it is the
  question for the person ("this old test fails as it was; the proposer
  rewrote it"). The entry carries `restored_result: fails`.
- Once the person approves the item, the candidate's version of that file is
  used instead of the restored one, and its tests count as regression
  evidence like any other.
- A modified runner configuration file is handled the same way: restored for
  the run, listed as an item `runner-config:PATH`, approved by a person.
  Editing it never makes the status `broken` by itself.

### The hook

- **Stop event:** if the content id equals the last checked one, do nothing.
  Otherwise run `emi done`. On `repair` with rounds left, block the stop and
  give the proposer the `repair` lines. On `stop`, `ask_person`,
  `incomplete`, or no rounds left, let it stop and show the person the text
  receipt; when rounds ran out, the first line is `NOT VERIFIED`.
- **User-message event:** reset the round count; on the first one of a
  session, record the session baseline.
- State lives outside the repository, under the user's state directory.

## Work

### Wave 1 (four packages, in parallel)

| Package | Branch | Scope |
|---|---|---|
| A. Done check | `feat/done-check` | Crate `emi-done`; `emi init`; `emi done` with config, baseline, derived contract, regression evidence, changed old tests and approvals, receipt, action table. `change` is `unknown` with a `not_measured` reason until package B is wired in. |
| B. Proving tests | `feat/proving` | Crate `emi-proving`: added tests classified as proving or redundant by running them against the baseline; proven and unproven functions from a coverage run of the proving tests. Returns the receipt's `change` object. |
| C. Harness hook | `feat/hook` | Crate `emi-hook`; `emi hook` for Claude Code and an opencode plugin; round state; content-id skip; installation. Built against interface 2 and 3, tested with a stand-in `emi done`. |
| D. Verdict correctness | `fix/verdict` | From the report: strict contract and reducer (AX1), complete runner evidence for skips, expected failures, empty suites and truncated output (AX5). Remove `max_tokens`. The failing `trend.rs` date test. The repository name in the README. Each concern is first confirmed with a reproducer or closed as already met. |

### Wave 2

- Integrate the four branches; wire `emi-proving` into `emi done`; an
  end-to-end test of the loop with a scripted proposer.
- `docs/done.md`, and the README leads with the promise.
- Enforced verification: `emi done --setting enforced` in CI, config read
  from the baseline, published as a status (`action.yml`).
- Pre-register the experiment in `emi-lab`.

### Later, CI only

Isolation before every execution (AX2), sealed inputs (AX3), durable runs and
budgets (AX4).

### Cut

The merge broker (AX7), the literal-fitting oracle (AX8), and the recovery of
the S13–S18 artifacts.

## How we know it worked

Pre-registered in `emi-lab` before any outcome data:

- **Tasks:** 100 from SWE-bench Verified (Python, not used in earlier
  studies), each run with and without the hook, same model and harness. EMI
  gets the repository only, never the task's hidden tests.
- **Main number:** how often the proposer makes a completion claim while the
  hidden tests fail.
- **Worth it:** that number falls by at least a third, the solve rate does not
  fall, and tokens per solved task rise by no more than 20%.
- **Real use:** about 10 tasks from the owner's own repositories, judged by
  the owner.
- **A miss** means rethinking the hook, not shipping it.
