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
emi task install-hook --authority ~/.emi-authority --task fix-add --repo .    # pre-push: completed commits only
```

Every verify also runs the **guardrails** (docs/guardrails.md):
- **Blocking violations reject the candidate:** existing tests skipped or deleted, CI, secret or vendored-code edits.
- **Review-tier violations make it `needs_review`:**
  - fewer tests or assertions, test configuration changes, skips on new tests, assertions that cannot fail;
  - new suppressions, pasted duplicates, lockfiles, growth.
- **Configuration:** the spec's `"guardrails"` holds a policy object (tiers, allowed classes, budgets); `false` opts out.

Exit codes: `verify` returns 0 when accepted and 3 otherwise; `complete`
and `hook-check` return 0 or 3. Agents get the same through MCP as
`emi_task_verify` and `emi_task_status` (docs/agents.md).

## The spec

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
  voids earlier acceptances.
- **Checks.** Commands are argv lists, never a shell. They run through
  `emi-exec` in a fresh snapshot of the candidate (a commit's raw blobs, or
  the working tree as a commit would contain it). By default they run under
  bubblewrap, with a PID namespace, a read-only root, no network and a
  verified cleanup. `link` symlinks ignored dependency directories (a
  virtualenv, `node_modules`) into the snapshot. A check passes when it
  exits with `success.exit_code`.
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
  - Changing what a trusted test expects takes a contract revision.
- **`held_out`:** files frozen into the contract (outside the repository)
  and written into the workspace before the checks run. They catch code
  that special-cases the visible tests. Keep the authority directory out of
  the agent's reach.
- **`inventory`:** a command that lists test ids (one per line; `pytest`
  keeps `path::name` lines). It runs on the baseline and on the verified
  workspace. A baseline test missing from the candidate rejects it, which
  catches tests dropped through configuration (`collect_ignore`,
  `--deselect`, a runner option) rather than by editing test files.

This mirrors ImpossibleBench's finding: agents facing contradictory tests
cheated about half the time, mostly by editing tests, and read-only tests
restored at scoring stopped that without hurting legitimate performance
([arXiv 2510.20270](https://arxiv.org/abs/2510.20270)). Special-casing
product code gets past restored tests, and held-out tests are the answer
to that.

## Decisions

| Decision | When |
|---|---|
| `rejected` | a requirement's check failed, a change left the envelope, a blocking guardrail fired, a baseline test is missing from the inventory, the structural gate blocked, or the budget is exhausted |
| `incomplete` | a requirement could not be verified: launch errors, timeouts, or a run whose cleanup was not verified; or the structural gate, the guardrails or the test inventory could not run |
| `needs_review` | everything verified, but the structural gate warned under `treatment: review`, a review-tier guardrail fired, or a trusted file was changed |
| `accepted` | every requirement verified by its frozen checks, and every gate passes |

Every verification charges the budget and appends its check runs (with
cleanup receipts) and its decision to the append-only `evidence.jsonl` in
the authority. `complete` accepts only a commit whose latest decision
under the current contract is `accepted`; verifying the working tree
never completes a task.
