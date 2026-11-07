---
status: accepted
---

# Languages are drivers behind one boundary

EMI checks Python projects tested with pytest, and knowledge of Python had
spread beyond the adapter: `emi-done` parses pytest's command and output,
names Python's test-runner configuration files, builds an import-origin
check in Python, and imports evidence types (`LawEvidence`, the sandbox
`Runner`) from `emi-python`, though they say nothing about Python. Adding
a second language would have meant editing the orchestrator and the
receipt in every place that knows the first.

We decided that each language is a driver behind one boundary, in a new
crate, `emi-driver`:

- `emi-driver` holds the `Driver` trait and the language-neutral types its
  methods take and return: per-test outcomes, test ids, the evidence each
  stage produces. It depends on nothing language-specific.
- `emi-done` depends on `emi-driver` and on no language crate. It is given
  a driver and calls only the trait. `emi-cli` chooses the driver from the
  configuration's `language`.
- Each stage the orchestrator runs (regression, change proving, laws,
  preserve, adequacy, escape-hatch signals) is a method of the trait. A
  driver that cannot measure a stage says so, and the stage is reported as
  not measured with that reason: never as a pass. A driver without
  regression evidence cannot let a proposer stop.
- `emi-python` is the Python driver. A Rust driver, regression first
  (`cargo test`), is the second; it exists to show that the boundary is
  not shaped around Python, as a boundary drawn from one implementation
  tends to be.

A new crate was chosen over a module in `emi-done` so that a language
crate implements the trait without depending on the orchestrator.

## Consequences

- `scripts/emi-done-has-no-language.sh` checks that `emi-done` depends on
  no language crate.
- The decision kernel is unchanged: drivers produce evidence, and the
  facts and the action table stay where they are (ADR 0008).
- The pins' environment variables for Python (`PYTHONHASHSEED`,
  `PYTHONPATH`) still live in `emi-kernel`; they move to the Python driver
  when the pins are split by language.
- A stage a driver does not measure is visible in every receipt of that
  language, so what a language's checks are worth is stated, not implied.
