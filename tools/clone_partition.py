#!/usr/bin/env python3
"""Check that Rust body fingerprints partition functions exactly like CPython.

The oracle groups clone candidates by `ast.dump` of the function body (and of
its normalized copy). The Rust engine hashes its own structural encoding. The
two are interchangeable only if they induce the same equivalence classes:
every CPython class maps to exactly one Rust class and vice versa.

    python3 tools/clone_partition.py DIR [--binary target/release/examples/fingerprints]
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import os
import subprocess
import sys
import warnings
from collections import defaultdict
from pathlib import Path

ORACLE = Path(os.environ.get("EMI_ORACLE", Path.home() / "Projects" / "emi-v0.4"))
sys.path.insert(0, str(ORACLE))


def cpython_fingerprints(root: Path) -> dict:
    import python_probe
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
        rel = str(path.relative_to(root))
        for fn in ast.walk(tree):
            if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            body = fn.body[1:] if ast.get_docstring(fn, clean=False) is not None else fn.body
            exact = ast.dump(ast.Module(body=body, type_ignores=[]), include_attributes=False)
            norm = python_probe._normalized_signature(body)
            key = (rel, fn.lineno)
            value = (hashlib.sha256(exact.encode()).hexdigest(), hashlib.sha256(norm.encode()).hexdigest())
            out[key] = None if key in out else value  # ambiguous keys drop out
    return {k: v for k, v in out.items() if v is not None}


def rust_fingerprints(binary: Path, root: Path) -> dict:
    done = subprocess.run([str(binary), str(root)], capture_output=True, text=True, check=True)
    out = {}
    for line in done.stdout.splitlines():
        rel, lineno, exact, norm = line.split("\t")
        key = (rel, int(lineno))
        out[key] = None if key in out else (exact, norm)
    return {k: v for k, v in out.items() if v is not None}


def check(label: str, py: dict, rs: dict, idx: int) -> list:
    """Classes must correspond one-to-one; report splits and merges."""
    fwd, back = defaultdict(set), defaultdict(set)
    for key in py.keys() & rs.keys():
        fwd[py[key][idx]].add(rs[key][idx])
        back[rs[key][idx]].add(py[key][idx])
    problems = []
    for p, rset in fwd.items():
        if len(rset) > 1:
            keys = sorted(k for k in py.keys() & rs.keys() if py[k][idx] == p)
            problems.append(f"{label}: one CPython class split into {len(rset)} Rust classes: {keys[:4]}")
    for r, pset in back.items():
        if len(pset) > 1:
            keys = sorted(k for k in py.keys() & rs.keys() if rs[k][idx] == r)
            problems.append(f"{label}: {len(pset)} CPython classes merged in Rust: {keys[:4]}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("root", type=Path)
    parser.add_argument("--binary", type=Path,
                        default=Path(__file__).resolve().parent.parent / "target/release/examples/fingerprints")
    args = parser.parse_args()
    py = cpython_fingerprints(args.root)
    rs = rust_fingerprints(args.binary, args.root)
    common = py.keys() & rs.keys()
    print(f"functions: cpython {len(py)}, rust {len(rs)}, compared {len(common)}")
    missing = sorted(py.keys() - rs.keys())
    if missing:
        print(f"  {len(missing)} functions not fingerprinted by Rust, e.g. {missing[:3]}")
    problems = check("exact", py, rs, 0) + check("normalized", py, rs, 1)
    for p in problems[:20]:
        print("  " + p)
    print("PASS" if not problems else f"FAIL ({len(problems)} class mismatches)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
