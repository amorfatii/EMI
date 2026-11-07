#!/usr/bin/env bash
# The checks, in two tiers.
#
#   scripts/check.sh --fast   formatting, lints, kernel purity, and every
#                             test that runs no real pytest (under a minute
#                             once built): while working.
#   scripts/check.sh          everything CI runs: the fast tier, then every
#                             test and the release build. Before pushing.
#
# Most of the full run is the integration tests that run `emi done` on real
# projects with real pytest. With cargo-nextest installed they run side by
# side across test binaries (`cargo install cargo-nextest`); without it,
# `cargo test` runs one binary at a time. CI runs them with nextest,
# sharded across machines.
set -euo pipefail
cd "$(dirname "$0")/.."

fast=false
case "${1:-}" in
    --fast) fast=true ;;
    "") ;;
    *) echo "usage: scripts/check.sh [--fast]" >&2; exit 2 ;;
esac

cargo fmt --check
cargo clippy --all-targets -- -D warnings
scripts/kernel-is-pure.sh
scripts/emi-done-has-no-language.sh

if $fast; then
    # tests that need no real pytest: unit tests, and integration tests
    # whose runners are stand-ins (`emi-cli` has a `done` test too, which
    # does run pytest, so that one is named by its crate)
    cargo test --quiet --workspace --lib --bins \
        --test hook --test engine_evidence --test cleanup --test functions --test hunks
    cargo test --quiet -p emi-done --test done
    exit 0
fi

if cargo nextest --version >/dev/null 2>&1; then
    cargo nextest run --workspace --no-fail-fast
    cargo test --quiet --workspace --doc
else
    cargo test --quiet --workspace
fi
cargo build --release --quiet
