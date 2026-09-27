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
| `tests.deleted` | A test file deleted, or moved out of the test layout | block |
| `tests.fewer` | Fewer test definitions across the change's test files (Python `def test_*`, JS/TS `it`/`test`, Go `func Test*`, Rust `#[test]`, JUnit `@Test`) | block |
| `tests.fewer_assertions` | Fewer assertions across the change's test files | block |
| `tests.skip_added` | Skip, ignore or xfail added: `@pytest.mark.skip/skipif/xfail`, `pytest.skip()`, `unittest.skip*`, `it.skip`/`xit`/`describe.skip`, `this.skip()`, `t.Skip`, `#[ignore]`, `@Disabled` | block |
| `tests.config_changed` | Test or coverage configuration changed: `pytest.ini`, `conftest.py`, jest/vitest/karma/mocha/playwright configs, `.coveragerc`, `.nycrc`, codecov; test sections of `pyproject.toml`/`setup.cfg`/`Cargo.toml`; test environments of `tox.ini` and test sessions of `noxfile.py` | block |
| `prohibited.ci` | CI configuration (`.github/workflows`, `.gitlab-ci.yml`, …) changed | block |
| `prohibited.secret` | A secret-like file (`.env*`, `*.pem`, `id_rsa*`, `.npmrc`, …) changed | block |
| `prohibited.vendored` | Vendored third-party code (`vendor/`, `node_modules/`, `third_party/`) changed | block |
| `prohibited.lockfile` | A lockfile changed | review |
| `suppress.added` | A suppression added: `# noqa`, `# type: ignore`, `# pylint: disable`, `# pragma: no cover`, `eslint-disable`, `@ts-ignore/@ts-expect-error`, `c8/istanbul ignore`, `#[allow(...)]`, `//nolint`, `@SuppressWarnings`, or an `emi-ignore` without a reason | review |
| `dup.introduced` | Added product code repeating 6+ significant lines found elsewhere in the tree (moved code does not count) | review |
| `growth.net_sloc` / `growth.files` | Net added lines or touched files beyond the policy budget | review |

**Details that keep the checks precise:**
- Files inside test directories are test fixtures, even when they look vendored or secret.
- Test and assertion counts are summed across the change, so splitting a test file is not a removal.
- Markers inside string literals are ignored.
- Files under `example(s)/` are documentation.

## Measured precision

Precision is "when a guardrail fires, the described event really
happened". It was measured in `emi-lab` (study GV1) on 1,696 human
commits from 20 open-source repositories, per round. In the final round
every block-tier id but one met the ≥ 0.95 bar (labels re-audited after a
blind second-reviewer check):

| Guardrail | Precision |
|---|---|
| `prohibited.ci` | 20/20 |
| `tests.deleted` | 20/20 |
| `tests.skip_added` | 16/19 (three were reformatted existing markers, fixed since) |
| `tests.fewer` | 8/8 |
| `tests.fewer_assertions` | 15/15 |
| `tests.config_changed` | 19/20 |

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
