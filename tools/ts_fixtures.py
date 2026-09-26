#!/usr/bin/env python3
"""Differential check of tsconfig resolution on the TypeScript reference cases.

Each case of the oracle's ts_reference_fixtures.json (outputs recorded from
typescript 5.6.3) is written to a temp tree plus one probe file importing
every `paths` key and a few bare packages. The oracle and the Rust binary
must then agree on the whole measurement, both for the tree itself and for
each subdirectory measured with the case root as config root.

    python3 tools/ts_fixtures.py [--binary target/release/emi]
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


def write_tree(root: Path, files: dict) -> None:
    keys = set()
    for name, content in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(content) if isinstance(content, dict) else content, encoding="utf-8")
        if isinstance(content, dict):
            keys.update((content.get("compilerOptions") or {}).get("paths") or {})
    specs = sorted(k.replace("*", "thing") for k in keys) + ["lodash", "@scope/pkg", "./missing"]
    lines = [f'import m{i} from "{s}";' for i, s in enumerate(specs)]
    lines.append("export function probeAll() { return [" + ", ".join(f"m{i}()" for i in range(len(specs))) + "]; }")
    (root / "zz_probe.ts").write_text("\n".join(lines) + "\n", encoding="utf-8")


def run(binary: Path, scope: Path, config_root: Path) -> list:
    import js_probe
    try:
        want = js_probe.collect(str(scope), on_syntax_error="record", config_root=str(config_root))
    except ValueError as exc:
        return [] if "no JavaScript" in str(exc) else [f"oracle failed: {exc}"]
    cmd = [str(binary), "scan", str(scope), "--probe", "js", "--tolerate-syntax-errors",
           "--config-root", str(config_root)]
    done = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        return [f"emi scan failed: {done.stderr.strip()}"]
    return parity.compare({"measurement": want}, json.loads(done.stdout), "js")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", type=Path, default=HERE.parent / "target" / "release" / "emi")
    args = parser.parse_args()
    cases = json.loads((ORACLE / "ts_reference_fixtures.json").read_text(encoding="utf-8"))["cases"]
    failures = 0
    for case in cases:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_tree(root, case["files"])
            scopes = [root] + sorted(p for p in root.iterdir() if p.is_dir() and p.name != "node_modules")
            # Also: the case root measured against a subdirectory config root,
            # i.e. a scope outside its config root (mappings disabled).
            runs = [(s, root) for s in scopes] + [(root, s) for s in scopes[1:2]]
            for scope, config in runs:
                report = run(args.binary, scope, config)
                label = f"{case['name']}:{scope.relative_to(root).as_posix()}@{config.relative_to(root).as_posix()}"
                print(f"{'PASS' if not report else 'FAIL'} {label}")
                for line in report:
                    print(f"     {line}")
                failures += bool(report)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
