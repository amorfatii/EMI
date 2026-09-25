# EMI

Structural maintenance-burden measurement and change gating, rebuilt in Rust.
EMI measures three burden dimensions per region of code (E: erosion from size
and branching, V: redundancy, C: coupling), keeps missing evidence as explicit
intervals, and gates a change only on burden growth it can prove.

**Status: Phase 1 of the rebuild.** The engine reproduces the v0.5 Python
tool's region, SLOC, complexity and erosion measurements exactly for Python
and JavaScript/TypeScript (`docs/parity.md`). Redundancy, coupling, the burden
ledger and the `diff` gate follow in Phase 2; until then use the Python tool
for gating.

## Use

```sh
cargo build --release
target/release/emi scan path/to/src --probe python   # or --probe js
target/release/emi scan path/to/src --probe js --tolerate-syntax-errors -o scan.json
```

Options: `--exclude GLOB` / `--include GLOB` (fnmatch against scope-relative
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
| `emi-cli` | The `emi` binary |

`corpus/` holds the golden measurements from the v0.5.1 oracle and their pinned
inputs; `tools/parity.py` checks the engine against them.

## Performance

Scanning CPython's standard library plus site-packages (799k lines,
35,525 functions) takes 0.14 s wall-clock on a 20-thread machine; the Python oracle
takes about 58 s for the same tree.
