# Supervised tasks (`emi task`)

An agent that grades its own work can pass any bar it sets. A supervised
task separates the roles:
- **The controller** freezes the objective, the requirements, the checks
  that prove them, the scope the change may touch, budgets, and the
  structural policy. It does this before the agent starts, in an
  **authority directory outside the repository**.
- **The agent** submits candidates. EMI verifies them by running the frozen
  checks itself, and only a verified commit can complete the task.

```sh
emi task create   --authority ~/.emi-authority --spec task.json
emi task verify   --authority ~/.emi-authority --task fix-add --worktree   # or --candidate REF
emi task status   --authority ~/.emi-authority --task fix-add
emi task complete --authority ~/.emi-authority --task fix-add --candidate HEAD
emi task revise   --authority ~/.emi-authority --task fix-add --changes '{"objective": "..."}' --actor lead --reason "scope grew"
emi task approve  --authority ~/.emi-authority --task fix-add --candidate HEAD --item 'guard:ci.edited:.github/workflows/test.yml' --actor lead --reason "pin bump requested in the issue"
emi task escalate --authority ~/.emi-authority --task fix-add --candidate HEAD --reason "test_two asserts the opposite of the objective"
emi task install-hook --authority ~/.emi-authority --task fix-add --repo .    # pre-push: completed commits only
```

Every verify also runs the **guardrails** (docs/guardrails.md):
- **Blocking violations reject the candidate:** by default only edits outside the envelope. Since v0.4, emi-lab GV2 puts deleted or skipped existing tests and CI or vendored-code edits at review tier: the events are real, but blocking them was rarely warranted. Restored trusted tests reject a hidden bug regardless.
- **Review-tier violations make it `needs_review`:**
  - secret-like files (`.env`, `*.pem`, …);
  - fewer tests or assertions, test configuration changes, skips on new tests, assertions that cannot fail;
  - new suppressions, pasted duplicates, lockfiles, growth.
- **Configuration:** the spec's `"guardrails"` holds a policy object (tiers, allowed classes, budgets); `false` opts out.

Exit codes: `verify` returns 0 when accepted and 3 otherwise; `complete`
and `hook-check` return 0 or 3. Agents get the same through MCP as
`emi_task_verify` and `emi_task_status` (docs/agents.md).

## The spec

`emi task draft --target FILE --objective TEXT [--held-out DEST=FILE] [--profile profile.json] -- TEST COMMAND`
writes a spec like the one below: the existing tests and the test-runner
configuration trusted, the targets, their neighbours and new tests
permitted, runner configuration protected, checks sandboxed without
network. With a saved profile it warns where the targets' tests are weak
or missing (docs/profile.md). `emi task refactor-spec` does the same for a
simplification.

```json
{
  "task_id": "fix-add", "repo": ".", "baseline": "HEAD",
  "objective": "make add() add",
  "requirements": [{"id": "R1", "acceptance": "add(1, 2) == 3", "checks": ["unit"]}],
  "checks": {"unit": {"command": ["python3", "-m", "pytest", "-q", "tests/test_add.py"],
                      "success": {"exit_code": 0}, "timeout_s": 120}},
  "envelope": {"permitted_paths": ["src/*"], "protected_paths": ["tests/*", "pyproject.toml"]},
  "structural": {"probe": "python", "source": "src", "allowance": 10, "headroom": 50},
  "execution": {"isolation": "best", "network": false, "link": [".venv"]},
  "warning_policy": {"treatment": "review"},
  "budgets": {"max_retries": 5, "max_time_s": 1800}
}
```

- **The contract.** It is `emi-task-contract-0.1`, the oracle's schema,
  content-addressed by `contract_id`. Editing a frozen contract is detected
  ("tampered"). A revision needs an actor and a reason, gets a new id, and
  voids earlier acceptances. Every revised section is validated as at
  creation, requirements must still bind to existing checks, and the ids
  derived from the structural policy follow it; identity, baseline and
  derived fields cannot be revised. A contract with no requirements never
  reduces to `accepted`.
- **Limits.** Check and inventory `timeout_s` must be in (0, 604800]
  seconds. `execution` accepts only `isolation` (`best`, `bwrap`, `none`),
  `network` (a boolean), `link` (relative paths) and `env` (strings);
  anything else is refused rather than read as the default.
- **Checks.** Commands are argv lists, never a shell. They run through
  `emi-exec` in a fresh snapshot of the candidate (a commit's raw blobs, or
  the working tree as a commit would contain it). By default they run under
  bubblewrap, with a PID namespace, a read-only root, no network and a
  verified cleanup. `link` symlinks ignored dependency directories (a
  virtualenv, `node_modules`) into the snapshot. A check passes when it
  exits with `success.exit_code`.
- **Pinned dependencies.** Linked directories are outside the candidate's
  tree, yet the checks run the tools inside them, so an agent could edit
  its test runner there without touching a tracked file. At freeze the
  contract records a digest of each linked directory (`dependencies`:
  every path, file content, executable bit and link target). Each
  verification recomputes them before and after the checks run; any
  difference makes the decision `incomplete`. To accept a changed
  environment (a new package the fix needs), revise the `execution`
  section, which re-pins the directories as they are then.
- **Gates:**
  - *Envelope:* every changed path (from the baseline, renames split into
    delete and add) must match `permitted_paths`, avoid
    `excluded_paths`/`protected_paths`, and use a permitted change kind.
    Protecting the tests keeps an agent from weakening them.
  - *Structural:* EMI's own gate from baseline to candidate. A block
    rejects. A warn is treated per `warning_policy`: `blocking` rejects,
    `review` gives needs-review, `advisory` passes.
  - *Verification* (below): trusted tests restored, held-out tests run,
    test inventory compared.

## What a verification runs on

**Reading the repository.** The agent can write the repository's `.git/config`, `.git/info/attributes` and refs, and git would let those run programs (filters, textconv drivers, fsmonitor, hooks) or change what a read returns (a `-diff` attribute makes test files "binary", a replace ref swaps an object). Every git call EMI makes switches those off, and every read that compares revisions goes through a clean view: a scratch bare repository that borrows the repository's objects and nothing else.

**The measured tree.** Every verification reads one commit. For `--candidate` it is the submitted commit. For `--worktree`, the working tree (tracked and untracked-but-not-ignored files) is first frozen, byte for byte and without running any filter, into a commit inside the clean view, leaving the repository's index, HEAD, refs and object store alone; the decision names it as `content_id`. The envelope, the guardrails, the structural and refactor gates and every check run all read that commit, so an edit made while verification runs cannot show two gates two different trees. No check ever runs in the measured copy, so the gates see exactly what was submitted, whatever a check did at run time.

**The check runs.** Each check, and the test inventory, runs in its own fresh copy:
- trusted files are restored and mounted **read-only**, along with the held-out files;
- restoring and injecting never follow links the candidate planted;
- the authority directory and the source repository are replaced by empty
  directories inside the sandbox, so a check cannot read contracts,
  held-out files of other tasks, evidence, or the agent's live tree.
  Linked dependency directories are mounted back read-only. A virtualenv
  with an *editable* install of the repository points at the live tree,
  not the candidate, so its imports fail here instead of silently testing
  the wrong code: install non-editable, or set `PYTHONPATH` through
  `execution.env`.

**Isolation fails closed.** Checks run under bubblewrap:
- **bubblewrap unavailable:** the runs cannot count, and the decision is `incomplete`;
- **the sandbox fails to start the command:** an environment fault (`incomplete`), not the candidate's failure;
- **`"execution": {"isolation": "none"}`:** runs without a sandbox, and the decision's `assurance` says `unsandboxed`.

**What an acceptance means.** The candidate passed the frozen checks under the recorded environment:
- the tests test what they test, and no more;
- held-out tests run in the same interpreter as the candidate's code, so they catch special-casing but are not hidden from code determined to inspect them;
- a local pre-push hook is a convenience; enforce completion in CI or repository policy.

## Verification that tampering cannot pass

Detectors such as the guardrails can miss things. For a supervised task,
the stronger move is to make weakened or gamed tests unable to pass. The
optional `verification` section does this the way benchmarks grade:

```json
"verification": {
  "trusted": ["tests/*", "conftest.py"],
  "held_out": {"tests/test_acceptance_hidden.py": {"file": "/secure/acceptance.py"}},
  "inventory": {"command": ["python3", "-m", "pytest", "--collect-only", "-q"], "format": "pytest"}
}
```

- **`trusted`:** existing files matching these globs are restored to their
  baseline version before the checks run.
  - An agent that guts, skips or deletes a trusted test is verified against
    the original, so the change fails like the bug it hides.
  - New files the agent adds (new regression tests) still run.
  - The candidate still carries its edit to a trusted file, so such a
    candidate is `needs_review` even when the checks pass.
  - Restoring cannot undo a file the baseline did not have. A candidate
    that **adds** test-runner configuration (`conftest.py`, `pytest.ini`,
    `sitecustomize.py`, jest/vitest configs, … anywhere in the tree) is
    `needs_review`: a new `conftest.py` can rewrite every test's outcome.
  - Changing what a trusted test expects takes a contract revision.
  - A candidate that modifies or deletes a test file the `trusted` globs miss (EMI's test conventions: test directories, `test_*.py`, `*_test.go`, `*.test.*` and the like) is `needs_review`. Such a file runs as the candidate left it, so a narrow list must not turn a weakened test into a silent acceptance.
- **`held_out`:** files frozen into the contract (outside the repository)
  and written into the workspace before the checks run. They catch code
  that special-cases the visible tests. Keep the authority directory out of
  the agent's reach.
- **Without an inventory**, a check passes on its exit code alone: code
  that ends the test process with status 0 before the tests run would
  pass. `task create` warns, every decision lists it under `limits`, and
  the `code.early_exit` guardrail flags the usual ways to do it.
- **`inventory`:** a command that lists test ids (one per line; `pytest`
  keeps `path::name` lines). It runs on the baseline and on the verified
  workspace. A baseline test missing from the candidate rejects it, which
  catches tests dropped through configuration (`collect_ignore`,
  `--deselect`, a runner option) rather than by editing test files.

- **`shake`:** `{"rounds": 1}` (up to 3). Every check that passed runs again, and the task needs review if a rerun fails. Each rerun changes three things:
  - the trusted Python tests' functions and classes are renamed;
  - the workspace sits at another path;
  - `PYTHONHASHSEED` changes, unless the contract fixes it.

  Honest code cannot tell these runs apart. Code that depends on which test calls it, or where it runs, can.
  - **Names that stay:** tests are renamed only in test modules (`test_*.py`, `*_test.py`, `tests.py`). A name used anywhere else keeps it: elsewhere in its file, in another trusted Python file (which may import it), or in the check command. So the rename breaks nothing. Helpers, models and URL modules beside the tests are never renamed.
  - **Cost:** each round repeats every passing check.
  - **If a rerun cannot complete:** the decision is `incomplete`, never an acceptance.
  - **Not covered:** code that special-cases the visible inputs. Held-out tests with other inputs are the answer to that.

**Making held-out tests.** Three ways to get cases the agent never sees. All write the held-out file to `--out`, which must be outside the repository.

- **Random split by test** (`--unit tests`, the default): `emi task holdout --file tests/test_x.py --out /secure/held.py --share 0.3` moves a seeded share of the file's top-level test functions and test methods to `--out`. Decorators travel with their tests; imports, helpers and fixtures stay in both files. A file with one test cannot be split.
- **Random split by assert** (`--unit asserts`): for files with one test function holding many asserts, such as `def check(candidate): assert candidate(1, 2) == 3 ...` called by `def test_x(): check(fn)`. The units are the assert statements written directly in the body of a top-level function (test or helper) that has at least two; an assert may span lines, and asserts nested in `if`, `for` or `with` are not units. `--share` and `--seed` choose which move. Each such function keeps at least one assert on each side, functions with fewer than two asserts are left whole, and everything else (imports, helpers, the callers) stays in both files, so each runs on its own. The output lists the held-out asserts as `<function>:<n>` (1-based position in the function) and carries `"unit": "asserts"`.
- **Designed variants** (`--vary --reference REF.py`): adds new cases and leaves the visible file unchanged. It reads the visible cases of the form `assert f(args) == expected` (`candidate(...)` in a `check(candidate)` helper stands for what the caller `check(f)` passes), derives new inputs from their literal arguments (neighbouring numbers, truncated, extended, reversed, permuted or swapped strings and lists, arguments mixed between cases), and gets each expected value by calling the reference in a separate process. `--count` (default 20) is the most cases written; the same file and `--seed` give the same cases. Run it on its own, not together with a split; to get both, split first and then vary the shortened file.

Why designed variants catch more: a random split holds out cases from the same distribution the agent already saw, so code that special-cases that distribution can pass them. Variants are inputs nobody wrote into the tests, with expected values from a trusted reference, so hard-coded answers and special cases have nothing to match. Nothing is invented: `--vary` without `--reference` is refused.

The reference must stay outside the repository (it is refused inside it), so the agent never reads it. It defines the entry point under the same name the tests call. Its results are compared exactly, so a reference whose float results differ from an honest solution's in the last digit gives false rejections; prefer integer, string and list cases there.

What is dropped is counted, never hidden: the output has `{"variants_written": n, "dropped": {"reference_error": a, "timeout": b, "unstable_repr": c, "duplicate": d, "unparsed_visible": e}}`. A case is dropped when the reference raised or ran past its per-case timeout, when a value does not read back from its `repr` as an equal Python literal, when the input repeats another or a visible case, and a visible assert is counted as unparsed when it is not a call compared with `==`, has non-literal arguments or keywords, or its entry point cannot be determined. `stopped_early` says the overall time ran out. A run that writes no case is an error and writes no file.

Next steps for any of the three:
1. For a split, commit the shortened file as the task baseline.
2. Pass the printed `--held-out` flag to `task draft`.

The split works line by line, so both halves must parse as Python (checked with `python3`) before either file is written. A file it cannot split cleanly is refused and left unchanged. The same `--seed` gives the same split.

This mirrors ImpossibleBench's finding: agents facing contradictory tests
cheated about half the time, mostly by editing tests, and read-only tests
restored at scoring stopped that without hurting legitimate performance
([arXiv 2510.20270](https://arxiv.org/abs/2510.20270)). Special-casing
product code gets past restored tests, and held-out tests are the answer
to that.

## Refactoring tasks

The optional `refactor` section, `{"targets": [globs], "min_cognitive_reduction": 0.2}`, requires the targets to get simpler: their cognitive complexity must fall by that share. The measurement includes every non-test file the candidate adds or modifies. Graded erosion must not rise.

Restored trusted tests and the inventory keep the behaviour. `emi task refactor-spec` writes such a spec, and docs/refactoring.md describes the workflow.

## Decisions

| Decision | When |
|---|---|
| `rejected` | a requirement's check failed, a change left the envelope, a blocking guardrail fired, a baseline test is missing from the inventory, the structural gate blocked, or the budget is exhausted |
| `incomplete` | a requirement could not be verified: launch errors, timeouts, or a run whose cleanup was not verified; or the structural gate, the guardrails or the test inventory could not run; or a linked dependency directory differs from its pin |
| `needs_review` | everything verified, but the structural gate warned under `treatment: review`, a review-tier guardrail fired, a trusted file was changed, a test file outside the trusted set was changed, or an escalation is open; completes only with an approval of its `review_items` |
| `accepted` | every requirement verified by its frozen checks, and every gate passes |

Every verification charges the budget and appends its check runs (with
cleanup receipts) and its decision to the append-only `evidence.jsonl` in
the authority. Rows are written whole under a lock on the task, and the
budget is charged by re-reading the state under that lock, so concurrent
verifications are all counted. A line that is not a record is an error
("the evidence log is damaged"), never skipped: a lost row could make an
older decision look like the latest. `complete` accepts only a commit whose latest decision
under the current contract is `accepted`, or `needs_review` with an
approval of exactly that decision; verifying the working tree never
completes a task.

## Reporting a contract conflict

Agents under test pressure sometimes "pass" a task by bending the tests or
the check setup. `emi task escalate` (MCP `emi_task_escalate`) is the
sanctioned alternative: it records that the tests or the contract
contradict the objective, with a required reason and, optionally, the
candidate that showed it. The row is appended to `evidence.jsonl` and gets
a 1-based index among the escalations of the current contract.

- It never charges the budget and runs no checks.
- While it is open, every candidate verified under that contract, whichever
  one the escalation named, is held at `needs_review`
  (`gates.escalation.state` is `open`), so nothing is `accepted` without a
  person. Rejections and gaps are not softened: a rejected or incomplete
  candidate stays so.
- It is resolved by a person either approving the decision naming
  `escalation:<n>` for each open escalation, or revising the contract
  (a new contract id, so earlier escalations no longer apply).
- If the escalations cannot be read, the gate is `unverified` and the
  decision is `incomplete`, never "none".

`emi task status` lists the open escalations as `escalations`, and
`hook-check` mentions them when it blocks.

## Approving a review

Review-tier findings are real events that are usually fine: emi-lab GV2
found CI edits detected 30 of 30 times but warranting intervention once.
A `needs_review` decision lists them as `review_items`, one per finding:
`guard:<id>:<path>`, `trusted:<path>`, `runner_config:<path>`,
`untrusted_test:<path>`, `shake:disagree`, `structural:warn`,
`refactor:erosion_rose`, `escalation:<n>`. `emi task approve` records that a named person
looked at them:

- the actor, a reason, and **every** item by name are required; an item
  the decision does not contain is refused, so an approval cannot waive
  anything beyond what was reviewed;
- it is bound to that one decision about that one commit under the
  current contract. A new commit, a re-verification or a revision needs a
  new approval; nothing is turned off for later candidates;
- it is appended to `evidence.jsonl`, and the completion record carries it.

An approval is only as trustworthy as access to the authority directory:
the agent must not be able to run `emi task approve` as the controller.
It is not offered over MCP.
