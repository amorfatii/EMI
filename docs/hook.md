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
