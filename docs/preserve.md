# Keeping behaviour (`--preserve`)

When the change is a refactoring, the person asks for the behaviour to be
kept, and the law that follows needs no writing: every function the change
modifies must answer as the baseline's does.

```sh
emi done --preserve                  # every modified function
emi done --preserve src/billing      # only those under src/billing
emi intent preserve src/billing      # recorded for this repository (the hook reads it)
emi intent show
emi intent clear
```

Only the person declares the intent. In enforced verification only the flag
applies (a CI input), never what a machine recorded.

## How it compares

For each modified module-level function, static method or class method:

1. **Recorded inputs.** The baseline's tests run once with a recorder, which
   keeps the arguments the tests pass the function. Only arguments with an
   exact JSON encoding are kept: `None`, booleans, integers, finite floats,
   strings, lists, tuples, and dicts with string keys.
2. **Generated inputs.** When every parameter is annotated, Hypothesis draws
   inputs from those types (`preserve.generated_examples`, default 200). It
   is derandomized, so the same signature always gives the same inputs.
3. Each input runs against the baseline's code and the candidate's, in
   separate processes, under the same pins, each call under its own bound
   (`preserve.call_timeout_s`, default 2 s). The baseline runs every input
   twice. A function that answers one input two ways is depending on the
   clock, randomness or hidden state; that input is not compared, and the
   receipt shows the function is not `deterministic`.

An input where the two versions return different values, raise different
exceptions, or one returns where the other raises is a **divergence**. So
is an input the baseline answered and the candidate no longer finishes in
time, which is non-termination observed rather than guessed. Divergences
send the proposer back with the input and both answers:

```
app.py::sign answers differently from the baseline for the input
{"args":[0],"kwargs":{}}: it returned "zero" and now returned "negative".
The person asked for its behaviour to be kept.
```

The receipt's `preserve` object lists the divergences and, per function,
the inputs compared and whether the baseline answered deterministically. It
also lists what could not be compared.

## What it does not establish

This is evidence on the compared inputs, never a proof of equivalence, and
the receipt says how many inputs there were. Some functions are not
compared at all and are listed as such:

- instance methods, async functions and nested functions;
- functions the change added or removed;
- functions whose inputs had no stable encoding;
- functions in a file that does not parse.

A function whose signature changed usually cannot be called with the old
inputs: it shows up as divergences (a `TypeError`), which is the honest
answer.
