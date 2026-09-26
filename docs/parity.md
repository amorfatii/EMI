# Parity with the v0.5.1 Python oracle

The Rust engine replaces the Python tool (`~/Projects/emi-v0.4`, tag
`phase0-oracle`) only after it reproduces the oracle's measurements. Two
checks enforce that:

- **Golden mode** — `python3 tools/parity.py`: the eight pinned scopes in
  `corpus/repos.json` are fetched, materialized from raw git blobs, scanned
  with `target/release/emi`, and compared with `corpus/golden/*.json.gz`.
  Needs only git and python3; runs in CI.
- **Live mode** — `python3 tools/parity.py --tree DIR --probe python|js`: runs
  the oracle and the Rust binary on the same tree and compares them. Needs the
  oracle checkout (`EMI_ORACLE`). Used for breadth beyond the corpus.

Values must be equal (floats bit-for-bit); record order is reported separately.

## Fields covered

| Phase | Fields |
|---|---|
| 1 | `components[].{id, sloc, burden.E}`, `functions[].{file, function, line, sloc, cc, erosion_mass}` (+ `lines`, `erosion_exact` for JS), `regions[].{file, id, sloc, sha, E}`, `source_sha256`, JS `fallbacks`, JS syntax-error lines |
| 2 | redundancy (`burden.V`, clone groups, waste rules, test-value findings), coupling (`burden.C`, region `C`/`in_cycle`, cycles, unresolved calls), tsconfig resolution |
| 2 | scope metadata: full `errors` (with raw-byte `sha256`, CPython codec/OS wording, JS `ConfigError` diagnostics), `measurement_options`, Python `status`/`scope_complete`/`inventory`/`inventory_summary`, `.gitignore` selection, refusal messages for empty, missing, and test-only scopes (`tools/scope_cases.py`) |
| 2 | `emi diff` (whole gated report, identity fields aside) and `emi backtest` (golden express curve, checked in CI), trajectory rows |
| 2 | calculator and gate: the whole `review()` report, `guardrail()` verdicts, `compact_verdict`, exit codes, refusal messages |

## The calculator and gate

`tools/ledger_diff.py` fuzzes change documents (exact, unknown and partial
evidence, regions, unchanged components, incomplete scope, unverified
identity, history exposure, and corrupted variants) and requires
`emi review` to print **byte-identical** JSON to the oracle's `review()` +
`guardrail()` under five gate policies, or to refuse with the oracle's own
message. `corpus/golden/ledger.jsonl` records the oracle's answers (output
digests) for 250 cases, which `cargo test` replays without Python.

Floats match to the bit, which needed three CPython behaviors: `math.fsum`
(ported, including its half-even correction), `repr(float)`/`format(x, "g")`
for text, and `x ** 2`, which CPython computes with libm `pow` — not always
the correctly rounded `x * x` (about 0.1% of inputs differ in the last
place). `pyfmt::square` calls the host libm's `pow` like CPython does, so
parity holds against a CPython on the same libm (glibc and musl share the
implementation).

## Results (2026-09-26)

| Tree | Adapter | Functions | Result |
|---|---|---|---|
| Golden corpus (8 repos) | both | 13,232 | identical, same order |
| `/usr/lib/python3.14` (stdlib + site-packages) | Python | 35,525 | identical, same order |
| npm's bundled dependencies | JS | 6,189 | identical, same order (1 D1 file excluded) |
| `~/.opencode/node_modules` (effect-ts etc.) | JS/TS | 24,816 | identical, same order (6 D1 files excluded) |
| TypeScript reference cases (`tools/ts_fixtures.py`) | JS/TS | 58 runs | identical |
| `tools/diff_parity.py`, express (40 pairs, allowance 0 and 10) and the oracle's own history | diff + gate | identical reports and verdicts |
| `backtest-express` golden | backtest | identical rows and summary |
| `tools/ledger_diff.py`, seeds 1–6 | calculator + gate | 15,876 runs | byte-identical output, same refusals |

## Divergences (intentional)

**D1 — JS files containing line breaks other than `\n`.** `js_probe` finds
eligible lines with `str.splitlines()` (which also breaks on U+0085, U+2028,
U+2029, form feed, vertical tab, U+001C–U+001E) but hashes regions with
`split("\n")` and receives rows from tree-sitter, which only knows `\n`. On
such files it raises `IndexError` or attributes lines to the wrong rows. The
Rust adapter uses `\n` lines throughout. Identical on every other file; live
mode excludes D1 files from the comparison and lists them.

**D2 — Parser wording.** Syntax-error messages come from the parser: CPython
says `invalid syntax (a.py, line 1)`, ruff says `Expected an identifier`.
Error entries agree on file, stage, kind, lines and hash; the message of a
`SyntaxError` is not compared, and a raise-mode parse or decode failure only
has to be refused on both sides.

**D3 — Implementation identity in `measurement_options`.** The oracle names
its interpreter (`"python": "3.14.7"`) and its tree-sitter binding
(`"tree-sitter": "0.26.0"`); the engine names itself (`"engine":
"emi-rs-<version>"`) and the tree-sitter core it links (`0.26.2`), under its
own adapter profile. Options stay comparable within one implementation,
which is what a diff needs (both revisions are measured by the same binary);
the portable keys are compared with the oracle.

## Oracle quirks reproduced on purpose

These are behaviors of profile `python-probe-0.12`/`js-probe-0.10` that look
accidental but are kept so measurements stay comparable. Each should be
revisited at the first Rust-native profile bump.

- **Q1 — Python region hashes use `str.splitlines()` lines** while line
  numbers come from `tokenize` (`\n` only). Files with form feeds (common in
  the stdlib) hash shifted text. Deterministic, so region matching still works.
- **Q2 — TypeScript class names read as `<anonymous>`** in qualified ids: the
  TS grammar names classes with `type_identifier`, which the id builder does
  not accept.
- **Q3 — Mixed column units.** Python: token columns in code points compared
  with AST columns in bytes (docstring exclusion). JS: tree-sitter byte columns
  used as code-point indices (comment exclusion). Only matters for non-ASCII
  text before a docstring or comment on the same line.
- **Q4 — A module region is named after the tree root**: `<module>` normally,
  `<anonymous>` when tree-sitter's error recovery produces an `ERROR` root.
- **Q5 — Record order** follows the oracle: regions sorted by span, then
  CPython `ast.walk` breadth-first order (Python; `elif`, `except` and `case`
  each add a level) or start position (JS).
