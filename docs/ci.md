# EMI in CI

Two jobs cover a pull request.

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

The repository is a composite action (`action.yml`). It gates the PR
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
        with: {source: src, probe: js, allow-growth: "10", allow-headroom: "50"}
```

This repository dogfoods it (`.github/workflows/emi.yml`) on its own
Python tooling (`tools/`) until the Rust adapter exists. The comment comes
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
