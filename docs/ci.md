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
          mode: done
          comment: "false"
          sarif: "false"
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
not complete (plan's later isolation/sealing/budget work). Use disposable
workers without secrets; do not use `pull_request_target` to run PR code
with privileged credentials. SARIF findings are optional and unrelated to
whether the completion claim is supported.

## Legacy structural mode

With `mode: burden`, two parts cover a pull request.

1. **Gate.** `emi diff` measures the PR head against its merge base and exits
   2 on a proven burden increase (or an incomplete scan). Warnings keep exit
   0 unless `--fail-on-warn` is set.
2. **Annotate.** `emi scan --format sarif` emits SARIF 2.1.0 (validated
   against the OASIS schema). Code-scanning UIs diff it against the base
   branch, so reviewers see only the alerts the PR introduces.
   - **Rules:** `emi/erosion`, `emi/clone`, `emi/dependency-cycle`, the waste
     and mask rules, and the test-value rules.
   - **Fingerprints:** built from content (function names, clone membership,
     flagged text), so moving code does not reopen alerts.

## GitHub Actions

In structural mode the composite action (`action.yml`) gates the PR
against its merge base, posts or updates a short Markdown verdict comment
(also written to the job summary), and uploads scan plus rule findings as
one SARIF file:

```yaml
name: emi
on: pull_request
jobs:
  emi:
    runs-on: ubuntu-latest
    permissions: {contents: read, pull-requests: write, security-events: write}
    steps:
      - uses: actions/checkout@v4
        with: {fetch-depth: 0}
      - uses: dtolnay/rust-toolchain@stable
      - uses: amorfatii/EMI@main
        with: {mode: burden, source: src, probe: js, sarif: "true", allow-growth: "10", allow-headroom: "50"}
```

This Rust repository explicitly selects structural mode in its dogfood
workflow (`.github/workflows/emi.yml`); completion checks support pytest first.
The structural comment comes
from `emi diff --guardrail --format markdown --link-base URL`. It stays
under 60 lines (checked in tests), and file names link to the PR head.

With `--profile-json profile.json`, a saved `emi profile --top 0 --format
json` of the PR head, the comment adds the test evidence behind each
function the PR changes: its readiness, the lines the tests run, and the
sampled wrong changes they caught or missed. A profile of another commit is
flagged, not used. The section is advisory; it gates nothing.

## GitLab

`ci/emi.gitlab-ci.yml` runs the same job on merge requests. It keeps the
comment and a merged SARIF as artifacts.

## pre-commit

`.pre-commit-hooks.yaml` offers two hooks:
- `emi-check`: the rule catalog, plus `.emi/rules`.
- `emi-gate`: the staged working tree against `HEAD`. It uses `EMI_SOURCE`
  and `EMI_PROBE` from the environment.

The action and templates install emi with `cargo install --git`. Point
their URL at wherever this repository is published.

Calibrate `--allow-headroom` first with `emi backtest` over your recent
history: it prints the pass/warn/block curve per headroom. Choose the
headroom at which routine commits pass (express: 50 under `--profile
precise`).
