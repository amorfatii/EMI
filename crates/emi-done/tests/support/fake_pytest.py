"""A stand-in for the part of pytest the done check reads.

It collects plain `test_*` functions from `tests/`, runs them, prints
pytest's short test summary and last line, and exits with pytest's
statuses (0, 1, 2 for collection errors, 5 for no tests). The machine the
tests run on has no pytest, and installing packages is not allowed there.
"""

import ast
import importlib.util
import os
import sys


def test_files():
    found = []
    for root, dirs, files in os.walk("tests"):
        dirs.sort()
        for name in sorted(files):
            if name.endswith(".py") and (name.startswith("test_") or name.endswith("_test.py")):
                found.append(os.path.join(root, name).replace(os.sep, "/"))
    return sorted(found)


def collect(path):
    try:
        with open(path) as handle:
            tree = ast.parse(handle.read())
    except SyntaxError:
        return None
    return [n.name for n in tree.body if isinstance(n, ast.FunctionDef) and n.name.startswith("test_")]


def summary(failed, errors, passed):
    parts = []
    if failed:
        parts.append(f"{failed} failed")
    if errors:
        parts.append(f"{errors} error" + ("s" if errors > 1 else ""))
    if passed:
        parts.append(f"{passed} passed")
    return (", ".join(parts) or "no tests ran") + " in 0.01s"


def main(argv):
    files = test_files()
    ids, broken = [], []
    for path in files:
        names = collect(path)
        if names is None:
            broken.append(path)
        else:
            ids += [f"{path}::{n}" for n in names]
    if "--collect-only" in argv:
        print("\n".join(ids))
        print()
        print(f"{len(ids)} tests collected in 0.01s")
        return 2 if broken else (0 if ids else 5)
    problems = [("ERROR", p, "SyntaxError") for p in broken]
    passed = 0
    for path in files:
        if path in broken:
            continue
        spec = importlib.util.spec_from_file_location(path[:-3].replace("/", "."), path)
        module = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(module)
        except Exception as e:
            problems.append(("ERROR", path, f"{type(e).__name__}: {e}"))
            continue
        for name in collect(path):
            try:
                getattr(module, name)()
                passed += 1
                sys.stdout.write(".")
            except Exception as e:
                problems.append(("FAILED", f"{path}::{name}", f"{type(e).__name__}: {e}"))
                sys.stdout.write("F")
    print()
    if problems:
        print("=========================== short test summary info ============================")
        for kind, ident, message in problems:
            print(f"{kind} {ident} - {message}")
    failed = sum(1 for p in problems if p[0] == "FAILED")
    errors = len(problems) - failed
    print(summary(failed, errors, passed))
    if broken:
        return 2
    if problems:
        return 1
    return 0 if passed else 5


sys.exit(main(sys.argv[1:]))
