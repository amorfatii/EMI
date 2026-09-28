# Verified refactoring

A refactor should make code simpler without changing what it does. Tools
that only score code can check the first half; this page describes how
EMI checks both, for a person or an agent doing the work.

```sh
emi explain src/app.py                                     # what to change, and why
emi task refactor-spec --target src/app.py -- python -m pytest -q > spec.json
emi task create --authority ~/.emi-authority --spec spec.json
# ... the agent refactors ...
emi task verify --authority ~/.emi-authority --task refactor-app --worktree
```

## 1. Choose: `emi explain`

For every function in a file or directory, highest graded erosion first, it shows:
- **graded erosion** (profile `graded`), split into its complexity half and its size half;
- **where the cognitive complexity comes from:**
  - 1 per branch, loop or catch;
  - the **nesting penalty** (each structure also costs its nesting depth);
  - boolean chains, else branches, and labelled jumps;
- **steps**, each anchored to lines and with the cognitive complexity it removes:
  - **deep nesting:** return early (guard clauses), or extract the innermost block;
  - **nested branches:** invert the outer condition so inner branches sit at the top level;
  - **complex condition:** name each part of a mixed `and`/`or` chain;
  - **else-if chain:** a lookup table or dispatch;
- **cognitive complexity after the steps.**

```text
src/app.py:1 classify  graded 9.8 (complexity 6.0, size 3.8)  cognitive 12 = 4 structures + 5 nesting + 1 boolean + 2 else  sloc 12
  1. deep nesting (line 4, 9; -4 cognitive): ... return early from the enclosing conditions (guard clauses), or extract the innermost block ...
  after these steps: cognitive 12 -> 8
```

**Why graded chooses and cognitive complexity measures:**
- **Graded ranks bug-prone functions better than EMI's old erosion.** It was right in all 20 repositories of study G2 (emi-lab `results/G2`). It ranks about as well as function length, which is why it has a size half.
- **Its complexity half saturates** at about one increment per five lines. A very complex function stays near the ceiling until most of its complexity is gone, so `explain` flags saturated functions and progress is tracked in cognitive complexity.
- **Neither measure rewards splitting a function evenly,** because both are sums over density. Threshold warnings do reward it: splitting a long method in two silences "Large Method" without making anything simpler.

## 2. Verify: a refactoring task

`emi task refactor-spec --target FILE [--min-reduction 0.2] -- TEST COMMAND` prints a spec for `emi task create`. It sets these parts:

| Part | What it does |
|---|---|
| `refactor` | The targets' cognitive complexity must fall by at least `min_cognitive_reduction` (default 20%). Graded erosion must not rise; if it does, the result needs review |
| `verification.trusted` | Every existing test, restored before the checks run. A refactor that breaks behaviour fails even if the agent edits the test to hide it |
| `verification.inventory` | For pytest: the collected test ids must all still be there |
| `envelope` | The targets, files next to them (new helpers), and tests |
| `checks` | The test command, run by EMI itself in a snapshot of the candidate |

The `refactor` gate measures the targets plus **every non-test source file the candidate adds or modifies**, before and after. Moving code into a new or existing module is therefore not a reduction.

**How the gate decides:**

| Gate state | When |
|---|---|
| `pass` | cognitive complexity fell by the required share, graded did not rise |
| `fail` (rejects) | the fall is smaller than required |
| `needs_review` | complexity fell but graded erosion rose |
| `unverified` (incomplete) | nothing to measure at the baseline |

Checked end to end (`crates/emi-controller/tests/lifecycle.rs`):

| Candidate | Decision |
|---|---|
| real flattening, behaviour kept (cognitive 12 → 6) | accepted |
| the same shape with one case changed | rejected (tests) |
| ... and the test edited to hide it | rejected (the trusted test is restored) |
| rename only | rejected (0% simpler) |
| function moved to a new module, or into an existing one | rejected (the moved code is counted) |

## Compared with a score-driven loop

**CodeScene's loop.** Its MCP server has no tool that refactors. Its refactoring guidance asks the agent to:
1. record the Code Health score;
2. make a few small changes;
3. re-review until the score improves.

Nothing in that loop runs the tests. And in emi-lab studies, Code Health did not predict bugs or agent success beyond size (S2a, S5a, S5r).

**EMI's loop.**
- It ties success to behaviour through restored tests and the test inventory.
- It measures simplification with a quantity that splitting cannot fake.
- It records every verification in the task's evidence log.

## Limits

- **The tests define "behaviour".** Untested behaviour can change unnoticed. Held-out tests (docs/tasks.md) and good coverage narrow that gap.
- **Cognitive complexity misses some improvements.** Better names and clearer data flow count as no progress.
- **`--min-reduction` is a policy, not a law.** Some real refactors (extracting a concept, removing duplication) lower complexity only a little, so choose the target accordingly.
