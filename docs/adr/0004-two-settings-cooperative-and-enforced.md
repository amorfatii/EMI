---
status: accepted
---

# Verification has two settings: cooperative and enforced

On a developer's machine the proposer runs as the developer. It can reach
the configuration, the approvals and the controller's records, so a local
check cannot be made tamper-proof without a separate user and sandbox for
the controller itself. We decided not to build that. Local verification is
**cooperative**: it catches a careless proposer, not a cheating one, and
says so on every receipt. **Enforced** verification runs in CI on a clean
runner, with the configuration read from the baseline.

The lab's evidence supports the split: real agents made no bypass attempts
(S12) and weakened tests no more than people do (S7), while careless false
completion claims are the everyday failure.

## Consequences

- Isolation before every execution, sealed inputs and durable budgets are
  CI-only work, and later.
- `enforced` names which inputs EMI reads. It is not a claim of adversarial
  security until that later work is done.
- A receipt from the cooperative setting must never be accepted where an
  enforced one is required; the setting is part of the receipt's assurance.
