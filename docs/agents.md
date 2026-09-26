# EMI for coding agents (`emi mcp`)

`emi mcp` serves EMI over the Model Context Protocol (stdio, JSON-RPC 2.0),
so an agent can check its own change before it asks a human to review it.
EMI measures everything itself from git blobs or the working tree; the
agent passes locations and refs, never numbers (audit F-16).

## Setup

Claude Code:

```sh
claude mcp add emi -- /path/to/emi mcp
```

Other MCP clients take the same command in their server configuration:
`{"command": "/path/to/emi", "args": ["mcp"]}`.

## Tools

| Tool | What it answers |
|---|---|
| `emi_gate` | *Did my change grow burden?* Measures the working tree (or `after`) against `before` (default `HEAD`). It returns `block`/`warn`/`pass` with margins per dimension, the responsible components, and a `next_step` (`split`, `shrink`, `justify`, `inspect-unknowns`, `proceed`). |
| `emi_explain` | *Why is this file burdened?* Per region: SLOC, complexity, erosion, coupling dependencies, unresolved calls. It also returns clone groups and findings for the file. `line` narrows the answer to one region. |
| `emi_hotspots` | *Where does maintenance cost accrue?* Burden × change frequency, change coupling, authorship, AI-attributed share. |
| `emi_accept_rewrite` | *Is my refactor a real win?* Accepts only when matched density improves **and** measured burden is reduced, with no regressions. Dilution (clean code added, nothing removed) returns `needs-review`. |
| `emi_scan` | *What does the scope look like?* Totals per dimension, unknown-coupling share, cycles, the most burdened files. |

Common arguments: `repo` (default `.`), `source` (scope directory, default
`src`), `probe` (`python`/`js`) and `profile` (`precise` by default for
agents, `parity` for the v0.5.1 contract).

## The loop

1. Edit.
2. Call `emi_gate`.
   - `pass`: done.
   - `block`: the evidence names the component and dimension. `next_step: split` means one component carries most of the proven growth; `shrink` means it is spread out.
   - `warn` with `inspect-unknowns`: the growth is possible but unproven. Call `emi_explain` on the evidence files and resolve the dynamic calls if you can.
3. For a refactor, call `emi_accept_rewrite` before claiming it made the code better.

A block is always proven growth beyond the allowance. EMI never blocks on
uncertainty, and never on the headline score, which padding can game.
Structural measurement does not see behavior, so the gate belongs next to
tests and types, never instead of them.
