# EMI

**When your agent says done, EMI tells you what was actually established —
and the same change always gets the same answer.**

A coding agent is a nondeterministic source of changes. EMI is the
deterministic part: at the completion claim it runs the project's tests and
the person's laws against the change, under pinned conditions, and returns a
decision that is a pure function of the code, the specification and the
environment. When the answer is no, the agent is sent back with what failed
and, where there is one, the input that breaks it. **EMI reports a status; it
never merges.**

Python with pytest, on Linux. It needs bubblewrap (without it, a passing
check goes to a person instead of letting the agent stop), and pytest and coverage.py
(7.9 or newer) in the project's environment; Hypothesis for property laws
and for generated inputs. Claude Code and OpenCode V2 adapters are included.
Rust with `cargo test` is checked for regressions only (`"language":
"rust"`, written by hand: `emi init` detects Python); the receipt says
which stages it did not measure.

```sh
emi init                                   # confirm the detected configuration
git add .emi/config.json && git commit -m "Configure EMI"
emi hook install --harness claude-code     # or: --harness opencode
emi done --format text                     # check by hand at any time
```

## What a check establishes

| Part | What it says | Docs |
|---|---|---|
| **Regression** | Every test the baseline had still passes, run in its baseline form: an edited or deleted test is restored, and no test may go missing. | [done.md](docs/done.md) |
| **New behaviour** | Which tests the agent added fail before the change and pass after (proving tests), and which changed functions they exercise. | [done.md](docs/done.md) |
| **Laws** | The person's own tests in `.emi/laws`, held as the baseline has them. A law that held must hold; an open law must come to hold, with Hypothesis's counterexample when it does not. | [laws.md](docs/laws.md) |
| **Kept behaviour** | With `--preserve`, every modified function must answer as before, on the inputs the tests pass it and inputs drawn from its annotations. | [preserve.md](docs/preserve.md) |
| **Determinism** | Every run is pinned; tests that differ between two baseline runs are quarantined; a failure that does not reproduce is not evidence. `emi replay` recomputes a receipt from nothing cached. | [determinism.md](docs/determinism.md) |

The action and the exit status:

| Exit | Action | Meaning |
|---|---|---|
| 0 | `stop` | The evidence in the receipt holds. Not a statement that the task is done. |
| 3 | `repair` | Something the agent can fix, named with what to do. |
| 4 | `ask_person` | A changed old test, runner configuration or law waits for a person's approval, or the checks ran without a sandbox. |
| 5 | `incomplete` | Something could not be measured, or did not reproduce. Never a pass. |
| 2 | | Usage or configuration error. |

Every receipt says what it does not establish (`not_verified`), which
setting it ran in, and the ids of its inputs and its decision.

## A law, end to end

```python
# .emi/laws/test_orders.py  — the person's
from hypothesis import given, strategies as st
from shop.orders import Order, cancel

@given(st.builds(Order))
def test_a_cancelled_order_is_cancelled(order):
    assert cancel(order).status == "cancelled"
```

If the agent's `cancel` returns the order unchanged, the check sends it
back with the counterexample:

```
Action: repair
  .emi/laws/test_orders.py::test_a_cancelled_order_is_cancelled is a law that
  does not hold yet; make the code satisfy it. It says: Failing test case: ...
```

If the agent edits the law instead, the edit changes nothing: the law runs
as the baseline has it, and the edit waits for a person (`law:PATH`). If the
agent proposes a law of its own, the receipt shows whether that law would
still hold were the function trivial (`returns its first argument
unchanged`), before anyone approves it.

## Settings

- **Cooperative** (the default, on a person's machine): the agent could
  alter EMI's inputs, so this catches a careless agent, not a cheating one.
- **Enforced** (`--setting enforced`, in CI): the configuration and the laws
  are read from the baseline and local approvals are ignored. See
  [ci.md](docs/ci.md). It is not yet a claim of adversarial security; run
  untrusted pull requests on an ephemeral worker with no secrets.

## Commands

| Command | |
|---|---|
| `emi init` | Detect the test command, source layout and default branch; write `.emi/config.json` |
| `emi done` | Check the working tree (or `--candidate REF`) against the baseline; print the receipt |
| `emi done --approve ITEM` | Approve a changed old test, runner configuration file or law (`law:PATH`) |
| `emi done --preserve [PATH]...` | Also require the modified functions to keep their behaviour |
| `emi intent preserve\|clear\|show` | Record that intent for this repository, for the hook |
| `emi replay RECEIPT` | Compute a receipt again from nothing cached; exit 0 when the decision is the same |
| `emi hook install\|uninstall` | Run the check when the agent tries to stop ([hook.md](docs/hook.md)) |
| `emi journal report` | What the hook did, and a person's labels of its interventions |

## Install

You need Rust 1.90 or newer (the repository pins its toolchain).

```sh
git clone https://github.com/amorfatii/EMI.git
cd EMI
cargo install --locked --path crates/emi-cli
```

## Not established yet

The controlled experiment, with and without the hook, has not been run:
there is no claim yet that EMI lowers the false completion rate, keeps the
solve rate or bounds the extra tokens. The pre-registered design is in the
validation lab, outside this repository.

## Contributing

Run `scripts/check.sh --fast` while working: formatting, clippy with
warnings denied, the kernel purity check and the tests that run no real
pytest, in about a minute. `scripts/check.sh` adds the integration tests
(real projects, real pytest) and a release build, which is what CI runs,
split over three machines. With
[cargo-nextest](https://nexte.st) installed, the full run executes the
test binaries side by side.
[CLAUDE.md](CLAUDE.md) lists the rules the code follows, the vocabulary is
in [CONTEXT.md](CONTEXT.md), and the decisions are in [docs/adr](docs/adr).

<details>
<summary>Crate layout</summary>

| Crate | Role |
|---|---|
| `emi-kernel` | The decision: typed facts, the action table, the policy id; canonical JSON, hashing, globs, test paths, pins. No I/O. |
| `emi-git` | The one git runner, with repository config, hooks and replace refs switched off |
| `emi-tree` | Verified snapshots of commits and working trees; line-level hunks |
| `emi-exec` | Sandboxed execution: bubblewrap, process groups, verified cleanup |
| `emi-driver` | The boundary between the checks and a language: the `Driver` trait and the evidence its stages return (ADR 0009) |
| `emi-rust` | The Rust driver, regression first: `cargo test` run and read test by test; every other stage is reported as not measured |
| `emi-python` | The Python driver: function spans and mutants (tree-sitter-python), changed functions, the proving measurement, laws and stand-ins, coverage, behaviour comparison, pytest's output |
| `emi-done` | The done check, the same for every language: configuration, baseline, the verification engine, regression, laws, preserve, receipts, replay. It depends on no language crate. |
| `emi-hook` | Harness adapters (Claude Code, OpenCode V2), repair rounds, the laws lock |
| `emi-cli` | The `emi` binary |

The burden measurements EMI started with are archived at tag
`archive/burden-final` ([ADR 0005](docs/adr/0005-the-burden-stack-is-archived.md)).

</details>

## License

MIT. See [LICENSE](LICENSE).
