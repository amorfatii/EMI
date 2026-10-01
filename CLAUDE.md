# EMI — rules for agents working in this repository

EMI verifies code changes made by AI coding agents (see README.md; the
vocabulary is in CONTEXT.md and the decisions in `docs/adr/`). The structural
burden measurements are frozen: the golden corpus check keeps them from
changing by accident, and the Python tool at `~/Projects/emi-v0.4` is no
longer the reference (docs/adr/0002).

## Before you commit
- Run `scripts/check.sh` (fmt, clippy -D warnings, tests, release build,
  golden parity). Never pipe it through `tail`/`grep` when deciding whether
  it passed; the exit status is the verdict.
- A change that alters any measured value needs a profile bump (the
  `PROFILE` constant in the adapter crate) and a new entry in `docs/parity.md`.

## Rules (each comes from a defect found in the v0.5 audit)
- A gate or check never reports pass/satisfied for input it could not
  resolve: return an explicit unknown (`Evidence::Partial`, needs_review) and
  count what was unresolved. (audit F-02, F-17)
- Never turn a failure into a plausible value (`""`, `0`, `"<unreadable>"`,
  an empty list). Return an error or an explicit unknown. (F-17)
- One implementation per concern: one process executor, one canonical JSON
  encoder, one hasher, one scratch-dir resolver. Import it; do not copy it. (F-12)
- Never hardcode absolute scratch paths such as `/tmp/opencode`; never keep
  evidence or sealed artifacts under `/tmp`. (F-04)
- Receipts and records state what was observed, never a default: a process
  executor kills the whole process group on every exit path and reports
  cleanup from a post-kill probe. (F-01)
- Hashed identities reject values without a stable encoding (NaN, sets,
  arbitrary objects). (F-06)
- Functions stay under cognitive complexity 15 and 80 lines (clippy config
  enforces warnings; CI denies them). Validators with many guards use a table
  of (predicate, message), not an if-chain. (F-11)
- Adapters (`emi-lang-*`) depend only on `emi-ir`, never on each other or on
  `emi-scan`/`emi-cli`. (F-10)
- Comments explain why; no work-package, audit or ticket ids in code. (F-15)
- Tests clean up what they create (`tempfile`), never depend on build output,
  and take expected values from the oracle (`tools/oracle_snippet.py`) rather
  than from the code under test. (F-08, F-13)
- No research or experiment code in this repository; it belongs in the
  separate validation-lab repository. (F-09)
