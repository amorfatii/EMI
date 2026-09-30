# Mutation on a change's lines (`emi diff-mutate`)

A passing test run says the tests did not fail. It does not say whether
they would have noticed the changed code going wrong. `emi diff-mutate`
answers that for the lines a change wrote: it makes small wrong versions of
them ("mutants") and runs the tests against each.

```sh
emi diff-mutate --repo . --base origin/main --head HEAD -- python -m pytest -q -x
emi diff-mutate --base HEAD~1 --max-mutants 40 --link .venv --format json -- python -m pytest -q
```

It is a report, never a gate: the exit code is 0 whenever the measurement
completed, with survivors or not, and non-zero only for usage or I/O
errors.

## What gets mutated

- Only functions the change added or modified, and within them only the
  lines the change added. The rest of a modified function is not mutated:
  the question is about what this change wrote. Added lines outside any
  function (module-level code) are not mutated.
- The mutants are those of `emi profile` (docs/profile.md): one token
  changed inside a binary, comparison or boolean expression, or a boolean
  literal flipped. A line inside nested functions belongs to the innermost.
- The mutants are read from the head; a changed file whose head version
  cannot be read is listed under `not_mutated.unreadable`, never skipped.

## How it runs

Exactly as `emi profile`'s mutation stage: the unmutated head runs first,
then each mutant in a fresh copy of the head, isolated (bubblewrap, no
network unless `--network`, the repository hidden, `--link` directories
read-only, process-group cleanup), bounded by `--timeout-s` and by three
times the unmutated run. `--env KEY=VALUE` sets the test environment.

A mutant is **killed** when the tests fail, **survived** when they pass,
**timed out** when it does not finish, and an **error** when its copy
could not be prepared or the run could not be verified clean.

## Picking `--max-mutants`

Each mutant costs one test run, one after another: 20 mutants of a
two-minute suite take 40 minutes. The default is 20. When the change has
more candidates, the sample is spread over the changed functions: each
function gets one mutant before any gets a second, and so on; within a
function the mutants are spread evenly over its added lines. With fewer
mutants than functions, the functions are spread evenly over the change
(file, then line). The same change and budget always pick the same
mutants. Use a fast, focused test command (`-x`, the relevant test
directory) and raise the budget for changes you will read closely.

## Reading the result

Survivors are places to look, not defects. A surviving mutant may be
**equivalent** to the original (`x >= 0` vs `x > 0` where `x` is never 0),
which no test can kill. The common case is still a concrete wrong change
the tests let through: add an assertion that would fail on it, or decide
the line does not matter.

| `state` | Meaning |
|---|---|
| `measured` | the baseline passed and the sampled mutants ran |
| `unverified` | the unmutated head's tests did not pass; `baseline` says how, and every count is `null` (a failing suite would "kill" every mutant) |
| `nothing_to_mutate` | the change added no mutable line in a function of a supported language; nothing ran |

`--format json` (the canonical encoder):

```json
{"state": "measured", "base": "<sha>", "head": "<sha>",
 "candidates": 7, "run": 7, "killed": 5, "survived": 2, "timed_out": 0, "errors": 0,
 "survivors": [{"file": "src/pricing.py", "function": "shipping", "line": 8,
                "original": "<", "replacement": ">="}],
 "not_mutated": {"unparsed": [], "unsupported": ["README.md"], "unreadable": [], "removed_only": 1},
 "baseline": {"passed": true, "status": "exited", "exit_code": 0, "...": "..."},
 "isolation": "bwrap",
 "note": "a surviving mutant may be equivalent to the original; treat survivors as places to look, not defects"}
```

`not_mutated` lists what no mutant was drawn from: files that did not
parse on a side, files in no supported language, head files that could
not be read, and `removed_only`, the changed functions to which the change
added no line. The text format prints one line per survivor and a summary.
