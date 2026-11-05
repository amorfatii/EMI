#!/usr/bin/env bash
# Everything CI runs, in order; stops at the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets -- -D warnings
scripts/kernel-is-pure.sh
cargo test --quiet
cargo build --release --quiet
