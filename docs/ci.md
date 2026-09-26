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

```yaml
name: emi
on: pull_request
jobs:
  emi:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0            # merge base and history
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install --git <emi repository> emi-cli --locked
      - name: Gate
        run: >
          emi diff --repo . --source src --probe js --profile precise
          --before origin/${{ github.base_ref }} --after HEAD --merge-base
          --guardrail --compact --allow-growth 10 --allow-headroom 50
      - name: Annotate
        if: always()
        run: emi scan src --probe js --profile precise --format sarif -o emi.sarif
      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: emi.sarif
          category: emi
```

Calibrate `--allow-headroom` first with `emi backtest` over your recent
history: it prints the pass/warn/block curve per headroom. Choose the
headroom at which routine commits pass (express: 50 under `--profile
precise`).
