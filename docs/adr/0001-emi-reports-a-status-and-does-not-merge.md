---
status: accepted
---

# EMI reports a status and does not merge

The production readiness plan of 2026-10-01 proposed a merge broker: a
protected component holding merge credentials that completes accepted
candidates without a person. We decided against it. EMI publishes its
acceptance decision as a status, and the hosting platform's branch
protection, required checks and merge queue decide whether a change merges.

The platforms already hold merge credentials and already bind a check to the
exact commit and base being merged, so a broker would rebuild that and make
EMI responsible for credential separation. Keeping EMI to "decide and
report" also keeps one engine for both settings: cooperative verification
inside a coding harness and enforced verification in CI.

## Consequences

- EMI holds no merge or push credentials, in any setting.
- Unattended merging is a platform configuration (a required EMI status plus
  auto-merge), not an EMI feature.
- A stale decision is the platform's problem to prevent: EMI's status must
  name the exact candidate and baseline it was computed for.
