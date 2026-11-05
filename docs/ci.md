# EMI in CI

## Completion evidence (default)

The composite action runs `emi done --setting enforced` against immutable
baseline and candidate refs. It reads configuration from the baseline,
ignores local approvals, writes one JSON receipt and job summary, and uploads
both as an artifact. Only `action: stop` passes; repair (3), review (4),
incomplete (5), and usage errors (2) all fail the job. EMI never merges.

Commit `.emi/config.json` before creating the task branch. The caller must
prepare the project's test environment, including pytest and coverage.py
>=7.9, before calling the action. Ubuntu workers need working bubblewrap.
Pin the action and binary to a reviewed EMI revision; do not build EMI from
the untrusted PR's sources as an authoritative check.

```yaml
name: emi-completion
on: pull_request
jobs:
  completion:
    runs-on: ubuntu-latest
    permissions: {contents: read}
    steps:
      - uses: actions/checkout@v4
        with: {fetch-depth: 0}
      - uses: dtolnay/rust-toolchain@stable
      - run: python3 -m venv .venv
      - run: .venv/bin/pip install -e '.[test]' pytest 'coverage>=7.9,<8'
      - uses: amorfatii/EMI@main # pin to a reviewed commit when deployed
        with:
          comment: "false"
```

Adapt dependency installation to the repository. Add this job to branch
protection as a **required status**. The action cannot configure that setting
for you. Outputs are `action`, `regression`, and `change`; the artifact is
`emi-completion-receipt`. The base and head SHAs default to the PR's refs.
Outside a PR, pass `baseline` explicitly and optionally `candidate` (default
HEAD). `comment: true` needs `pull-requests: write` and may fail for forks.

Changed old tests and runner configuration remain review items in CI: this
version has no privileged review-approval channel. It intentionally fails
the required job rather than silently treating review as verified.

The `enforced` input choice is implemented, but adversarial CI hardening is
not complete: isolation before every execution, sealed inputs and durable
budgets are later work (ADR 0004). Use disposable workers without secrets;
do not use `pull_request_target` to run PR code with privileged credentials.

## GitLab

`ci/emi.gitlab-ci.yml` runs the same enforced check on merge requests and
keeps the receipt as an artifact. Prepare the project's test environment
before it, as for the GitHub Action.

## Laws and intent in CI

Enforced verification reads the laws from the baseline, so a change to a
law lands through its own reviewed merge first; in the change it is a
review item (`ask_person`). To require that a refactoring keeps behaviour,
pass `--preserve` in the job; a machine's recorded intent is not read.

The action and templates install emi with `cargo install --git`. Point
their URL at wherever this repository is published, and pin it to a
reviewed revision.
