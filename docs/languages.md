# Languages

| Language | Probe | Profile(s) | Oracle |
|---|---|---|---|
| Python | `python` | `parity` (v0.5.1 `python-probe-0.12`), `precise` | yes |
| JavaScript / TypeScript | `js` | `parity` (v0.5.1 `js-probe-0.10`), `precise` | yes |
| Rust | `rust` | `emi-rust-0.1` (native) | none |

## Rust (`emi-rust-0.1`)

Native semantics (no v0.5 oracle exists), chosen to match the other
adapters' contracts:

- **SLOC** counts rows covered by a non-comment token; doc comments are
  comments. `#[cfg(test)]` items and `#[test]` functions are test code:
  never product SLOC, like test files. So are `tests/`, `benches/` and
  `examples/`.
- **Crates and modules.** A file's crate is the nearest `Cargo.toml` with a
  `[package]` name, with `-` read as `_`. Its module follows the Cargo
  layout: `src/lib.rs` and `src/main.rs` are the crate root, `src/a/mod.rs`
  and `src/a.rs` are `crate::a`, and `src/a/b.rs` is `crate::a::b`.
- **Regions.** Every `fn` is a region (free, inherent, trait or nested), and
  so is every declaration: struct, enum, union, trait, impl, const, static,
  type, `macro_rules!` and inline module. A file of type definitions is
  therefore not read as one long eroded block. Closures belong to their
  enclosing function. Ids look like `S`, `impl S`, `S.m`,
  `<S as Display>.fmt` and `inner.deep`.
- **Complexity** is 1, plus each `if` (including `if let` and `else if`),
  `while`, `for`, `?`, `&&` and `||`, plus each `match` arm after the
  first. `loop` without a condition does not branch.
- **Redundancy** counts whole-function clones (exact, and with identifiers
  and literals blanked; 6+ lines and 50+ leaves) and repeated long lines,
  capped at file size.
- **Coupling.** A region depends on the first-party modules its code names:
  - `use` bindings, including renames, groups, `self` and globs (resolved
    through each module's item catalog);
  - `crate::`, `self::` and `super::` paths;
  - child modules declared with `mod x;`;
  - paths into other workspace crates;
  - paths spelled inside macro invocations.

  Resolution follows `pub use` re-exports to the defining module.
  Declaring a child module is ownership, not a dependency. Method calls
  through values are dispatch, as in the `precise` profile. Rust needs no
  unknowns: every name is resolved, local, external (std or a registry
  crate) or a prelude item. Mutually dependent modules form static cycles,
  and cycle members carry their full SLOC as C.

Rules for Rust in the catalog: `rust-dbg`, `rust-todo-macro` and
`todo-marker`.

## Dogfooding

EMI gates its own crates (`.github/workflows/emi.yml`):

```sh
emi backtest --repo . --source crates --probe rust --commits 12
emi diff --repo . --source crates --probe rust --before main --worktree --guardrail
```

The first self-backtest (2026-09-26) blocked 9 of 11 feature commits at
allowance 10. EMI then named `emi-lsp`'s 76-line, complexity-13
`Server::handle` as the source of that commit's growth. Splitting it per
message, as the gate's `next_step: split` suggested, took the file's
erosion from 15.7 to 1.1.
