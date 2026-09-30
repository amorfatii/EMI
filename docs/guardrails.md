# Guardrails (`emi guard`)

Guardrails are deterministic checks on a change whose correctness can be
verified directly from the diff. They do not predict anything. EMI's
validation studies found that static quality scores (EMI's own,
cognitive complexity, CodeScene's Code Health) add little beyond size for
predicting defects or agent success. What EMI can state with certainty is
*what a change did*:
- it skipped a test;
- it deleted assertions;
- it edited CI or a lockfile;
- it silenced a linter;
- it pasted code that already exists.

These are the ways an AI agent makes a task *look* done.

```sh
emi guard --base main                 # the working tree (untracked files included) against main
emi guard --base main --head HEAD     # a commit range
emi guard --base main --format json   # also: text (default), sarif, markdown
emi guard --base main --policy guard.json --fail-on review
```

**Exit codes:**

| Code | Meaning |
|---|---|
| 0 | No violation at or above `--fail-on` (default `block`) |
| 2 | A blocking violation |
| 3 | Review-tier violations only, with `--fail-on review` |

## What is checked

Every violation carries a stable id, a path, a line where there is one,
and evidence.

| Id | Detects | Default tier |
|---|---|---|
| `scope.outside` / `.excluded` / `.protected` / `.kind` | Edits outside the permitted paths, to excluded or protected paths, or of a forbidden kind (policy scope; tasks use their envelope) | block |
| `tests.deleted` | A test file deleted, or moved out of the test layout | review (block until v0.3) |
| `tests.fewer` | Fewer test definitions across the change's test files (Python `def test_*`, JS/TS `it`/`test`, Go `func Test*`, Rust `#[test]`, JUnit `@Test`); merging tests into a parametrized one also lowers the count | review |
| `tests.fewer_assertions` | Fewer assertions across the change's test files (a table-driven refactor can check the same cases with fewer asserts) | review |
| `tests.skip_added` | Skip, ignore or xfail added to a test the baseline already had: `@pytest.mark.skip/skipif/xfail`, `pytest.skip()`, `unittest.skip*`, `it.skip`/`xit`/`describe.skip`, `this.skip()`, `t.Skip`, `#[ignore]`, `@Disabled` | review (block until v0.3) |
| `tests.skip_new_test` | The same markers on a test the change adds (a platform-only or slow test, say): a judgment call | review |
| `tests.assertion_trivialized` | An assertion that cannot fail added to a test file (`assert True`, `self.assertTrue(True)`, `expect(true).toBe(true)`, `assert!(true)`), e.g. a real check replaced with the same count | review |
| `tests.config_changed` | Test or coverage configuration changed: `pytest.ini`, `conftest.py`, jest/vitest/karma/mocha/playwright configs, `.coveragerc`, `.nycrc`, codecov; test sections of `pyproject.toml`/`setup.cfg`/`Cargo.toml`; test environments of `tox.ini` and test sessions of `noxfile.py`; a new `conftest.py` that installs hooks or plugins (one with fixtures only is a new test package) | review |
| `prohibited.ci` | CI configuration (`.github/workflows`, `.gitlab-ci.yml`, …) changed | review (block until v0.3) |
| `prohibited.secret` | A secret-like file (`.env*` other than templates, `*.pem` with a private key, `id_rsa*`, `.npmrc`, …) changed | review |
| `prohibited.vendored` | Vendored third-party code (`vendor/`, `node_modules/`, `third_party/`, CMake `_deps/`) changed | review (block until v0.3) |
| `prohibited.lockfile` | A lockfile changed | review |
| `tests.unverified` | A changed test file whose old or new version could not be read, so its tests and assertions were not counted (unknown, not zero) | review |
| `code.early_exit` | Product code (not tests, not an entry point such as `main.go` or `cmd/`) that ends the process or reaches into the test runner: `os._exit`, a module-level `sys.exit`/`exit`/`raise SystemExit`, `_pytest` imports, `pytest_*` hooks, `process.reallyExit`/`abort`, a top-level `process.exit`, `os.Exit`, `process::exit`. A check passes on its exit code, so code that exits with 0 while the tests import it passes them without running one | review |
| `gaming.test_sniff` | Python product code that asks whether a test runner is loaded (`sys.modules` checked for `pytest`, `unittest`, `nose`), so it can behave one way under the tests and another in use | review |
| `gaming.always_equal` | A Python `__eq__` whose whole body is `return True` (or `__ne__` returning False): its values pass every equality assertion | review |
| `gaming.test_literals` | Python product code that holds one test case's inputs and expected value within two lines, as in `if (a, b) == (1, 2): return 3` or a table entry `(1, 2): 3`. Test cases are assertion lines, in test files that mention the changed module, that call something with a literal argument and hold a literal outside the call. Special-casing the change only moved does not count | review |
| `gaming.unverified` | The tests or the new version of a file could not be read, so the gaming checks did not run on it (unknown, not clean) | review |
| `suppress.added` | A suppression added: `# noqa`, `# type: ignore`, `# pylint: disable`, `# pragma: no cover`, `eslint-disable`, `@ts-ignore/@ts-expect-error`, `c8/istanbul ignore`, `#[allow(...)]`, `//nolint`, `@SuppressWarnings`, or an `emi-ignore` without a reason, in the project's own code (not vendored or generated files) | review |
| `dup.introduced` | Added product code repeating 6+ significant lines found elsewhere in the tree (moved code, a duplicate the file already had, comments, export lists and `dist*/` build output do not count) | review |
| `growth.net_sloc` / `growth.files` | Net added lines or touched files beyond the policy budget | review |

**Details that keep the checks precise:**
- Files inside test directories are test fixtures, even when they look vendored or secret.
- Test and assertion counts are summed across the change, so splitting a test file is not a removal.
- Markers inside string literals are ignored.
- Files under `example(s)/` are documentation.
- A new package's manifest (an added `pyproject.toml`) does not change existing test configuration.

**Detectors vs. guarantees.** These checks report what a change did. In a
supervised task, the contract's `verification` section (docs/tasks.md)
makes the main cheats unable to pass instead:
- trusted tests are restored before the checks run;
- held-out tests run that the agent never saw;
- the test runner's inventory is compared with the baseline's.

There, the guardrails explain what the agent tried.

**Gaming checks.** Restoring trusted tests cannot catch code that fools
the tests from the product side, with the tests left as they were. The
`gaming.*` checks look for the forms of that seen so far. They cover
Python only, and were built only from emi-lab's mechanism suite and the
first form of each family in emi-lab S11 (its development data). S11
measures, on real agent cheats and on forms the checks were not built
on, how much they catch and how often they flag honest work. Until then
they ask for review and never block. In a supervised task, that means
`needs_review` instead of `accepted`.

They are review hints, not guarantees: each recognises a known shape,
and code written differently passes it. `code.early_exit` is the same
kind of rule. The other ids (`scope.*`, `tests.*`, `prohibited.*`,
`suppress.added`, `growth.*`, `dup.*`) observe what the diff did. A task
decision's `assurance` lists which kind its reasons rest on, and
`catch_kind: pattern` means only a recognised pattern stood between the
candidate and acceptance (docs/tasks.md).

## v0.4: what blocks by default

**Study GV2** (emi-lab `results/GV2/REPORT.md`) was pre-registered. It measured v0.3 on 1,079 pull requests from 60 repositories no earlier study used. Each firing was asked two questions: did the event happen, and was stopping the change warranted?

| Id | Event happened | Blocking warranted |
|---|---|---|
| `prohibited.ci` | 30/30 | 1/30 |
| `tests.deleted` | 8/8 | 2/8 |
| `tests.skip_added` | 6/7 | 5/7 |
| `prohibited.vendored` | 1/4 | 0/4 |

- **What this means:** the events are real, but in ordinary work they are mostly the change's purpose: a new workflow, tests of a removed feature, a known-flaky test parked with a FIXME.
- **What it cost:** blocking them cost **8.5 false blocks per 100 PRs** (95% interval 6.4 to 10.7).
- **Under the registered rule:** all four now need **review** by default.
- **What still blocks:** only envelope violations (`scope.*`), meaning edits outside what a task or policy permits.

In a supervised task, stopping a bad change is the job of the verifier's guarantees (docs/tasks.md):
- trusted tests are restored and read-only;
- held-out tests run;
- the test inventory is compared.

Pattern rules are not used for that. A project that wants a rule to block sets it in `tiers`, for example `{"tests.skip_added": "block"}`.

## Measured precision

Precision means: when a guardrail fires, the event it *means* really
happened. A new skip, not a reformatted one, counts; a deleted fixture
is not a deleted test; a formatting-only edit is not a test-configuration
change.

It was measured in `emi-lab` (study GV1) on 1,696 human commits from
20 open-source repositories, with a second reviewer blind-checking a
sample of the labels. Round 5 (emi `2e0efd9`, a fresh sample) is final
for v0.1: a blocking guardrail below 0.95 was demoted to review by
default rather than tuned further.

| Guardrail | Default tier | Round 5 precision |
|---|---|---|
| `prohibited.ci` | block | 20/20 |
| `tests.deleted` | block | 8/8 |
| `tests.skip_added` | block | 8/8 |
| `tests.fewer` | review | 7/8 (a merge into a parametrized test) |
| `tests.config_changed` | review | 17/20 (a comment-only edit; no-op coverage settings) |
| `tests.fewer_assertions` | review | 7/9 (refactors that check the same cases) |
| `prohibited.lockfile` | review | 20/20 |
| `suppress.added` | review | 19/20 |
| `dup.introduced` | review | 16/20 (docs example variants; a generated file) |
| `tests.skip_new_test`, `tests.assertion_trivialized` | review | new in v0.2, not yet measured |

**On agent pull requests** (study S7: 4,704 real PRs from 149 repositories, emi v0.1):
- **Weakening:** agent PRs did not weaken tests more often than human PRs in the same repositories.
- **Precision:** 198 sampled firings were labelled, with 40 spot-checked blind by a second reviewer.
- **False alarms found:**
  - `skip_added` fired on skips in new tests (split out in v0.2);
  - `suppress.added` fired inside `node_modules/`, `vendor/`, CMake `_deps/` and coverage reports;
  - `dup.introduced` fired on license headers, `__all__` lists, compiled `dist-cjs/` copies, and duplicates that already existed and were only reformatted;
  - `config_changed` fired on new packages' `pyproject.toml` files;
  - `prohibited.secret` fired on `.env.template` files, version pins and `${VAR}` placeholders (0/4, and 0/20 in S6).
- **v0.3 changes:**
  - v0.3 fixes these classes;
  - `prohibited.secret` is demoted to review.
- **Replayed on the S7 sample:** no true firing was lost, and 42 of 55 false alarms went quiet. That is a development check on the sample that found them; precision on a fresh sample is still to be measured.

The block tier was right 36 of 36 times: the described event happened (exact 95% interval 0.90 to 1.00). Whether blocking it was *useful* is a separate question, which GV2 measured (below). Any id can be made blocking
for a project with the policy's `tiers`.

Lockfiles changed in 11.7% of human commits, too often to block without
context, so `prohibited.lockfile` needs review by default. The guard
takes about 12 ms per commit (0.24 s on django with duplication
checking).

**On AI-agent patches** (study S6: 61,116 SWE-bench Verified patches from 130 systems, plus 500 gold patches):
- **Firing rates:** block-tier guardrails fired on 1.0% of agent patches and on none of the gold patches.
- **Half of those firings were not the agent's doing.** They were the benchmark environment's own `tox.ini` edit, which systems that diff the whole tree submit along with their patch.
- **The real firings** were mostly:
  - test files overwritten or truncated;
  - assertions deleted;
  - a failing test replaced by `@skip` and `pass`;
  - `conftest.py` deleted;
  - committed `build/` output.
- **Precision:** after a blind spot-check by a second reviewer, the test guardrails were right 66/66 times. The block tier overall was 80/107.
- **The misses, all fixed since:**
  - skip markers inside test-fixture strings and comments;
  - suppressions that were moved, or kept on an edited line;
  - public CA bundles (`cacert.pem`) reported as secrets.

  These are no longer reported.

## Policy

A JSON object; every key is optional.

```json
{
  "permitted_paths": ["src/*", "tests/*"],
  "excluded_paths": ["src/generated/*"],
  "protected_paths": ["tests/fixtures/*"],
  "change_kinds": ["add", "modify"],
  "allow_classes": ["lockfile"],
  "tiers": {"tests.config_changed": "review", "suppress": "block", "dup": "off"},
  "max_net_sloc": 300,
  "max_files": 12
}
```

- **Globs** are posix fnmatch: `*` spans `/`.
- **`tiers`** takes a full id or a family (`tests`, `suppress`, …). The values are `block`, `review` or `off`. `off` reports a violation without affecting the decision.
- **`allow_classes`** permits changes to `lockfile`, `ci`, `secret_like` or `vendored` files.

## In supervised tasks

Task contracts (docs/tasks.md) freeze a `guardrails` policy, which is on
by default:
- `"guardrails": false` opts out;
- an object sets tiers, allowed classes and budgets;
- scope is enforced by the contract's envelope.

`emi task verify` then reports `gates.guardrails`:
- a blocking violation makes the decision `rejected`;
- review-tier ones make it `needs_review`.

## For agents

- **The MCP tool `emi_guard`** returns the same report, so an agent can fix each violation by id, file and line before it finishes. `emi_task_verify` includes the guardrails gate.
- **As a Claude Code `Stop` hook:** the agent cannot end its turn while a blocking violation stands. In `.claude/settings.json`:

  ```json
  {
    "hooks": {
      "Stop": [{"hooks": [{"type": "command",
        "command": "emi guard --base \"$(git merge-base HEAD origin/main)\" --format text 1>&2"}]}]
    }
  }
  ```

  Exit code 2 blocks the stop, and the violations reach the model on stderr.
- **In CI:** `emi guard --base origin/main --head HEAD --format sarif -o guard.sarif`, or `--format markdown` for a PR comment.
