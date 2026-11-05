---
status: accepted
---

# The decision is computed by a kernel with no I/O

The decision rules were spread over an untyped JSON contract, a generic
task engine's reducer and the done check's action table. We decided that
the decision is one pure, total function in `emi-kernel`, over counted
facts and a policy, and that the kernel depends only on encoding and
hashing crates. `scripts/kernel-is-pure.sh` checks its dependencies and
refuses process, file, network or clock access in its sources.

## Consequences

- The table is tested over every combination of findings under every
  policy: no finding can turn a decision into `stop`, and a hard finding
  (a fault, a break, a broken or unmet law, nondeterminism, a failing
  added test, a divergence, an unapproved change) is never `stop`.
- Any change to the table bumps `POLICY`, which every receipt names.
- Canonical JSON and the hasher live in the kernel, the one implementation
  of each.
