#!/usr/bin/env python3
"""Differential check of `emi diff --guardrail` against the oracle.

For consecutive first-parent commit pairs of a repository, runs the
oracle's build_change + review + guardrail in-process and the Rust binary,
and requires the same report after removing the fields that name the
implementation (adapter profile, options, and the ids digested from them).

    python3 tools/diff_parity.py --repo DIR --source lib --probe js [--pairs 20]
"""
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORACLE = Path(os.environ.get("EMI_ORACLE", Path.home() / "Projects" / "emi-v0.4"))
sys.path.insert(0, str(ORACLE))
import emi  # noqa: E402

# Report fields that name the implementation rather than the measurement.
IDENTITY = {"measurement_profile", "measurement_options", "measurement_contract_id",
            "snapshot_id_before", "snapshot_id_after", "measurement_compatibility"}


def canonical(value):
    """Integral floats as ints, identity fields dropped, recursively."""
    if isinstance(value, dict):
        return {k: canonical(v) for k, v in value.items() if k not in IDENTITY}
    if isinstance(value, list):
        return [canonical(v) for v in value]
    if isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def first_difference(want, got, path="$"):
    if type(want) is not type(got):
        return f"{path}: {json.dumps(want)[:200]} vs {json.dumps(got)[:200]}"
    if isinstance(want, dict):
        for key in list(want) + [k for k in got if k not in want]:
            if key not in want or key not in got:
                return f"{path}.{key}: present on one side only"
            found = first_difference(want[key], got[key], f"{path}.{key}")
            if found:
                return found
        return None
    if isinstance(want, list):
        if len(want) != len(got):
            return f"{path}: {len(want)} vs {len(got)} items"
        for i, (a, b) in enumerate(zip(want, got)):
            found = first_difference(a, b, f"{path}[{i}]")
            if found:
                return found
        return None
    return None if want == got else f"{path}: {want!r} vs {got!r}"


def oracle(args, before, after):
    try:
        change = emi.build_change(args.repo, args.source, before, after, probe=args.probe,
                                  tolerate_syntax_errors=args.tolerate_syntax_errors)
        report = emi.review(change)
        report["guardrail"] = emi.guardrail(report, args.allow_growth)
        return report, None
    except (ValueError, RuntimeError, OSError) as exc:
        return None, str(exc)


def rust(args, before, after):
    cmd = [str(args.binary), "diff", "--repo", str(args.repo), "--source", args.source,
           "--before", before, "--after", after, "--probe", args.probe, "--guardrail",
           "--allow-growth", str(args.allow_growth)]
    if args.tolerate_syntax_errors:
        cmd.append("--tolerate-syntax-errors")
    done = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if done.returncode not in (0, 2) or not done.stdout.strip():
        return None, done.stderr.strip()
    return json.loads(done.stdout), None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--source", default="src")
    parser.add_argument("--probe", choices=("python", "js"), default="python")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--pairs", type=int, default=20)
    parser.add_argument("--allow-growth", type=float, default=10.0)
    parser.add_argument("--tolerate-syntax-errors", action="store_true")
    parser.add_argument("--binary", type=Path, default=HERE.parent / "target" / "release" / "emi")
    args = parser.parse_args()
    log = subprocess.run(["git", "-C", str(args.repo), "log", "--first-parent", "--format=%H",
                          "-n", str(args.pairs + 1), args.head],
                         capture_output=True, text=True, check=True).stdout.split()
    commits = list(reversed(log))
    failures = 0
    for old, new in zip(commits, commits[1:]):
        want, want_err = oracle(args, old, new)
        got, got_err = rust(args, old, new)
        label = f"{old[:12]}..{new[:12]}"
        if want_err or got_err:
            ok = bool(want_err) and bool(got_err)
            problem = None if ok else f"oracle: {want_err!r}; emi: {got_err!r}"
        else:
            problem = first_difference(canonical(want), canonical(got))
        verdict = (want or {}).get("guardrail", {}).get("decision", "refused")
        print(f"{'PASS' if not problem else 'FAIL'} {label} {verdict}")
        if problem:
            print(f"     {problem}")
            failures += 1
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
