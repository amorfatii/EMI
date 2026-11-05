---
status: accepted
---

# An outcome that does not reproduce is not evidence

The done check cleared a failure when a later run of the suite passed:
"flaky, not a break". That made the decision depend on how runs happened to
go, which contradicts deterministic acceptance. We decided that under the
same pins a test outcome that differs between two runs counts for neither
side.

## Consequences

- The baseline's tests run twice under the same pins. Tests that differ
  are quarantined, listed in the receipt, and are not regression evidence.
- A candidate failure that passes when the same run is repeated is
  `nondeterministic`, and the action is `incomplete`. One that fails again
  is a break, whatever else could not be reproduced.
- Every run is pinned (docs/determinism.md), so fewer outcomes differ to
  begin with, and `emi replay` checks that a decision is reproduced from
  nothing cached.
