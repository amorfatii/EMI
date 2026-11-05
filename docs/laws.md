# Laws

Laws are tests a person owns that say what the code must do. They live in
their own directory, `.emi/laws/` unless the configuration says otherwise,
and they are plain pytest files: example tables, Hypothesis properties,
or tests of `icontract`/`deal` contracts. There is no new format to learn.

```python
# .emi/laws/test_orders.py
from hypothesis import given, strategies as st

from shop.orders import Order, cancel


@given(st.builds(Order))
def test_a_cancelled_order_is_cancelled(order):
    assert cancel(order).status == "cancelled"


def test_cancelling_twice_is_cancelling_once():
    order = Order(id=1, status="open")
    assert cancel(cancel(order)) == cancel(order)
```

The default directory starts with a dot, so the project's own test command
does not collect it; EMI runs the laws on their own.

## What a check does with them

Every check lays the laws **as the baseline has them** over the candidate
and runs them there and at the baseline, under the same pins as every other
run (docs/determinism.md). A law file the agent edited or deleted is put
back; a law file it added is left out. What each law shows:

| Baseline | Candidate | Status | Action |
|---|---|---|---|
| passed | passed | `holds` | — |
| passed | failed, again on a second run | `broken` | repair |
| passed | failed, then passed on a second run | `nondeterministic` | incomplete |
| failed, errored, or absent | passed | `now_holds` | — |
| failed, errored, or absent | failed | `unmet` | repair |
| any | skipped or `xfail` | `pending` | — |

An open law (failing at the baseline) is the task: the proposer is sent
back until it holds. A law the person is not ready to hold every change to
is marked `@pytest.mark.xfail(strict=True)` and is `pending`.

A law about a function that does not exist yet cannot be collected at the
baseline. Its laws count as errored there, so once the candidate makes them
pass they `now_hold`.

**Counterexamples.** A repair line carries what the failing law said:
Hypothesis's falsifying example when there is one, otherwise the
assertion. For example:

```
.emi/laws/test_absolute.py::test_never_negative is a law that does not hold
yet; make the code satisfy it. It says: Failing test case:
test_never_negative( x=-1, )
```

Hypothesis runs derandomized and with no example database, so the same
code gives the same counterexample every time.

**Law-checked functions.** Each passing law runs once under per-test
coverage. A changed function whose changed lines a passing law executes
lists that law under `laws` in the receipt's `change.functions`. The
summary line also counts them: `Laws: 2 of 2 hold, 1 newly hold, 1 changed
function(s) law-checked.`

## Changing a law

Any change in the laws directory is an item `law:PATH` that waits for a
person (`ask_person`). That includes a modified file, a deleted one, and an
added one. Approve it with:

```sh
emi done --approve law:.emi/laws/test_orders.py
```

The approval is bound to the file's exact content and to the baseline, as
for old tests. Once approved, the candidate's version counts. In enforced
verification (CI), laws are read from the baseline only, so a law change
lands through its own reviewed pull request first.

**Proposals and vacuity.** The agent may draft laws. An unapproved law file
is a proposal: it counts for nothing, but the receipt shows what it would
be worth. Its laws run on the candidate, and for every product function
they execute, EMI tries trivial stand-in bodies:

- returns its first argument unchanged;
- returns `None`;
- returns the default of its return annotation (`0`, `""`, `[]` and so on);
- raises `NotImplementedError`.

A proposal whose laws all still pass under a stand-in is reported, for
example `app.py::normalize returns its first argument (`text`) unchanged`.
This is the identity trap: idempotence alone is satisfied by a function
that does nothing. One check spends at most twelve stand-in runs.

## The lock

With `"laws": {"lock": true}`, the Claude Code hook also refuses the
agent's `Edit`, `Write`, `MultiEdit` and `NotebookEdit` calls under the laws
directory, as they are made. This is fast feedback only. A file written
another way (a shell command) is still restored for every check, which is
the actual guarantee, and the OpenCode adapter has no such hook.

## What laws do not establish

- A law is a test: it holds on the inputs it runs, not on all inputs.
  Hypothesis widens those inputs; it does not prove the property.
- Vacuity is checked only against the four stand-ins, and only for
  proposals. An approved law is taken as the person wrote it.
- Without laws, what the change should do is shown only by the proposer's
  own tests. The receipt says so.
