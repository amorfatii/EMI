# Measurement profiles

A profile is the measurement contract: two measurements are comparable only
under the same profile (and options). `emi scan|diff|backtest --profile …`
selects one; `parity` is the default.

| Profile | Python | JS/TS | Purpose |
|---|---|---|---|
| `parity` | `emi-python-0.1:parity-python-probe-0.12:…` | `emi-js-0.1:parity-js-probe-0.10:…` | Reproduces the v0.5.1 oracle exactly (`docs/parity.md`) |
| `precise` | `emi-python-0.1:precise-0.1:…` | `emi-js-0.1:precise-0.1:…` | Static coupling with value origins (below) |
| `graded` | `…precise-0.1:…+graded-g2` | `…precise-0.1:…+graded-g2` | `precise` coupling; function erosion graded by cognitive complexity and Halstead volume (below). Rust and Go: the native profile `+graded-g2` |

`parity` and `precise` differ only in the coupling dimension (C, region `C`,
`in_cycle`, `resolved_dependencies`, `unresolved_calls`, cycles). Their
erosion, redundancy, SLOC and regions are identical. `graded` is `precise`
with a different erosion for function regions.

## `graded-g2`: erosion that ranks buggy functions

**Formula.** A function region's erosion is

`E = s · [(1 − e^(−(g/s)/k_g)) + (1 − e^(−(v/s)/k_v))] / 2`

- `s`: the function's SLOC;
- `g`: its SonarSource cognitive complexity;
- `v`: its Halstead volume;
- `k_g = 1/14` and `k_v = 15.42`, both frozen by study G1.

**Where the metrics come from.** `s`, `g` and `v` come from `emi-panel`, a port of the lab's metric panel (panel-0.1) on the same pinned grammars. Each is measured without nested functions, as in the lab.

**Which regions it applies to.** A region is graded when a panel function starts on its definition line; when several start there, the outermost is used. Two kinds of region keep the adapter's erosion:
- module and class leftovers, and declarations;
- regions on unparseable lines.

**Evidence (emi-lab `results/G2/REPORT.md`, pre-registered).**
- **Data:** 20 repositories not used in any earlier study (5 each in Python, JS/TS, Go and Rust), 1,014 issue-linked bug fixes.
- **Measure:** the mean within-bug AUC for ranking the functions each fix touched above the other functions of that version.

| Measure | Within-bug AUC |
|---|---|
| graded | 0.821 |
| parity erosion | 0.745 |
| cognitive complexity | 0.772 |

- **Where graded wins:** in all 20 repositories and all 4 languages.
- **What it doesn't do:** it ranks about as well as plain SLOC (0.816) or Halstead volume (0.820). The score is size-weighted, and it doesn't carry a complexity signal that size lacks.
- **What it fixes:** parity erosion ranks buggy functions *worse* than line count.

**Parity checks.**
- `emi panel FILE…` prints the panel metrics. emi-lab `tools/panel_parity.py` compared them with the lab panel on 3,896 files (45,505 functions) and found them identical.
- `emi scan --profile graded` reproduces the G2 values for 3,608 of 3,610 sampled functions. The other two are one-line functions nested on the line of an outer one; the region is the outer function's.

**Where it is the default.** `graded` is the default on the agent surfaces: MCP tools and `emi hotspots`. `emi scan|diff` keep `parity` as the default, and task contracts keep `precise`.

## `precise-0.1`: static coupling with value origins

Parity marks a function's whole coupling unknown when *any* call's receiver
is not a first-party import. A method call on a parameter (`str.indexOf`,
`request.db.query`) is unknown, so 42–75% of SLOC carried unknown coupling on
the corpus and every express diff warned at any headroom.

The precise profile measures **static coupling**: a region depends on the
first-party modules its code reaches through lexically visible names. Every
binding carries the origin of its value:

| Origin | Examples | Calls through it |
|---|---|---|
| module reference | imports, `require('./a').f`, `const f = mod.f`, destructured members, first-party classes and their attributes | dependency on that module |
| external | runtime globals, packages, `require('debug')(…)` | nothing |
| local | literals, functions, classes, call results, instances | nothing (the dependency is counted at the call or `new` site) |
| dispatch | parameters, loop/comprehension items, `with`/`except` targets, fields assigned from them | nothing: dynamic dispatch belongs to the code that obtained the value |
| unknown | conflicting imports, unresolvable or non-literal loaders | unknown |

Scoping follows the language: environments nest (callbacks and closures see
enclosing functions' names), declarations hoist, module-level variables are
bound, `this` in an arrow function is the enclosing method's instance, and
Python modules are named from the enclosing package (`warehouse/` scans as
`warehouse.accounts.models`, so first-party absolute imports resolve).

**What stays unknown**: a call whose target can be first-party code that the
static view cannot identify:
- **Undeclared names:** a global name defined in no scope and not a runtime global.
- **Dynamic module loading:** non-literal or unresolvable `require`/`import()`.
- **Dynamic lookups:** `eval`/`Function`, and `getattr`/`exec`/`__import__`.
- **Unidentifiable ancestry:** `this`/`self` attributes of a class whose ancestry cannot be resolved (a computed base or metaclass), or that defines `__getattr__`/`__getattribute__`.
- **Conflicting imports and star imports.**

A class whose bases are first-party makes an attribute it does not define
depend on those base modules; package or builtin bases contribute nothing.

### Results on the corpus (2026-09-26)

| Repo | Unknown C, parity | Unknown C, precise | Resolved fanout, parity → precise |
|---|---|---|---|
| commander | 75.2% | 0.0% | 22 → 22 |
| eslint | 52.8% | 0.9% | 1661 → 1674 |
| express | 72.6% | 2.5% | 10 → 14 |
| mocha | 59.3% | 3.7% | 388 → 438 |
| parse-server | 42.2% | 0.7% | 1512 → 1583 |
| sails | 72.8% | 2.8% | 176 → 185 |
| warehouse | 60.1% | 3.2% | 62 → 2745 |
| itsdangerous | 62.3% | 5.5% | 50 → 51 |

Unknown-coupling SLOC across the corpus drops **by 97%** (from 98,897 to
3,236 of 181,674 SLOC); the plan's Phase 3 target was 50%. Warehouse's
fanout jump is recall: parity named its modules relative to the scanned
package (`accounts.models`), so every `from warehouse.accounts…` import
looked external.

**Precision.** New edges (precise minus parity) were sampled by hand across
all corpus repos (≈45 edges). Every sampled edge was a reference to
first-party code under the definition above:
- **Inherited `this`/`self` methods**, e.g. parse-server routers reaching `ClassesRouter`.
- **Closure names**, e.g. sails' `setHeaders`.
- **Module-member aliases**, e.g. `var methods = require('./utils').methods`, `const isFunction = astUtils.isFunction`.
- **Package-qualified imports** (warehouse).

An early draft that attributed call *results* to the callee's module
produced false edges (a string id from `cryptoUtils.newObjectId()`), so call
results are local values, as in parity. A labelled 200-edge precision study
belongs to the validation lab.

**Backtest.** Under precise, the 40-pair express backtest passes all 40 pairs
at headroom 50 and above. At headroom 10 it still warns, because of the one
genuinely dynamic `require(mod)` template-engine loader in `view.js`. Under
parity it warns on all 40 pairs at every headroom.

Tests: `crates/emi-scan/tests/precise.rs` pins both directions (what the
profile resolves and what must stay unknown) for each language.
