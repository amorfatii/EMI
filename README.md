# EMI

Structural maintenance-burden measurement and change gating, rebuilt in Rust.
EMI measures three burden dimensions per region of code (E: erosion from size
and branching, V: redundancy, C: coupling), keeps missing evidence as explicit
intervals, and gates a change only on burden growth it can prove.

**Status: Phase 2 of the rebuild.** The engine reproduces the v0.5.1 Python
tool for Python and JavaScript/TypeScript end to end: measurements (erosion,
redundancy, coupling, regions, scope metadata), the burden ledger and review
report (byte-identical), the block/warn/pass gate, `emi diff` between git
refs and `emi backtest` over history (`docs/parity.md`). It can replace the
Python tool for scanning and gating.

## Use

```sh
cargo build --release
target/release/emi scan path/to/src --probe python   # or --probe js
target/release/emi scan path/to/src --probe js --tolerate-syntax-errors -o scan.json
```

Gate a change between two refs (exit 0 pass/warn, 2 block or incomplete scope;
`--fail-on-warn` makes warn exit 2):

```sh
target/release/emi diff --repo . --source src --before main --after HEAD --merge-base --guardrail
target/release/emi diff ... --guardrail --compact          # token-cheap verdict for agents
target/release/emi review change.json --guardrail          # gate a saved change document
target/release/emi backtest --repo . --source src --commits 41 --headroom 10 --headroom 50
```

Rules: `emi check src` runs declarative tree-sitter rules (a first-party
catalog plus your YAML) with reasoned, expiring suppressions and baselines;
`emi sarif-merge` combines tools' SARIF (`docs/rules.md`).

CI: `emi scan --format sarif` feeds code-scanning UIs; `docs/ci.md` has a
GitHub workflow that gates PRs and uploads annotations.

Supervised tasks: `emi task create|verify|complete` freezes a task
contract outside the repo and verifies candidates by running its checks in
a sandbox with verified cleanup; only an accepted commit completes the task
(`docs/tasks.md`).

Agents: `emi mcp` serves the gate, explanations, hotspots and refactor
acceptance over the Model Context Protocol (`docs/agents.md`); `emi diff
--worktree --guardrail` gates uncommitted work from the command line.

History: `emi hotspots --repo . --source src --probe js` ranks files by
burden × change frequency, with change coupling, authorship and AI-attributed
share (`docs/history.md`); `emi diff --history` adds the churn-weighted
exposure view to reviews.

Profiles: `--profile parity` (default) reproduces the v0.5.1 oracle;
`--profile precise` measures static coupling with value origins and cuts
unknown coupling by 97% on the corpus (`docs/profiles.md`).

Gate policy: `--allow-growth UNITS` (proven growth tolerated, default 10),
`--allow-headroom UNITS` (possible growth behind missing evidence),
`--max-evidence N`, `--log-trajectory DIR` (+ `--trajectory-strict`).

Scan options: `--exclude GLOB` / `--include GLOB` (fnmatch against scope-relative
paths, repeatable), `--no-test-detection` (Python: measure test files as
product code), `--no-default-excludes` (do not prune venv/build/cache dirs).
Exit code 2 on any error.

## Layout

| Crate | Role |
|---|---|
| `emi-ir` | Records, evidence intervals, burden formulas, region assignment, Python-compatible text rules |
| `emi-lang-python` | Python adapter on ruff's parser (tag 0.16.9) |
| `emi-lang-js` | JS/TS adapter on tree-sitter 0.26.2, grammars js 0.25.0 / ts 0.23.2 |
| `emi-scan` | Scope enumeration and parallel measurement |
| `emi-ledger` | Calculator (ledger, review report) and gate, with CPython-exact arithmetic and JSON |
| `emi-diff` | Verified git snapshots, the change document, backtest, trajectory rows |
| `emi-rules` | Declarative query rules, first-party catalog, suppressions, baselines |
| `emi-exec` | Check execution: process groups, subreaping, PID namespaces, verified cleanup receipts |
| `emi-controller` | Supervised tasks: frozen contracts, envelope/structural gates, requirement ledger, completion |
| `emi-mcp` | Model Context Protocol server exposing EMI's tools to agents |
| `emi-history` | History mining with rename tracking, churn/authorship/AI attribution, change coupling, hotspots |
| `emi-cli` | The `emi` binary |

`corpus/` holds the golden measurements from the v0.5.1 oracle and their pinned
inputs; `tools/parity.py` checks the engine against them.

## Performance

Scanning CPython's standard library plus site-packages (799k lines,
35,525 functions) takes 0.14 s wall-clock on a 20-thread machine; the Python oracle
takes about 58 s for the same tree. The 40-pair express backtest runs in 0.5 s.
