#!/usr/bin/env bash
# The kernel decides from evidence alone: it may depend on encoding and
# hashing crates, never on anything that runs processes or touches files.
set -euo pipefail
cd "$(dirname "$0")/.."
allowed="hex serde serde_json sha2"
deps=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys
m = json.load(sys.stdin)
k = next(p for p in m["packages"] if p["name"] == "emi-kernel")
print(" ".join(sorted(d["name"] for d in k["dependencies"] if d["kind"] is None)))')
if [ "$deps" != "$allowed" ]; then
  echo "emi-kernel may depend only on: $allowed" >&2
  echo "it depends on: $deps" >&2
  exit 1
fi
if grep -rnE 'std::(fs|process|net|env|time)\b|SystemTime|Instant|Command::new|File::' crates/emi-kernel/src; then
  echo "emi-kernel must not do I/O" >&2
  exit 1
fi
