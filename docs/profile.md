# Code profile (`emi profile`)

`emi profile` answers, per function: **if this code changes, would the
tests notice a wrong change?** It joins what EMI already measures with
two kinds of test evidence and keeps three kinds of statement apart:

- **facts**, measured or read from history;
- **inferences**, what those facts suggest (never measured themselves);
- **unknowns**, what the profile could not establish, and why.

It is not a bug prediction. emi-lab found no static score, EMI's included,
that predicts defects better than code size (G2, S1). The profile describes
how verifiable a change would be, and gates nothing. Whether diff coverage
and mutation results should gate `emi task verify` is what emi-lab S9
decides.

```sh
emi profile --repo . --source src                       # structure and history only
emi profile --repo . --source src --coverage coverage.json      # a report you produced at HEAD
emi profile --repo . --source src --link .venv \
    --coverage-command "python -m coverage run -m pytest -q" \
    --coverage-command "python -m coverage json -o cov.json" --coverage-output cov.json
emi profile --repo . --source src --coverage lcov.info --link .venv -- python -m pytest -q -x
emi profile ... --format json -o profile.json           # for `emi task draft --profile`
```

## What goes into it

| Evidence | Source | Per function |
|---|---|---|
| Structure | `emi-panel` (the `graded` profile, as `emi explain`) | SLOC, cognitive complexity, graded erosion, refactoring steps |
| History | `emi hotspots` over `--days` (default 365) | the file's revisions, authors, AI-attributed share, files it changes together with |
| Coverage | `--coverage REPORT`, or `--coverage-command` run by EMI; lcov, Cobertura XML, coverage.py JSON, Go cover profile | executable and executed lines of the function's body (the header line runs on import, so it is not counted) |
| Mutation | the test command after `--` | sampled mutants caught or missed, and the missed ones by line |

Functions are ranked by **exposure**: the file's revisions × the function's
graded erosion (the hotspot product at function level), then graded
erosion.

### Coverage measured by EMI

`--coverage-command` (repeatable) and `--coverage-output` make EMI measure
coverage itself: the commands run in order in one fresh copy of the
profiled commit, under the same sandbox as mutant runs (below), and the
report they write at `--coverage-output` is read back. The evidence then
belongs to exactly the commit profiled, and the JSON records each step
(command, exit code, duration, isolation). Commands are quoted like shell
words but never run by a shell. Examples:

| Stack | Commands | Output |
|---|---|---|
| Python | `python -m coverage run -m pytest -q`, then `python -m coverage json -o cov.json` | `cov.json` |
| Go | `go test -coverprofile=cover.out ./...` | `cover.out` |
| Jest | `npx jest --coverage --coverageReporters=lcov` | `coverage/lcov.info` |

A step that fails, times out or leaves processes behind is not a partial
report: `coverage.error` says which step and why, and every function's
coverage is unknown. A failing test suite fails the step; profile a commit
whose tests pass.

### Coverage import

The report must come from the profiled commit. Its paths are mapped onto
the repository: absolute paths under the repository, paths relative to it
or to a Cobertura `<source>`, and paths from another checkout (a CI
runner's) that end in exactly one repository file. Entries that match no
file are counted as `unmatched`, entries that could name several files as
`ambiguous`; neither is dropped silently. A report that names lines past
the end of a file is `stale` for that file, and its lines are not used.

### Sampled mutation

A mutant is one token changed inside a binary, comparison or boolean
expression (`<` → `>=`, `==` → `!=`, `and` → `or`, `+` → `-`, …) or a
boolean literal flipped. For the most exposed functions
(`--mutate-functions`, default 10), up to `--mutants-per-function`
(default 5) mutants are spread over the lines the tests execute. A mutant
on a line no test runs survives by construction; coverage already says so.

1. The unmutated commit runs first. If its tests do not pass, no mutant
   result is reported: a failing suite would "catch" every mutant.
2. Each mutant then runs in a fresh copy of the commit, like a supervised
   task's check: bubblewrap with no network, the repository hidden (so
   imports come from the copy), `--link` directories mounted read-only,
   process-group cleanup.
3. A mutant is **caught** when the test command fails or times out, and
   **missed** when it passes. A timeout is capped at three times the
   unmutated run (at least ten seconds more).

The runs are sequential and cost one test-suite run each: 50 mutants of a
two-minute suite take over an hour and a half. Sample what you will act on.

## Readiness

| Readiness | When | Before delegating a change here |
|---|---|---|
| `strong` | at least 3 mutants got a verdict and at least 80% were caught | `delegate`: a green run means something here |
| `weak` | fewer than 80% caught, or any mutant missed | `held_out_tests_first` |
| `unexecuted` | coverage shows no line of the function ran | `tests_or_human_review_first`: any change passes the suite |
| `executed` | coverage shows some lines ran; no mutation verdict | `unknown` |
| `unknown` | no coverage and no mutation verdict | `unknown` |

The thresholds are defaults, not calibrated values. One missed mutant is
enough for `weak` because it is a concrete wrong change the tests let
through; the profile lists it (`line 47: \`<\` -> \`>=\``).

## Inferences

- **hard to follow:** cognitive complexity of 15 or more, with the largest
  step `emi explain` proposes;
- **changes often:** at least 3 revisions, among the most changed tenth of
  the scope's files (only in scopes of 10 files or more);
- **changes together:** the file has change-coupling partners.

## Output

`--format json` holds everything: `summary` (readiness and delegation
counts over all functions), `coverage` (report format, files placed,
unmatched and ambiguous entries), `mutation` (the baseline run and the
isolation actually used), `skipped_files` (files that could not be read
or parsed, never profiled from a partial parse), `functions` (the `--top`
most exposed, default 20; `--top 0` lists all) and `change_coupling`.

Agents get the same profile without mutation through the MCP tool
`emi_profile` (docs/agents.md).

## In the PR comment

`emi diff --guardrail --format markdown --profile-json profile.json` adds,
for the functions the change touches, their readiness, lines run and
wrong changes caught, least verifiable first (docs/ci.md). The profile must
be of the change's head (`--top 0`, so every function is in it); one of
another commit is flagged and not used. With `--format json` the same
section is `changed_readiness`.

## From profile to task

`emi task draft` writes a supervised task spec for a change to given
targets (docs/tasks.md). With `--profile profile.json` it warns, before
the task is frozen, where the targets' functions are not `strong`, and
when the profile is of another commit. `--held-out DEST=FILE` adds the
tests the agent must not see: write them where the profile says `weak`.

```sh
emi profile --coverage coverage.json --format json -o profile.json -- python -m pytest -q
emi task draft --target src/pricing.py --objective "..." --profile profile.json \
    --held-out tests/test_pricing_hidden.py=/secure/pricing_hidden.py -- python -m pytest -q > task.json
emi task create --authority ~/.emi-authority --spec task.json
```
