# EMI — rules for agents working in this repository

EMI checks code changes made by AI coding agents and returns a decision that
is a pure function of its inputs (see README.md; the vocabulary is in
CONTEXT.md and the decisions in `docs/adr/`). The burden measurements EMI
started with are archived at tag `archive/burden-final` (ADR 0005).

## Before you commit
- Run `scripts/check.sh` (fmt, clippy -D warnings, kernel purity, tests,
  release build). Never pipe it through `tail`/`grep` when deciding whether
  it passed; the exit status is the verdict.
- A change to what a receipt says in one of the situations of
  `crates/emi-cli/tests/characterize.rs` changes that table in the same
  commit, on purpose, with the reason in the message.
- A change to the action table, or to what `emi_kernel::decide::Facts`
  holds, bumps `emi_kernel::decide::POLICY`.

## Rules
- The kernel (`emi-kernel`) does no I/O: no process, file, network, clock or
  environment access, and no dependency beyond encoding and hashing.
  `scripts/kernel-is-pure.sh` checks it. Decisions are made there and only
  there.
- A gate or check never reports pass/satisfied for input it could not
  resolve: return an explicit unknown (an `incomplete` row, a `not_measured`
  entry) and say what was unresolved.
- Never turn a failure into a plausible value (`""`, `0`, `"<unreadable>"`,
  an empty list). Return an error or an explicit unknown.
- An outcome that differs between two runs under the same pins is evidence
  for neither side. Never clear a failure because a later run passed
  (ADR 0007).
- Every test run goes through the pins (`emi_kernel::pins`, the `emi_pins`
  plugin): the same at the baseline and on the candidate.
- Laws are the person's. Nothing the proposer writes in the laws directory
  counts until a person approves it (ADR 0006).
- No language model takes part in a decision (ADR 0003).
- One implementation per concern: one process executor (`emi-exec`), one
  canonical JSON encoder and one hasher (`emi-kernel`), one parser for
  Python (tree-sitter-python in `emi-python`), one scratch-dir resolver.
  Import it; do not copy it.
- Never hardcode absolute scratch paths; never keep evidence or sealed
  artifacts under `/tmp`.
- Receipts and records state what was observed, never a default: a process
  executor kills the whole process group on every exit path and reports
  cleanup from a post-kill probe. A run kills only the processes it started.
- Hashed identities reject values without a stable encoding (NaN, sets,
  arbitrary objects). Nothing that varies between runs (a time, a scratch
  path, a duration) goes into a hashed part of a receipt.
- Functions stay under cognitive complexity 15 and 80 lines (clippy config
  enforces warnings; CI denies them). Validators with many guards use a table
  of (predicate, message), not an if-chain.
- Language knowledge stays in its adapter: `emi-python` is the only crate
  that knows Python or pytest syntax.
- No new crate or subcommand without a decision record saying why.
- Comments explain why; no work-package, audit or ticket ids in code.
- Tests clean up what they create (`tempfile`) and never depend on build
  output.
- No research or experiment code in this repository; it belongs in the
  separate validation-lab repository.
