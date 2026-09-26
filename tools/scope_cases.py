#!/usr/bin/env python3
"""Differential check of scope edge cases against the oracle.

Small synthetic trees (empty, test-only, zero-eligible, all-failed, mixed
failures, .gitignore selection, missing root, JS config-only failures)
must yield the same refusal message, or the same measurement including
status, completeness, inventory and errors.

    python3 tools/scope_cases.py [--binary PATH]
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORACLE = Path(os.environ.get("EMI_ORACLE", Path.home() / "Projects" / "emi-v0.4"))
sys.path.insert(0, str(ORACLE))
sys.path.insert(0, str(HERE))
import parity  # noqa: E402

PY_CASES = {
    "empty": {},
    "only-text": {"README.md": "hi\n"},
    "test-only": {"tests/test_a.py": "def test_x():\n    assert 1\n", "conftest.py": "x = 1\n"},
    "zero-eligible": {"a.py": "# comment only\n", "b.py": '"""Docstring only."""\n'},
    "all-failed": {"a.py": "def (:\n", "b.py": b"\xff\xfe"},
    "mixed": {"a.py": "def f():\n    return 1\n", "b.py": "def (:\n", "c.py": b"x = '\xff'\n"},
    "gitignore": {".gitignore": "# c\n/gen/\n*_pb2.py\nbuild\n", "a.py": "x = 1\n",
                  "gen/b.py": "y = 2\n", "m_pb2.py": "z = 3\n", "pkg/build/c.py": "w = 4\n"},
    "tests-and-product": {"tests/test_a.py": "def test_x():\n    assert True\n", "a.py": "x = 1\n"},
}

JS_CASES = {
    "empty": {},
    "bad-config-only": {"tsconfig.json": "{not json", "a.ts": "export const x = 1;\n"},
    "config-and-unreadable": {"tsconfig.json": '{"extends": "./missing"}', "a.js": b"\xff\xfe"},
    "zero-sloc": {"a.js": "// only a comment\n"},
}


def write(root: Path, files: dict) -> None:
    for name, content in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            path.write_bytes(content)
        else:
            path.write_text(content, encoding="utf-8")


def oracle(probe: str, root: Path, flags: dict):
    try:
        if probe == "python":
            import python_probe
            return python_probe.collect(str(root), **flags), None
        import js_probe
        return js_probe.collect(str(root), **flags), None
    except (ValueError, SyntaxError, UnicodeDecodeError) as exc:
        return None, str(exc)


def rust(binary: Path, probe: str, root: Path, flags: dict):
    cmd = [str(binary), "scan", str(root), "--probe", probe]
    if flags.get("on_syntax_error") == "record":
        cmd.append("--tolerate-syntax-errors")
    if flags.get("use_gitignore"):
        cmd.append("--use-gitignore")
    done = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        return None, done.stderr.strip().removeprefix("emi: error: ")
    return json.loads(done.stdout), None


def check(binary, probe, name, files, flags, missing_root=False) -> list:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "scope"
        if not missing_root:
            root.mkdir()
            write(root, files)
        want, want_err = oracle(probe, root, flags)
        got, got_err = rust(binary, probe, root, flags)
    if want_err is not None or got_err is not None:
        # Raise-mode parse/decode failures carry the parser's own wording
        # (CPython vs ruff); both sides must refuse, not word it alike.
        worded = want_err is not None and got_err is not None and not want_err.startswith("no ")
        if want_err != got_err and not worded:
            return [f"refusal differs: oracle {want_err!r} vs emi {got_err!r}"]
        return []
    report = parity.compare({"measurement": want}, got, probe)
    if probe == "python" and want.get("inventory") != got.get("inventory"):
        report.append(f"inventory differs: {want.get('inventory')} vs {got.get('inventory')}")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", type=Path, default=HERE.parent / "target" / "release" / "emi")
    args = parser.parse_args()
    runs = []
    for name, files in PY_CASES.items():
        for flags in ({}, {"on_syntax_error": "record"}, {"on_syntax_error": "record", "use_gitignore": True}):
            runs.append(("python", name, files, flags, False))
    runs.append(("python", "missing-root", {}, {}, True))
    for name, files in JS_CASES.items():
        for flags in ({}, {"on_syntax_error": "record"}):
            runs.append(("js", name, files, flags, False))
    failures = 0
    for probe, name, files, flags, missing in runs:
        report = check(args.binary, probe, name, files, flags, missing)
        label = f"{probe}:{name} {' '.join(f'{k}={v}' for k, v in flags.items())}"
        print(f"{'PASS' if not report else 'FAIL'} {label}")
        for line in report:
            print(f"     {line}")
        failures += bool(report)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
