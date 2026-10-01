# Codebase trend (`emi trend`)

`emi diff` judges one change. `emi trend` answers a slower question: over
the last months, has the code become harder to maintain, and if so, is that
because there is more of it or because what is there got denser?

```
emi trend --repo . --source src --since 2026-01-01
emi trend --repo . --source src --since v1.4 --until main --step 10 --format md -o trend.md
```

It walks the first-parent commits after `--since` (a ref, or a date read as
UTC midnight; the `--since` commit itself is not measured) up to `--until`
(default `HEAD`), measures the scope at each one, and writes one row per
commit and a summary. It is a description of the history, not a quality
verdict: it gates nothing and exits 0 whenever the walk ran (2 when the
refs cannot be resolved or the range holds no commit).

## What each row holds

Each commit is measured from its raw tracked blobs, exactly as `emi diff`
measures one side of a change (same `--probe` and `--profile` defaults:
`python`, `parity`).

```json
{"sha": "…", "date": "2026-03-02T10:14:05+01:00", "state": "measured",
 "sloc": 18231, "burden": 2210.4, "files": 214,
 "guard_warnings": {"suppress": 2, "tests": 1}, "clones": 37,
 "unmeasured": [{"kind": "burden", "file": "api/views.py", "dimension": "C", "unknown_sloc": 41}]}
```

| Field | Meaning |
|---|---|
| `date` | The commit's committer date, ISO 8601 with a numeric offset (`+00:00` for UTC, never `Z`), whatever git version wrote it |
| `sloc`, `files` | Source lines and measured files in the scope |
| `burden` | Total E + V + C mass over the scope: the measured part only |
| `guard_warnings` | Guardrail findings (`emi guard`, default policy) of the step from the previous row's commit to this one, counted by family; `null` when the guard could not run |
| `clones` | Clone groups in the scope (exact clones, as the measurement reports them) |
| `unmeasured` | What the totals leave out: burden evidence with unknown lines (`kind: burden`, with its `dimension` and `unknown_sloc`), files that could not be read or parsed (`kind: file`), and guard checks that could not run (`kind: guard`) |

A commit that cannot be measured (a syntax error without
`--tolerate-syntax-errors`, an unreadable tree) is still a row:
`{"sha", "date", "state": "unverified", "error", …}`. The walk never skips
a commit silently, and an unverified commit never gets totals.

Burden that EMI cannot resolve (say, coupling behind an import it cannot
trace) is listed, not guessed: `burden` counts only the known mass, and the
same code measured with more evidence can only add to it. Compare rows
measured with the same profile.

## The summary

JSONL ends with `{"summary": …}`: how many commits were measured and
unverified, the totals at the first and last measured commit, their
change, the guard warnings summed over every step (and how many steps the
guard could not check), and the size decomposition of the burden change:

```
Δburden = ΔSLOC × first density + last SLOC × Δdensity      (density = burden / SLOC)
```

The first term is the growth explained by **more code** (the new lines at
the old density), the second the growth explained by **denser code** (the
change in burden per line, over today's size). The two terms add up to the
burden change exactly; each is reported with its share of it. A share can
exceed 100% when the other term has the opposite sign (the code grew but
got simpler per line), and is undefined (`null`) when the burden did not
change. The split needs two measured commits with source lines; otherwise
`decomposition` is `null` and `decomposition_unavailable` says why.

`--format md` gives a table of the commits (date, short SHA, SLOC, burden,
guard warnings, clones) and the summary as one paragraph.

## Why totals and the split, not a density

Burden grows with size: a bigger codebase carries more of it even when
every function is as simple as before, and per-file studies (including
this project's own) find size-weighted burden ranks files almost exactly
like SLOC. A falling density can therefore hide a codebase that doubled,
and a rising one can come from deleting simple code. So the trend always
shows the raw totals, and density appears only inside the decomposition,
next to them: "burden grew by 400; 300 of that is the 6,000 new lines,
100 is the existing code getting branchier" is a statement the numbers
support; "density rose 3%" alone is not.

## Cost

Every selected commit is measured in full, and every step is checked by
the guard, so a walk costs about one `emi scan` and one `emi guard` per
commit (commits run in parallel). For long histories, measure every Nth
commit with `--step N`; the first and last commit of the walk are always
measured, and each guard step then covers everything between two measured
commits.
