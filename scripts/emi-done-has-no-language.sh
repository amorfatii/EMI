#!/usr/bin/env bash
# The orchestrator checks every language the same way (ADR 0009): it is
# given a driver, and may depend on no language crate. Its tests may: a
# dev-dependency is out of reach of the library's own code.
set -euo pipefail
cd "$(dirname "$0")/.."
languages="emi-python emi-rust"
deps=$(cargo metadata --format-version 1 --no-deps |
  python3 -c 'import json, sys
m = json.load(sys.stdin)
k = next(p for p in m["packages"] if p["name"] == "emi-done")
print(" ".join(sorted(d["name"] for d in k["dependencies"] if d["kind"] is None)))')
for lang in $languages; do
  if [[ " $deps " == *" $lang "* ]]; then
    echo "emi-done must not depend on the language crate $lang (ADR 0009)" >&2
    exit 1
  fi
done
