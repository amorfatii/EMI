# Completion hooks

After `emi init`, commit `.emi/config.json` and install one adapter:

```sh
emi hook install --harness claude-code
emi hook install --harness opencode
```

Installation is project-local by default; `--scope user` installs for the
user. It preserves unrelated settings and refuses foreign files. To remove:

```sh
emi hook uninstall --harness opencode
```

Restart or reload the harness after installation. Configure dependencies
before the session: Linux bubblewrap, the project's pytest, and coverage.py
>=7.9. EMI does not install project dependencies during a check.

- **Claude Code:** command hooks for `UserPromptSubmit` and `Stop`, stored
  in `.claude/settings.json`. A repair blocks Stop and gives one short fix.
- **OpenCode V2:** a native plugin in `.opencode/plugins/emi/index.js`. It registers
  a prompt-admission hook and subscribes to `session.execution.succeeded`
  (`session.idle` where provided). OpenCode's turn
  has already ended, so repair submits a marked follow-up prompt; it cannot
  block the original stop. Receipts are marked queued prompts (`resume: false`):
  visible in the queue, not a new model turn. They enter the transcript on the
  next user-started turn. Non-resuming synthetic notes were not immediately
  visible on V2 2.0.18. Child sessions, other locations, provider failures and
  interrupted turns are not treated as completion claims. V1 is not supported.

The first user message records the session baseline; later user messages
reset the default three repair rounds. EMI's own marked repair prompts do
not reset the counter. On a feature branch the branch baseline takes
precedence. State lives outside the repository under the user's state directory.

An unchanged tree after a completed check is skipped. Repair checks are not
cached as successful. When rounds run out the agent may stop, but the person
sees **NOT VERIFIED**. Environment faults are **INCOMPLETE**, never a pass.
Changes to old tests or runner configuration ask the person once per exact
file version. Nothing auto-approves those changes or merges a branch.

The product checks the adapters with a scripted proposer and real pytest.
The actual OpenCode V2 server is exercised separately against a local scripted
provider (no model, account, or paid call):

```sh
cargo test --locked -p emi-cli --test opencode_v2 native_harness -- --ignored --exact
```

This check requires OpenCode V2 installed; the ordinary suite skips it explicitly.
It confirms two repairs, round numbering, and a final queued receipt with no
fourth provider call. That tests integration, not usefulness. The experiment and owner
trial remain separate release evidence; see `docs/plan.md`.

## Warming the baseline

The first check of a task runs the test command twice: once on the baseline,
to have something to compare with, and once on the candidate. When the
person sends a message, the hook therefore starts `emi done --warm` in the
background and returns at once. That run executes the baseline's tests and
keeps the result; by the time the agent says it is finished, the check runs
the candidate only. A check that arrives while the warm run is still going
waits for it instead of running the suite beside it.

Warming is best effort: a warm run that fails changes nothing, since the
check runs the baseline itself and reports what went wrong. The result is
kept per baseline, configuration and dependencies, so later messages in the
same session start nothing. Set `EMI_HOOK_WARM=0` to turn it off.

## The journal

Every check the hook runs is appended to a journal outside the repository
(under the user's state directory, one file per repository): when it ran, the
session, the action, which rule it rested on, the repair round, how long it
took, and the receipt in counts. Checks skipped because nothing changed are
not recorded.

```sh
emi journal report                 # checks, interventions by rule, time, labels
emi journal label 3f2a9c useful    # or: noise, wrong   [--note "why"]
```

An intervention is a check whose action was not `stop`. Labelling them is how
a person records whether the hook was worth its time: `useful` (it caught
something, or said something needed), `noise` (right, not worth the time),
`wrong` (mistaken). The report lists the ones not labelled yet.

## Crash reports

Test suites crash child processes on purpose. EMI starts its commands with a
core limit of one byte, which keeps those crashes out of the system's crash
reports (and off the desktop); `emi` itself therefore leaves no core dump
either.

The limit only works while it stays one byte, and a program that crashes on
purpose often sets it to zero first (Python's own crash-test helpers do),
which hands the crash to the system's handler again. Inside the sandbox a
command therefore cannot change the core limit: the call fails with a
permission error and everything else is allowed. A test that needs to set
that limit fails in the sandbox the same way at the baseline and on the
candidate, so it is not counted as a regression. Without bubblewrap there is
no such filter.
