# Determinism

EMI's promise is **deterministic acceptance**: the same candidate, under the
same configuration, laws, environment and pins, always yields the same
decision on the same evidence. This page says what holds that in place and
how to check it.

## Pins

Every test run is held to the same pins, at the baseline and on the
candidate: regression runs, proving runs, law runs and behaviour
comparisons.

| Pin | Default | Config key |
|---|---|---|
| `PYTHONHASHSEED` | `0` | `pins.hash_seed` |
| `TZ` | `UTC` | `pins.tz` |
| `LC_ALL` | `C.UTF-8` | `pins.locale` |
| `random` and numpy seeded before each test from a hash of the test's id | on | `pins.seed_random` |
| Hypothesis: derandomized, no example database, no deadline, no literals drawn from other modules imported in the run, the first failure reported, no explain phase, shrinking bounded by a count of calls (1000) instead of a five-minute clock | on | `pins.hypothesis` (`derandomize` or `off`) |
| no bytecode written, pytest-randomly and the cache provider off | always | — |

The in-process pins are applied by a small pytest plugin, `emi_pins`, which
EMI places in every copy it runs in and loads with `-p emi_pins`. A test
command that runs tests in parallel (`-n`, `--numprocesses`, `--dist`) is
refused, because the order and what the processes share cannot be pinned.

The plugin sets these on every collected Hypothesis test, `@given`
functions and state machines alike, over whatever else the test set. A
profile alone would not reach them: `@given` takes the default settings
when its module is imported, before any plugin hook after collection, and
a state machine's `TestCase` carries its own. Hypothesis also draws
integers and strings found in the project modules imported so far; that
would make a test's inputs depend on which tests ran before it, so the
plugin turns it off. Hypothesis ends three things on a clock: looking for
further failures after the first (ten seconds), shrinking (five minutes)
and the deadline. Each would make what a run reports depend on the
machine's speed, so the first is off, the second is a count, and the third
is off.

A run that only confirms a failure (a law's second run, failed tests run
by themselves) sets `EMI_PIN_CONFIRM=1`: Hypothesis stops at its first
failure instead of shrinking it again. Generation is derandomized, so that
is the failure the first run found; whether it fails is all the second run
is asked.

The pins, and the digest of the plugin that applies them
(`pins_plugin`), are recorded in the receipt's `inputs`.

## What a decision is a function of

The receipt (`emi-done-receipt-1.0`) names its inputs:

- the baseline commit and the candidate's tree;
- the configuration's digest, the pins and the pins plugin's digest;
- the Python environment's digest: the interpreter, the architecture, and
  every installed distribution with its version;
- the laws' digest;
- the intent (`preserve`);
- the setting;
- the version of EMI and of its decision rules (`policy`, `emi-policy-3`).

From these come `input_id`. From everything in the receipt except where it
ran (`meta`) comes `decision_id`. The decision itself is computed by
`emi-kernel`, a crate that does no I/O and whose action table is tested
exhaustively. `scripts/kernel-is-pure.sh` keeps it that way.

## Nondeterminism is never resolved by luck

- **At the baseline,** the test command runs twice under the same pins. A
  test whose outcome differs between the two runs is **quarantined**: it
  is listed in `regression.quarantined` and is not regression evidence
  either way.
- **On the candidate,** a failure is confirmed before it counts. The failed
  tests run alone; if they pass alone and nothing was restored, the whole
  suite runs again under the same pins. A failure that shows again is a
  break. One that does not is **nondeterministic**: it is listed, the
  decision is `incomplete`, and a failure that did reproduce still counts.
  A lucky rerun never turns a failure into a pass.
- A law that held at the baseline and fails on the candidate is run once
  more. If it then passes, it is `nondeterministic` too.

What the pins do not hold can still differ: the clock, the network (off in
the sandbox), another process, files outside the copy. The receipt says so.

## Checking it: `emi replay`

```sh
emi done --format json > receipt.json
emi replay receipt.json
```

`emi replay` computes the receipt again in a fresh state directory, holding
only the person's approvals, so nothing recorded is reused. It runs both
baseline runs, the candidate, the laws and the comparison again, chooses
the baseline the way the original did, and prints whether `decision_id` is
the same (exit 0) or not (exit 1). When it differs, it names every part of
the receipt that differs. A working tree that changed since the receipt
cannot be replayed; commit the candidate and check it with `--candidate`.
