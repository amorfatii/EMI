---
status: superseded by 0005
---

# Structural burden is frozen and the Python parity obligation ends

The Rust engine was built to reproduce the Python tool (`emi-v0.4`) bit for
bit, and new measurement work had to match it. The lab then found that no
static score, EMI's included, ranks defects or agent success better than
lines of code (S1, S2a, S5a, S5r, G2), and that pattern-based blocking cost
8.5 false blocks per 100 pull requests (GV2). We decided to freeze the
burden features (`report`, `trend`, `hotspots`, `panel`, the graded profile,
the LSP) as a secondary "where to look" aid, and to stop treating the Python
tool as the reference. Product effort goes to verification.

## Considered options

- **Keep investing in burden.** Rejected: the evidence for it is null.
- **Delete burden.** Rejected: `explain` and the refactor gate feed the
  verification loop, and the features work.
- **Remove the parity check.** Rejected: the golden files still catch a
  measurement that changes by accident.

## Consequences

- The golden-corpus check stays in `scripts/check.sh` as a regression test
  of the existing measurements. It no longer means "matches the Python tool".
- The live comparison against the Python tool is dropped, and `emi-v0.4` is
  archived. The Python tool is not needed to build, test or release EMI.
- Burden is never the headline claim and never blocks a change by default.
