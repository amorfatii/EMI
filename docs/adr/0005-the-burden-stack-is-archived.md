---
status: accepted
---

# The structural burden stack is archived

ADR 0002 froze the burden features as a secondary "where to look" aid.
They stayed in the workspace and kept costing: the done check's crates
depended on the scanner, all four language adapters, the CPython-exact
ledger and the panel, for five small utilities. That was 30,000 lines the
verification path could not be changed without. We decided to remove them
from the workspace. Tag `archive/burden-final` keeps them, buildable as
they were.

## Considered options

- **Keep them frozen in place (ADR 0002).** Rejected: frozen code that
  every crate depends on is not frozen in cost.
- **A separate repository.** Rejected for now: nobody maintains it, and a
  tag keeps the history at no cost.

## Consequences

- The workspace has eight crates: `emi-kernel`, `emi-git`, `emi-tree`,
  `emi-exec`, `emi-python`, `emi-done`, `emi-hook`, `emi-cli`.
- The CLI is `init`, `done`, `replay`, `intent`, `hook`, `journal`.
- Python with pytest is the only language. Another one comes back as an
  adapter beside `emi-python`, once the completion check is shown to help.
- The golden corpus and the parity check are gone with what they tested.
