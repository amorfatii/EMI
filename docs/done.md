# The done check (`emi init`, `emi done`)

When a coding agent says it is finished, `emi done` says what was proven. It
implements the completion check in `docs/plan.md`; the vocabulary is in
`CONTEXT.md`.

## Set up once: `emi init`

```sh
emi init [--repo .] [--yes] [--force] [--default-branch BRANCH]
```

It detects the language (Python with pytest; anything else is refused with
"unsupported for now"), the source directory, the default branch, the test
command (`.venv/bin/python -m pytest -q` when a virtualenv exists, otherwise
`python -m pytest -q`), the test file globs and the dependency directories to
link (`.venv`, `venv`), prints them, and writes `.emi/config.json`.

- On a terminal it asks before writing. Without a terminal and without
  `--yes` it prints the proposal, writes nothing and exits 2.
- It never replaces an existing file without `--force`.
- Commit the file: enforced verification reads it from the baseline.

The file has the schema `emi-config-0.1` (see plan.md, interface 1). Unknown
keys and values of the wrong type are errors. `repair_rounds` (default 3) and
`on_unproven` (`report`, the default, or `repair`) are for the hook.

## Check: `emi done`

```sh
emi done [--repo .] [--baseline REF [--baseline-source session]] [--candidate REF]
         [--setting cooperative|enforced] [--format json|text] [--approve ITEM]...
```

The candidate is the working tree, or the commit given with `--candidate`.
The baseline is the merge base of HEAD and the default branch, also on a
branch just cut from it (the merge base is then HEAD). When HEAD is on the
default branch itself, or is detached at the merge base, no commit says where
the work began, so `--baseline` is required (the hook passes the session
baseline and says `--baseline-source session`); EMI does not guess one.

The tests run through `emi-controller` from a derived contract: the tests the
baseline had and the test-runner configuration are restored to their baseline
versions before the run (so an edited test runs in its original form), the set
of test ids is compared with the baseline's, and everything runs in a sandbox
without network. The contract and its evidence are kept outside the
repository, under `$XDG_STATE_HOME/emi` (default `~/.local/state/emi`);
each check adds a couple of kilobytes to the evidence log. Nothing is counted
against a budget: repair rounds belong to the hook. In the cooperative
setting, linked dependencies (`.venv`) that changed since the last check are
pinned again; enforced verification refuses them (`incomplete`).

### The receipt

`--format json` prints the receipt in canonical JSON (`emi-done-receipt-0.3`);
`--format text` is the same in plain words and is the default on a terminal.

- **Regression**: `held`, `broken` or `unknown`, with the tests run, the
  failed and missing test ids, and the reasons. It is computed over the old
  tests and runner configuration the candidate did not touch, plus the
  candidate's version of any touched file a person approved. `unknown` is
  never a pass: a failure that cannot be attributed to a file (the failed-id
  list may be cut) is `unknown`.
- **New behaviour** (`change`): added tests run against the baseline's product
  code with the candidate's tests overlaid, and against the same frozen
  candidate as the regression check. A proving test fails or errors before,
  passes after, and executes changed product code in its own test context.
  A test already passing before, or unrelated to the changed code, is redundant
  and sends the proposer back for removal. Collection/import execution alone
  does not earn proving credit. A changed function is proven only when a
  proving test executes its changed body. The levels are `proven`,
  `partly_proven`, `unproven`, `none`, and `unknown`.
  - Requires coverage.py >=7.9 in the same environment as pytest; EMI selects
    the tracing engine that supports per-test contexts, including on Python 3.14.
  - Unknown added-test outcomes or unmeasurable changed functions produce
    `incomplete`, not `stop`. Removed functions and unsupported files are
    listed as not measured, never described as proven.
  - `on_unproven: report` states unproven functions without demanding another
    round. `repair` sends them back within the hook's ordinary round budget.
  - The stricter unique-mutant rule is not implemented; the plain rule does
    not prove the tests express everything the user asked for.
- **Changed old tests** (`changed_old_tests`): every test file the baseline
  had that the candidate edited or deleted (`type: old-test`, item
  `old-test:PATH`), and every test-runner configuration file it edited,
  deleted or added (`type: runner-config`, item `runner-config:PATH`,
  `kind: added` possible). Each needs an approval with
  `emi done --approve ITEM`.
  - Until approved, a touched old test runs in its baseline form. If that
    form fails, the entry says `restored_result: fails` (the proposer
    rewrote a test that fails as it was) and the action is `ask_person`, not
    `repair`.
  - Once approved, the candidate's version of the file is used and its tests
    count as regression evidence like any other; if that version fails, the
    regression is `broken`. `restored_result` is then `unknown`.
  - Runner configuration (`pyproject.toml`, `conftest.py`, ...) is restored
    for the run and never makes the regression `broken` by itself. An added
    file cannot be left out of the run; the regression reasons say it ran.
  - An approval is bound to the file's exact content in the candidate (a
    deletion to the deletion) and to the baseline, so a further edit asks
    again.
- **`not_verified`**: always present; what the receipt does not establish.

### The action and the exit status

| Exit | Action | Meaning |
|---|---|---|
| 0 | `stop` | Regression held, no redundant test or unapproved changed old test/runner configuration. |
| 3 | `repair` | Something the proposer can fix; `repair` names the first failing thing. |
| 4 | `ask_person` | A changed old test or runner configuration file waits for an approval. |
| 5 | `incomplete` | EMI could not measure (no sandbox, no pytest, config absent at the baseline). Not a pass. |
| 2 | | Usage or configuration error. |

"Stop" does not mean "correct": it means the evidence in the receipt holds.

### Settings

- `cooperative` (default): the configuration is the candidate's own, and
  approvals are read from the state directory. The proposer could alter both,
  so this catches a careless proposer, not a cheating one.
- `enforced`: the configuration is read from the baseline revision (a git
  blob), never from the candidate, and `--baseline` is required. If the
  baseline has no `.emi/config.json` the action is `incomplete`. Local
   approvals are not read, so a changed old test stays a review item.

Regression evidence is a comparison, test by test, with the baseline's own
run. The test command runs once on the baseline (the result is kept and
reused for every later check of the same baseline, configuration and
dependencies) and once on the candidate:

- a test the baseline already skipped, or expected to fail, may stay so;
- a test that already failed at the baseline is not regression evidence,
  and the receipt says how many there are;
- a test that passed at the baseline and now fails, or no longer runs
  (skipped by the candidate's code), is `broken`;
- a test the candidate added counts when it fails, not when it is skipped
  (the change evidence reports that).

A test file the baseline already cannot import is tolerated the same way: the
run continues past it and its tests are not regression evidence. A file only
the candidate cannot collect is a break, or, for an added test file, leaves
the change evidence unknown.

Tests appended to an old test file need no approval: every line the baseline
had is still there, so nothing was rewritten. The file still runs in its
baseline form as regression evidence, and the appended tests are judged as
change evidence. A file with removed or changed lines asks a person.

The first check of a baseline therefore runs the suite twice. An empty
suite, a report that names fewer tests than were collected, an exit status
that disagrees with the report, and a cut or unreadable report are
`incomplete`: collection alone is not proof the tests executed. A run that
does not print one outcome per test (for example under `pytest-xdist`)
cannot be compared, and falls back to the strict rule: any skip or expected
failure is `incomplete`.

`enforced` describes which inputs EMI reads. It is **not yet a claim of
adversarial security**: isolation-before-launch, sealed dependencies and
durable budgets remain later work in the plan. Run untrusted PR tests on an
ephemeral worker with no secrets or write credentials, never with
`pull_request_target` and a privileged checkout.
