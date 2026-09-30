# Behavioural drift (`emi drift`)

A change can pass every test and still answer differently: a function that
used to return `0` for a negative input now raises `ValueError`, or a
division that returned `2.0` now returns `2`. When no test pins that
behaviour, nothing fails, and the difference reaches callers unannounced.
Changes of this kind (a function's input or error contract drifting) are
among the strongest predictors of an agent change causing trouble later.

`emi drift` shows it concretely. For each Python function a change
modifies, it takes the inputs the base commit's own tests pass to that
function, calls the old and the new version on each of them, and lists
every input where the two answer differently:

```
emi drift --base main --head HEAD -- python -m pytest -q
```

```
src/shop/prices.py clamp [compared]: 3 inputs, 3 compared
  input {"args": [-2], "kwargs": {}}
    old {"returned": 0}
    new {"raised": "ValueError"}
```

It is a report: it exits 0 whenever the measurement ran, drift or not
(exit 2 only for errors such as an unknown revision).

## How it works

1. **Targets**: the Python functions `base..head` modifies (the same answer
   as `emi changed-functions`). Module-level functions, static methods and
   class methods of module-level classes are replayed; the rest is counted.
2. **Harvest**: the test command runs in a copy of the base commit with a
   small recorder loaded as `sitecustomize` (its directory goes first on
   `PYTHONPATH`, so pytest, `python -m unittest` and any Python subprocess
   load it without a plugin or a change to the project; a `sitecustomize`
   the environment already had is loaded after it). The recorder wraps each
   target when its module is imported and keeps a call's arguments only when
   every one has a stable JSON encoding: `None`, booleans, integers within
   64 bits, finite floats, strings, and lists, tuples and string-keyed dicts
   of those. Anything else (an object, a set, an enum) is counted as
   `unrecordable`, never converted. Up to 50 distinct inputs are kept per
   function.
3. **Replay**: one fresh process per side imports the function the way the
   tests did, from a fresh copy of each commit, and calls it on every
   recorded input with freshly decoded arguments and a per-call timeout
   (`--call-timeout-s`, default 2). Each call yields `returned` (the value's
   JSON), `raised` (the exception type), `timeout` or `unencodable_result`.
   The base side calls every input twice: when the two answers differ
   (time, randomness, hidden state), the input is counted as `unstable`
   instead of being reported as drift.
4. **Compare**: a drift is an input whose old and new outcomes differ.

Tuples are written as `{"$tuple": [...]}` so a tuple and a list stay
different, in inputs and in results alike.

## Output

`--format json` gives:

- `state`: `measured`; `nothing_to_compare` (no replayable target, or the
  tests passed none of them an input); or `unverified`, with `reasons`,
  whenever something could not be followed through: the test command did
  not start, timed out, failed on the base commit or never loaded the
  recorder, a target could not be imported or found, or inputs were not
  replayed in time. A run that did not happen never reports "no drift".
- `functions`: per target, its `status` (`compared`, `no_inputs`,
  `import_failed`, `not_replayed`, `not_compared`), the number of `inputs`,
  how many were `compared`, up to 10 `drifts` (`input`, `old`, `new`) with
  `more_drifts` counting the rest, `unstable`, and `pinned`.
- `skipped`: targets not replayed and why (`instance_method`, `nested`,
  `async`, `not_found`).
- `counts`: `targets`, `compared`, `with_drift`, `skipped_nested`,
  `skipped_method`, `skipped_async`, `added_or_removed`, `unresolved`,
  `no_inputs`, `unrecordable`, `timeouts`, `import_failures`,
  `unencodable_result`, `unstable`, `not_replayed`.

The copies run like `emi profile`'s: isolated when bubblewrap is available,
network off unless `--network`, the repository hidden, `--link` for ignored
dependency directories (a virtualenv), `--env KEY=VALUE`, and `--timeout-s`
as the bound for the test run and for each side's replay.

## Limits of this version

- **Python only.**
- **Recorded inputs only.** Drift on inputs the base tests never pass is not
  seen; a function the tests never call is listed as `no_inputs`.
- **Instance methods are skipped** (counted as `skipped_method`): replaying
  one needs a `self`, which this version does not construct. Nested and
  async functions are skipped likewise.
- **`pinned` is `"unknown"`.** Whether a test would notice a drift needs the
  function reverted on the head commit and the tests rerun; this version
  does not do that and says so rather than guessing.
- A function in a renamed file is not found on the base side and makes the
  report `unverified`.
