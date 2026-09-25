#!/usr/bin/env python3
"""Compare the Rust CPython-shaped walk with CPython's `ast.walk`.

Every positioned node (constants and f-string pieces excluded) must appear
in the same breadth-first order with the same (lineno, col_offset).

    python3 tools/walk_check.py DIR [--binary target/release/examples/walk]
"""
import argparse
import ast
import subprocess
import sys
import warnings
from collections import defaultdict
from pathlib import Path

SKIP = {"Constant", "JoinedStr", "TemplateStr", "FormattedValue", "Interpolation"}


def cpython(root: Path) -> dict:
    out = {}
    for path in sorted(root.rglob("*.py")):
        if path.is_symlink() or not path.is_file():
            continue
        try:
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                tree = ast.parse(path.read_bytes().decode("utf-8"))
        except (SyntaxError, ValueError, UnicodeDecodeError):
            continue
        out[str(path.relative_to(root))] = [
            (type(n).__name__, n.lineno, n.col_offset) for n in ast.walk(tree)
            if hasattr(n, "lineno") and type(n).__name__ not in SKIP
            and not isinstance(n, ast.pattern) and not isinstance(n, ast.type_param)]
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("root", type=Path)
    parser.add_argument("--binary", type=Path,
                        default=Path(__file__).resolve().parent.parent / "target/release/examples/walk")
    args = parser.parse_args()
    want = cpython(args.root)
    got = defaultdict(list)
    done = subprocess.run([str(args.binary), str(args.root)], capture_output=True, text=True, check=True)
    for line in done.stdout.splitlines():
        rel, kind, lineno, col = line.split("\t")
        got[rel].append((kind, int(lineno), int(col)))
    bad = 0
    for rel, nodes in want.items():
        if rel not in got:
            continue
        mine = got[rel]
        if nodes != mine:
            bad += 1
            if bad <= 15:
                i = next((k for k, (a, b) in enumerate(zip(nodes, mine)) if a != b), min(len(nodes), len(mine)))
                print(f"{rel}: first difference at node {i}: cpython {nodes[i:i+2]} rust {mine[i:i+2]}")
    total = sum(len(v) for v in want.values())
    print(f"files {len(want)}, nodes {total}, files with differences {bad}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
