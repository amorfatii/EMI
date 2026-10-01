# The done check (`emi init`, `emi done`)

When a coding agent says it is finished, `emi done` says what was proven. It
is the first piece of the plan in `docs/plan.md`; the vocabulary is in
`CONTEXT.md`.

## Set up once: `emi init`

```sh
emi init [--repo .] [--yes] [--force] [--default-branch BRANCH]
```

It detects the language (Python with pytest; anything else is refused with
"unsupported for now"), the source directory, the default branch, the test
command (`.venv/bin/python -m pytest -q` when a virtualenv exists, otherwise
`python -m pytest -q`), the test file globs and the dependency directories to
link (`.venv`, `venv`), prints them, and writes `.emi/config.json`.

- On a terminal it asks before writing. Without a terminal and without
  `--yes` it prints the proposal, writes nothing and exits 2.
- It never replaces an existing file without `--force`.
- Commit the file: enforced verification reads it from the baseline.

The file has the schema `emi-config-0.1` (see plan.md, interface 1). Unknown
keys and values of the wrong type are errors. `repair_rounds` (default 3) and
`on_unproven` (`report`, the default, or `repair`) are for the hook.

## Check: `emi done`

```sh
emi done [--repo .] [--baseline REF [--baseline-source session]] [--candidate REF]
         [--setting cooperative|enforced] [--format json|text] [--approve ITEM]...
```

The candidate is the working tree, or the commit given with `--candidate`.
The baseline is the merge base of HEAD and the default branch. On the default
branch itself there is no such point, so `--baseline` is required (the hook
passes the session baseline and says `--baseline-source session`); EMI does
not guess one.

The tests run through `emi-controller` from a derived contract: the tests the
baseline had and the test-runner configuration are restored to their baseline
versions before the run (so a test edited to hide a bug runs in its original
form), the set of test ids is compared with the baseline's, and everything
runs in a sandbox without network. The contract and its evidence are kept
outside the repository, under `$XDG_STATE_HOME/emi` (default
`~/.local/state/emi`). Nothing is counted against a budget: repair rounds
belong to the hook.

### The receipt

`--format json` prints the receipt in canonical JSON (`emi-done-receipt-0.1`);
`--format text` is the same in plain words and is the default on a terminal.

- **Regression**: `held`, `broken` or `unknown`, with the tests run, the
  failed and missing test ids, and the reasons. `unknown` is never a pass.
- **New behaviour** (`change`): not measured yet; the receipt says so.
- **Changed old tests**: every test file the baseline had that the candidate
  edited or deleted, with whether its original version passes on the
  candidate. Each needs an approval: `emi done --approve old-test:PATH`. The
  approval is bound to that file's exact content in the candidate (a deletion
  is bound to the deletion) and to the baseline, so a further edit asks again.
- **`not_verified`**: always present; what the receipt does not establish.

### The action and the exit status

| Exit | Action | Meaning |
|---|---|---|
| 0 | `stop` | Regression held, no unapproved changed old test. |
| 3 | `repair` | Something the proposer can fix; `repair` names the first failing thing. |
| 4 | `ask_person` | A changed or deleted old test waits for an approval. |
| 5 | `incomplete` | EMI could not measure (no sandbox, no pytest, config absent at the baseline). Not a pass. |
| 2 | | Usage or configuration error. |

"Stop" does not mean "correct": it means the evidence in the receipt holds.

### Settings

- `cooperative` (default): the configuration is the candidate's own, and
  approvals are read from the state directory. The proposer could alter both,
  so this catches a careless proposer, not a cheating one.
- `enforced`: the configuration is read from the baseline revision (a git
  blob), never from the candidate, and `--baseline` is required. If the
  baseline has no `.emi/config.json` the action is `incomplete`. Local
  approvals are not read, so a changed old test stays a review item.
