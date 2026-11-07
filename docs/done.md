# The done check (`emi init`, `emi done`)

When a coding agent says it is finished, `emi done` says what was
established. The vocabulary is in `CONTEXT.md`; laws, kept behaviour and the
pins have their own pages (`laws.md`, `preserve.md`, `determinism.md`).

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

The file has the schema `emi-config-1.0` (`emi-config-0.1` files are still
read). Unknown keys and values of the wrong type are errors.

```json
{
  "schema": "emi-config-1.0",
  "language": "python",
  "source": "src",
  "default_branch": "main",
  "tests": {"command": [".venv/bin/python", "-m", "pytest", "-q"], "globs": ["tests/*"]},
  "link": [".venv"],
  "python_path": ["src"],
  "repair_rounds": 3,
  "on_unexercised": "report",
  "on_redundant": "report",
  "pins": {"hash_seed": 0, "tz": "UTC", "locale": "C.UTF-8", "seed_random": true, "hypothesis": "derandomize"},
  "laws": {"dir": ".emi/laws", "lock": false},
  "preserve": {"generated_examples": 200, "call_timeout_s": 2},
  "adequacy": {"max_mutants": 0, "on_survivors": "report"}
}
```

Everything after `tests` may be left out and takes these defaults.
`repair_rounds` is for the hook. `on_unexercised`, `on_redundant` and
`adequacy.on_survivors` say what each finding does to the action:
`report` (state it; the agent may stop), `repair` (send the agent back) or
`ask_person`. A test command that runs tests in parallel is refused.

## Check: `emi done`

```sh
emi done [--repo .] [--baseline REF [--baseline-source session]] [--candidate REF]
         [--setting cooperative|enforced] [--format json|text] [--full] [--approve ITEM]...
```

The candidate is the working tree, or the commit given with `--candidate`.
The baseline is the merge base of HEAD and the default branch, also on a
branch just cut from it (the merge base is then HEAD). When HEAD is on the
default branch itself, or is detached at the merge base, no commit says where
the work began, so `--baseline` is required (the hook passes the session
baseline and says `--baseline-source session`); EMI does not guess one.

The tests run through the done check's engine from a derived contract: the tests the
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

`--format json` prints the receipt in canonical JSON (`emi-done-receipt-1.0`);
`--format text` is the same in plain words and is the default on a terminal.
The text shows what this run in particular did not establish and counts the
limits every receipt has; `--full` lists those too. The JSON always holds all
of them (`not_verified`), with this run's under `this_run`.

- **Regression**: `held`, `broken` or `unknown`, with the tests run, the
  failed and missing test ids, and the reasons. It is computed over the old
  tests and runner configuration the candidate did not touch, plus the
  candidate's version of any touched file a person approved. `unknown` is
  never a pass: a failure that cannot be attributed to a file (the failed-id
  list may be cut) is `unknown`.
- **New behaviour** (`change`): added tests run against the baseline's product
  code with the candidate's tests overlaid, and against the same frozen
  candidate as the regression check. A proving test fails or errors before
  and passes after. One that failed in its own run needs nothing more: the
  test code is the same at both revisions, so the product change made the
  difference, wherever it is (a function, a module-level value, a data
  file). One that errored must also execute changed product code in its own
  test context, because a test file that does not import gives the same
  error to every test in it. A test already passing before, or one that
  errored and does not reach the change, is redundant. Redundant tests are
  stated in the receipt: such a test does not show the change works, and it
  may still guard behaviour the change must keep, so the proposer is not
  sent back for it unless the project sets `on_redundant: repair`, or
  unless it shows nothing (below).
  Collection/import execution alone does not earn proving credit.
  - An old test the candidate edited is judged in its edited form by the same
    rule, and listed as a proving test with `edited: true`. Whether the edit
    is acceptable stays the person's question; that the edited test fails on
    the old code and passes on the new is a measured fact, and the receipt
    says how many proving tests are edited old tests.
  - A case of a parametrized test that passes at the baseline while another
    case of the same test function is a proving test is a **companion**
    (`companion_tests`), not redundant: such a test shows the change with
    some cases and what must stay as it was with the others. The same test
    function means the same name in the same file: a test written once in a
    base class and run under several subclasses is one function.
  - An added test skipped (or expected to fail) at both revisions is listed
    in `skipped_tests`: it does not run here, shows nothing, and is not a
    measurement that failed.
  - **Tests that show nothing** send the proposer back whatever the policy.
    When the candidate changes product functions and none of its tests
    proves the change or fails, each redundant or skipped test it added is
    such a test: it is offered as evidence and cannot be any. A candidate
    that changes no product function may add tests for behaviour that
    already holds, so its passing tests are fine; only when every test it
    added is skipped has it added nothing that runs. A changed function is exercised only when a
  proving test executes its changed body: the lines the change added, or,
  where the change also removed lines from the function (a guard taken out,
  a statement rewritten), any line of it, since a removed line cannot run
  and its absence changes every call. The levels are `exercised`,
  `partly_exercised`, `unexercised`, `none`, and `unknown`. Exercised is not
  proved: a function a passing law executes also lists that law (`laws`).
  - Requires coverage.py >=7.9 in the same environment as pytest; EMI selects
    the tracing engine that supports per-test contexts, including on Python 3.14.
  - Unknown added-test outcomes or unmeasurable changed functions produce
    `incomplete`, not `stop`. Removed functions and unsupported files are
    listed as not measured, never described as exercised.
  - `on_unexercised: report` states unexercised functions without demanding another
    round. `repair` sends them back within the hook's ordinary round budget.
  - The stricter unique-mutant rule is not implemented; the plain rule does
    not prove the tests express everything the user asked for.
- **Changed old tests** (`changed_old_tests`): every test file the baseline
  had that the candidate edited or deleted (`type: old-test`, item
  `old-test:PATH`), and every test-runner configuration file it edited,
  deleted or added (`type: runner-config`, item `runner-config:PATH`,
  `kind: added` possible), and every agent-instruction file it edited,
  deleted or added (`type: instructions`, item `instructions:PATH`). Each
  needs an approval with `emi done --approve ITEM`.
  - Agent instructions are the files coding agents read as instructions or
    that register their hooks: `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`,
    `.cursorrules`, `copilot-instructions.md` and the like anywhere, and
    anything under `.claude/`, `.cursor/`, `.opencode/`, `.gemini/`,
    `.codex/` or `.windsurf/`. No test reads them, so they are not restored
    and their `restored_result` is `not_run`. They wait for a person
    because every later agent follows them: a note that a failing test may
    be skipped, or a removed hook (the one that runs EMI, say), outlives
    the change that wrote it.
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

- **`laws`**: the person's laws, as the baseline has them (`laws.md`).
- **`preserve`**: the behaviour comparison, when asked for (`preserve.md`).
- **`adequacy`**: with `adequacy.max_mutants` set, single-token mutants of
  the lines the change added are run against the proving tests and the
  passing laws. A mutant none of them notices is listed as a survivor:
  that line could go wrong without the evidence telling. A survivor may be
  equivalent to the original, so `adequacy.on_survivors` defaults to
  `report`; `repair` sends the agent back to pin the line down, and
  `ask_person` leaves it to a person. Each mutant is one more run.
- **`signals`**: lines the change added to Python product code that match
  a known escape hatch (a checker suppression, `eval`/`exec`, test-runner
  sniffing, an `__eq__` that returns `True`). They are found by pattern,
  labelled `assurance: pattern`, and never change the action. `null` when
  they could not be looked for; `this_run` says why.
- **`ask`**: when the action is `ask_person`, what a person has to decide
  beyond the items in `changed_old_tests`: redundant tests, unexercised
  functions or surviving mutants whose policy is `ask_person`, and checks
  that ran without a sandbox.
- **`inputs`, `input_id`, `decision_id`, `policy`**: what the decision is a
  function of (`determinism.md`). `meta` says where it ran and is the only
  part a replay does not reproduce.

### The action and the exit status

| Exit | Action | Meaning |
|---|---|---|
| 0 | `stop` | Regression held, no failing added test, no unapproved changed old test/runner configuration. It lets the proposer stop; it is not a statement that the task is done. |
| 3 | `repair` | Something the proposer can fix; `repair` names every item of the first failing kind (up to five, then a count), each with what to do. |
| 4 | `ask_person` | A changed old test, runner configuration or agent-instruction file waits for an approval, or something else is a person's to decide; the receipt's `ask` lists it. One such case: the checks ran without a sandbox because bubblewrap is not available on this machine. A failure there still sends the change back; a pass is not the proposer's to accept. |
| 5 | `incomplete` | EMI could not measure (no sandbox, no pytest, config absent at the baseline). Not a pass. An old test or law that held at the baseline and fails reproducibly is `repair` even then: the break is established whatever else could not be measured. |
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

**Which code the tests import.** EMI runs the tests in a copy of the
candidate, not in your checkout. A project with a src layout is usually
installed into its environment in editable mode, which points the
environment at one particular checkout; run from a copy, the tests would
import that checkout (or fail to import anything), not the code being
checked. `emi init` therefore records the import roots (`python_path`, for
example `["src"]`), every run puts them first on the import path, and a
check confirms that the project's own packages resolve inside the copy. If
they would not, the action is `incomplete` and the receipt names the package
and where it would have come from.

The copy holds what git holds, plus what `link` names. Files git ignores
that the tests import all the same (a version module generated at install
time, a compiled extension) are not in it, and without them the copy does
not import. `emi init` finds such files inside the project's own code and
adds them to `link`, as they are on disk. A linked compiled extension is not
rebuilt for a check: a change to its sources is not measured.

Regression evidence is a comparison, test by test, with the baseline's own
run. The test command runs once on the baseline (the result is kept and
reused for every later check of the same baseline, configuration and
dependencies) and once on the candidate:

- a test the baseline already skipped, or expected to fail, may stay so;
- a test that already failed at the baseline is not regression evidence,
  and the receipt says how many there are;
- a test that passed at the baseline and now fails, or no longer runs
  (skipped by the candidate's code), is `broken`;
- a test the candidate added is not regression evidence: one that fails is
  a failing added test (a `repair`), one that is skipped is listed in the
  change evidence (and is a `repair` when it shows nothing, above);
- a suite that ran at the baseline and does not start on the candidate (an
  import the change broke stops pytest before any test) is `broken`, with
  the run's last error line. The action is `repair` even though the change
  evidence cannot be measured on such a candidate.

A test file the baseline already cannot import is tolerated the same way: the
run continues past it and its tests are not regression evidence. A file only
the candidate cannot collect is listed in `change.failing_tests` with
`candidate_result: not collected` and is a `repair`; the other files are
still measured. An edited old test file whose baseline form no longer
collects on the candidate is the person's question, like a baseline form
that fails.

A failure is a break when it shows again. Before a test is reported as
broken, the failed tests are run once more by themselves on the candidate as
it is (seconds, not a suite run). Those that fail again are the break. Those
that pass were not broken by the product code alone:

- when the run restored test files or runner configuration the change
  edited (a `conftest.py`, a helper, expected output), they fail only beside
  the baseline versions of those files. That is the person's question, as it
  is for an edited test whose baseline form fails; the regression line says
  how many and names the first.
- when nothing was restored, the whole suite runs a second time under the
  same pins. A test that fails again depends on what ran before it, which
  the change altered: it stays a break. One that passes did not reproduce:
  it is listed in `regression.nondeterministic`, and the action is
  `incomplete`. A failure is never cleared by a later run that passed.

A test the candidate added that fails beside restored files is judged by the
change evidence alone, which runs it on the candidate as it is.

Chosen tests are run by naming them. pytest cannot select every test by
its id (a parameter with an escape in it); when it refuses, the file is run
whole and the chosen tests are read from its report.

Each check works on copies of the repository in the temporary directory and
removes them when it ends. A check that is killed (a harness timeout, an
interrupt) cannot, so every check first removes the copies left by EMI
processes that no longer exist.

Known limit: a test that can only fail on another platform (a Windows path
case, say) passes at the baseline here and is called redundant.

A redundant test carries why: it `passes at the baseline`, or it `executes
no changed line`. The text receipt counts them and names the first. Under
`on_redundant: repair` the repair says which, and that the test may be fixed
(made to fail on the old code, or to exercise the change) instead of removed.

A change outside every function (a module-level value, a table) has no
function to list: the status is `none`, and the text receipt says the change
is shown by its proving tests.

The text receipt's `held` line says what was not counted: tests that already
failed at the baseline, and failures a second look cleared.

A test the candidate adds that fails on the candidate is listed in
`change.failing_tests` and is a `repair`: the proposer is told which test,
and to fix the code or the test.

Tests appended to an old test file need no approval: every line the baseline
had is still there, so nothing was rewritten. The file still runs in its
baseline form as regression evidence, and the appended tests are judged as
change evidence. A file with removed or changed lines asks a person.

The baseline's tests run twice under the same pins, and a test whose outcome
differs between the two is listed in `regression.quarantined` and is not
regression evidence. The first check of a baseline therefore runs the suite
three times. An empty
suite, a report that names fewer tests than were collected, an exit status
that disagrees with the report, and a cut or unreadable report are
`incomplete`: collection alone is not proof the tests executed.

The outcomes are not read from what pytest prints while it runs (a test that
writes to the terminal, or a parallel runner, makes that unreadable). The
check starts the tests' own Python with a small reporter that calls
`pytest.main` with the configured arguments and, at the end, prints one
line per test from the reports pytest hands its plugins, closed by a count.
A listing that is cut short or does not add up is no evidence.

`enforced` describes which inputs EMI reads. It is **not yet a claim of
adversarial security**: isolation-before-launch, sealed dependencies and
durable budgets remain later work in the plan. Run untrusted PR tests on an
ephemeral worker with no secrets or write credentials, never with
`pull_request_target` and a privileged checkout.
