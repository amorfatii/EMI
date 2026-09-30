# EMI

**EMI tells you whether a code change made your codebase harder to maintain,
and proves it before it merges.**

It measures the structural *burden* of your code: how big and branchy it is,
how much of it is repeated, and how tangled its files are. Then it compares
two versions of your code and blocks a change only when it can prove the
burden grew. Where evidence is missing, EMI says so and shows the range of
possible values. It never fills in a guess.

EMI is built for checking changes made by AI coding agents, and works just as
well on human ones. It runs on Python, JavaScript/TypeScript, Rust and Go.

## What you can do with it

| You want to… | Run |
|---|---|
| See where your codebase is costly and changes often | `emi report` → one HTML file |
| See whether the codebase is getting harder to maintain over time, and whether from more code or denser code | `emi trend` |
| Block a pull request that makes the code worse | `emi diff --guardrail` (or the [GitHub Action](#in-ci)) |
| Hand a task to an AI agent and verify its result | `emi task create` / `verify` / `complete` |
| Know whether your tests would catch a wrong change | `emi profile` |
| See which lines of a change the tests would not notice going wrong | `emi diff-mutate` |
| See where a change now answers differently for inputs the tests pass | `emi drift` |
| Find out why a function is hard to maintain and how to fix it | `emi explain` |
| Enforce your own code rules | `emi check` |

## Install

You need Rust (1.90 or newer; the repository pins its toolchain, so
`rustup` fetches the right one automatically).

```sh
git clone https://github.com/amorfatii/EMI.git
cd EMI
cargo install --locked --path crates/emi-cli   # puts `emi` on your PATH
```

Or build without installing: `cargo build --release` and run
`target/release/emi`.

## Quick start

Run these inside any git repository. Replace `src` with your source folder
and `python` with your language (`python`, `js`, `rust` or `go`).

**1. Get the big picture.** Writes a self-contained HTML page you can open in
any browser or attach to a ticket:

```sh
emi report --source src --probe python -o report.html
```

It shows a map of the code by size and how often each file changes, the
*hotspots* (costly files that keep changing, the best places to start),
what the burden is made of, files that always change together, and rule
findings.

**2. Check a change.** Compare your branch with `main`:

```sh
emi diff --source src --probe python --before main --after HEAD --merge-base --guardrail
```

The verdict is `pass`, `warn` or `block`, with reasons, for example
`absolute burden grew beyond allowance: V +16 (allowance 10)`. The exit code
is what CI reads:

| Exit | Meaning |
|---|---|
| `0` | pass or warn (add `--fail-on-warn` to make warn fail too) |
| `2` | block, or the scan could not cover the whole scope |

Useful variants:

```sh
emi diff ... --worktree            # check uncommitted work instead of a commit
emi diff ... --compact             # a short verdict, cheap for agents to read
emi diff ... --format markdown     # a ready-to-post PR comment
```

**3. See what to improve.**

```sh
emi hotspots --source src --probe python    # ranked files, change coupling, authorship
emi explain src/app/billing.py              # per function: what drives its cost, and the steps that lower it
```

## Common workflows

### In CI

This repository is itself a GitHub Action. It gates each pull request,
posts the verdict as a PR comment and uploads findings to code scanning:

```yaml
- uses: actions/checkout@v4
  with: {fetch-depth: 0}
- uses: dtolnay/rust-toolchain@stable
- uses: amorfatii/EMI@main
  with: {source: src, probe: python}
```

The full workflow, permissions and SARIF upload are in
[docs/ci.md](docs/ci.md).

### Supervising AI agents

- **Supervised tasks.** `emi task create` freezes a task contract (what may
  change, which tests must keep passing, tests the agent never sees).
  `emi task verify` then runs those checks in a sandbox, on a fresh copy of
  exactly the submitted code, with the trusted tests restored read-only.
  Only an accepted commit can complete the task. See
  [docs/tasks.md](docs/tasks.md).
- **Guardrails.** `emi guard` flags deleted or weakened tests, edits to
  prohibited paths, new lint suppressions and duplicated code
  ([docs/guardrails.md](docs/guardrails.md)). Its `gaming.*` rules
  recognise known shapes of test gaming: they are review hints, not
  guarantees, since code written differently passes them. Verification
  relies on the structural checks (restored and held-out tests, the test
  inventory, the envelope), and each decision's `assurance` says which
  kind of evidence it rests on.
- **Agent tools.** `emi mcp` gives agents the gate, explanations and
  hotspots over the Model Context Protocol ([docs/agents.md](docs/agents.md)).

### Test readiness

```sh
emi profile --coverage coverage.json -- python -m pytest -q
```

For each function, `emi profile` combines structure, change history, the
lines your coverage report says the tests run (lcov, Cobertura, coverage.py
or Go), and a sample of deliberately broken versions ("mutants") to see
whether the tests notice. Each function gets a label such as `strong`,
`weak` or `unexecuted`. It is advice only and never blocks anything. Pass
the saved profile to `emi report --profile-json` or `emi task draft` to use
it there ([docs/profile.md](docs/profile.md)). `emi diff-mutate` does the
same for only the lines a change added ([docs/diff-mutate.md](docs/diff-mutate.md)).

### Rules

`emi check src` runs declarative rules: a built-in catalog plus your own
YAML. Suppressions must give a reason and can carry an expiry date, and
baselines let you adopt rules without fixing everything first
([docs/rules.md](docs/rules.md)).

### In your editor

`emi lsp` is a language server. It shows rule diagnostics, erosion hints
and, above each function, how much your edits changed its burden
([docs/editor.md](docs/editor.md)).

## How EMI measures

**Three dimensions of burden**, in line-equivalents:

| Dimension | What it counts |
|---|---|
| **E**: erosion | size and branching |
| **V**: redundancy | repeated code (clones) |
| **C**: coupling | dependencies between files, and dependency cycles |

**Unknowns stay unknown.** When EMI cannot resolve something (say, an
import it cannot trace), it records a range (`12–15`) instead of a single
number, and the gate only blocks on growth it can prove.

**Measurement profiles** (`--profile`):

| Profile | Use it for |
|---|---|
| `parity` | Reproducing the original Python tool (v0.5.1) byte for byte ([docs/parity.md](docs/parity.md)) |
| `precise` | Tracing where values come from to resolve coupling. It removes 97% of unknown coupling on the test corpus |
| `graded` | Erosion weighted by cognitive complexity and Halstead volume. It ranks buggy functions better than `parity` (AUC 0.821 vs 0.745 on 1,014 fresh bugs), though about as well as plain line count |

Details in [docs/profiles.md](docs/profiles.md).

**An honest note on prediction.** The companion `emi-lab` repository tests
what these numbers do and do not predict. So far no static score, EMI's
included, predicts defects better than code size. EMI's value is in
*verification*: proving what a change did, and checking agent work against
a contract it cannot edit.

## Tuning the gate

| Option | Effect |
|---|---|
| `--allow-growth N` | Proven growth tolerated before `block` (default 10) |
| `--allow-headroom N` | Possible (unproven) growth tolerated before `warn` (default: same as `--allow-growth`) |
| `--fail-on-warn` | Exit 2 on `warn` as well |
| `--max-evidence N` | How many evidence entries a verdict lists (default 5; 0 shows margins only) |
| `--log-trajectory DIR` | Append one JSON line per gate run to `DIR/emi-trajectory.jsonl` (`--trajectory-strict` exits 2 if it cannot) |

Before enforcing, `emi backtest --repo . --source src --commits 40` replays
the gate over your recent history, so you can pick allowances that fit.

Scope options for every measuring command: `--exclude GLOB` and
`--include GLOB` (repeatable, matched against paths inside the scope),
`--no-default-excludes` (also measure virtualenv, build and cache
folders), `--no-test-detection` (Python: treat test files as product code),
`--tolerate-syntax-errors` (record unparseable files instead of stopping).

## Speed

Scanning CPython's standard library plus site-packages (799k lines, 35,525
functions) takes 0.14 s on a 20-thread machine. The original Python tool
takes about 58 s on the same tree.

## Documentation

| Topic | |
|---|---|
| CI and the GitHub Action | [docs/ci.md](docs/ci.md) |
| Supervised agent tasks | [docs/tasks.md](docs/tasks.md) |
| Guardrails | [docs/guardrails.md](docs/guardrails.md) |
| Agents and MCP | [docs/agents.md](docs/agents.md) |
| Test readiness profile | [docs/profile.md](docs/profile.md) |
| Mutation on a change's lines | [docs/diff-mutate.md](docs/diff-mutate.md) |
| Behavioural drift | [docs/drift.md](docs/drift.md) |
| History, hotspots, change coupling | [docs/history.md](docs/history.md) |
| Burden over time | [docs/trend.md](docs/trend.md) |
| Rules | [docs/rules.md](docs/rules.md) |
| Refactoring tasks and `emi explain` | [docs/refactoring.md](docs/refactoring.md) |
| Editor integration | [docs/editor.md](docs/editor.md) |
| Languages | [docs/languages.md](docs/languages.md) |
| Measurement profiles | [docs/profiles.md](docs/profiles.md) |
| Parity with the Python tool | [docs/parity.md](docs/parity.md) |

## Contributing

Run `scripts/check.sh` before every commit. It runs formatting, clippy with
warnings denied, the tests, a release build, and the parity check against
the golden measurements in `corpus/`. [CLAUDE.md](CLAUDE.md) lists the rules
the code follows.

<details>
<summary>Crate layout</summary>

| Crate | Role |
|---|---|
| `emi-cli` | The `emi` binary |
| `emi-ir` | Records, evidence intervals, burden formulas, region assignment, Python-compatible text rules |
| `emi-scan` | Scope enumeration and parallel measurement |
| `emi-lang-python` | Python adapter on ruff's parser (tag 0.16.9) |
| `emi-lang-js` | JS/TS adapter on tree-sitter 0.26.2 (grammars js 0.25.0, ts 0.23.2) |
| `emi-lang-rust` | Rust adapter on tree-sitter-rust 0.24.2, with static module coupling |
| `emi-lang-go` | Go adapter on tree-sitter-go 0.25.0; package coupling validated against `go list` |
| `emi-lang-common` | Shared engine for native adapters: SLOC, regions, complexity, erosion, clones |
| `emi-ledger` | Calculator and gate, with CPython-exact arithmetic and JSON |
| `emi-diff` | Verified git snapshots, the change document, backtest, trajectory rows |
| `emi-history` | History mining with rename tracking, churn, authorship, AI attribution, change coupling |
| `emi-rules` | Declarative query rules, built-in catalog, suppressions, baselines |
| `emi-guard` | Guardrails on a change |
| `emi-controller` | Supervised tasks: frozen contracts, isolated check runs, gates, completion |
| `emi-exec` | Check execution: process groups, PID namespaces, verified cleanup |
| `emi-git` | The one git runner, with repository config, hooks and replace refs switched off |
| `emi-profile` | Coverage import, sampled mutation runs, per-function readiness |
| `emi-panel` | Classic function metrics (SLOC, cognitive complexity, Halstead volume) |
| `emi-lsp` | Language server |
| `emi-mcp` | Model Context Protocol server |

</details>

## License

MIT. See [LICENSE](LICENSE).
