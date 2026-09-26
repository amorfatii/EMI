#!/usr/bin/env python3
"""Differential parity: Rust `emi scan` against the v0.5.1 golden corpus.

For every repo in corpus/repos.json, fetch the pinned commit, materialize its
raw git blobs (no attribute filters, as the golden build did), run the Rust
binary on the product scope, and compare the fields implemented so far.
Values must be equal; order differences are reported separately.

    python tools/parity.py [--only NAME] [--binary target/release/emi]
    python tools/parity.py --tree DIR --probe python|js   # live oracle run on any tree

Golden mode needs only git and network access for the first fetch (cached in
EMI_CORPUS_CACHE, default ~/.cache/emi-dev/corpus). Live mode also needs the
Python oracle checkout (EMI_ORACLE, default ~/Projects/emi-v0.4).
"""
from __future__ import annotations

import argparse
import gzip
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
ORACLE = Path(os.environ.get("EMI_ORACLE", Path.home() / "Projects" / "emi-v0.4"))
CACHE = Path(os.environ.get("EMI_CORPUS_CACHE", Path.home() / ".cache" / "emi-dev" / "corpus"))

# Output groups implemented per adapter; extend as phases land. A group not
# listed is not compared, so it cannot pass vacuously or fail prematurely.
IMPLEMENTED = {
    "python": {"redundancy", "coupling"},
    "js": {"redundancy", "coupling"},
}

# Fields compared per record kind, by phase. Extend as phases land.
FUNCTION_FIELDS = ("file", "function", "line", "sloc", "cc", "erosion_mass")
COUPLING_FUNCTION_FIELDS = ("resolved_fanout", "resolved_dependencies", "local_callees",
                            "coupling_unknown", "unresolved_calls", "in_static_cycle", "local_callers")
COUPLING_REGION_FIELDS = ("in_cycle", "C")
JS_FUNCTION_FIELDS = ("lines", "erosion_exact")
REGION_FIELDS = ("file", "id", "sloc", "sha", "E")


def canonical(value):
    """JSON number types carry no meaning here: 12 and 12.0 compare equal."""
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, dict):
        return {k: canonical(v) for k, v in value.items()}
    if isinstance(value, list):
        return [canonical(v) for v in value]
    return value


def dump(value) -> str:
    return json.dumps(canonical(value), sort_keys=True)


def pick(record: dict, fields) -> tuple:
    return tuple(dump(record.get(f)) for f in fields)


def git(repo: Path, *args: str, stdin: bytes | None = None) -> bytes:
    return subprocess.run(["git", "-C", str(repo), *args], input=stdin,
                          capture_output=True, check=True).stdout


def fetch(entry: dict, depth: int = 1) -> Path:
    """Fetch the pinned commit (plus `depth` history) into the cache."""
    repo = CACHE / entry["name"]
    if not (repo / ".git").is_dir():
        repo.mkdir(parents=True, exist_ok=True)
        git(repo, "init", "-q")
        git(repo, "remote", "add", "origin", f"https://github.com/{entry['repository']}.git")
    probe = subprocess.run(["git", "-C", str(repo), "cat-file", "-e", entry["commit"] + "^{commit}"],
                           capture_output=True)
    shallow = (repo / ".git" / "shallow").is_file()
    if probe.returncode != 0 or (depth > 1 and shallow and int(
            git(repo, "rev-list", "--count", entry["commit"]).decode()) < depth):
        git(repo, "fetch", "-q", f"--depth={depth}", "origin", entry["commit"])
    return repo


def check_backtest(binary: Path, spec: dict, entry: dict) -> list:
    """Replay a golden backtest: the whole verdict curve must match."""
    golden = json.loads(gzip.decompress(
        (REPO / "corpus" / "golden" / f"backtest-{spec['name']}.json.gz").read_bytes()))
    repo = fetch(entry, depth=spec["history_depth"])
    cmd = [str(binary), "backtest", "--repo", str(repo), "--source", entry["scope"],
           "--probe", entry["probe"], "--head", entry["commit"],
           "--commits", str(spec["commits"] + 1), "--allow-growth", str(spec["allowance"]),
           "--format", "json"]
    for headroom in spec["headrooms"]:
        cmd += ["--headroom", str(headroom)]
    done = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        return [f"emi backtest failed: {done.stderr.strip()}"]
    got, want = json.loads(done.stdout), golden["result"]
    return [f"{key} differs" for key in want if want[key] != got.get(key)]


def materialize(repo: Path, commit: str, dest: Path) -> None:
    """Write the raw tracked blobs of `commit` (regular files and symlinks)."""
    entries = git(repo, "ls-tree", "-r", "-z", "--full-tree", commit).split(b"\0")
    blobs = []
    for raw in filter(None, entries):
        meta, path = raw.split(b"\t", 1)
        mode, kind, sha = meta.split()
        if kind != b"blob":
            raise ValueError(f"unsupported tree entry {kind!r} at {path!r}")
        blobs.append((mode, sha.decode(), path.decode("utf-8", "surrogateescape")))
    out = git(repo, "cat-file", "--batch", stdin="".join(s + "\n" for _, s, _ in blobs).encode())
    pos = 0
    for mode, sha, rel in blobs:
        header_end = out.index(b"\n", pos)
        size = int(out[pos:header_end].split()[2])
        data = out[header_end + 1:header_end + 1 + size]
        pos = header_end + 1 + size + 1
        target = dest / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        if mode == b"120000":
            os.symlink(data.decode("utf-8", "surrogateescape"), target)
        else:
            target.write_bytes(data)


def run_rust(binary: Path, root: Path, entry: dict) -> dict:
    cmd = [str(binary), "scan", str(root), "--probe", entry["probe"]]
    if entry.get("options", {}).get("on_syntax_error") == "record":
        cmd.append("--tolerate-syntax-errors")
    for glob in entry.get("exclude", []):
        cmd += ["--exclude", glob]
    done = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        raise RuntimeError(f"emi scan failed: {done.stderr.strip()}")
    return json.loads(done.stdout)


def compare_lists(label: str, want: list, got: list, key, report: list) -> None:
    want_keys, got_keys = [key(r) for r in want], [key(r) for r in got]
    if want_keys == got_keys:
        return
    if sorted(want_keys) == sorted(got_keys):
        report.append(f"{label}: same records, different order")
        return
    missing = [k for k in want_keys if k not in set(got_keys)]
    extra = [k for k in got_keys if k not in set(want_keys)]
    report.append(f"{label}: {len(want)} expected, {len(got)} got, "
                  f"{len(missing)} missing, {len(extra)} unexpected")
    for k in missing[:5]:
        report.append(f"  - expected {k}")
    for k in extra[:5]:
        report.append(f"  + got      {k}")


# Parser messages are CPython's own wording (SyntaxError text, codec
# errors); the engine reports the same file, stage, kind and location.
ENGINE_WORDED = {"SyntaxError", "IndentationError", "TabError", "TokenError"}
# Identity fields that name the implementation rather than the contract.
IMPLEMENTATION_KEYS = {"adapter", "python", "engine", "parsers"}


def error_key(e: dict) -> str:
    kept = {k: v for k, v in e.items() if not (k == "message" and e.get("error") in ENGINE_WORDED)}
    return dump(kept)


def portable(options: dict) -> dict:
    return {k: v for k, v in options.items() if k not in IMPLEMENTATION_KEYS}


def compare(golden: dict, got: dict, probe: str) -> list:
    want = golden["measurement"]
    report: list = []
    redundancy = "redundancy" in IMPLEMENTED[probe]
    coupling = "coupling" in IMPLEMENTED[probe]
    compare_lists("components", want["components"], got["components"],
                  lambda c: (c["id"], c["sloc"], dump(c["burden"]["E"]),
                             c["burden"].get("V") if redundancy else None,
                             dump(c["burden"].get("C")) if coupling else None), report)
    if coupling and dump(want["static_dependency_cycles"]) != dump(got.get("static_dependency_cycles")):
        report.append(f"static_dependency_cycles differ: {want['static_dependency_cycles']} vs {got.get('static_dependency_cycles')}")
    if redundancy:
        compare_lists("verbosity_findings", want["verbosity_findings"], got["verbosity_findings"],
                      lambda f: dump(f), report)
    fields = FUNCTION_FIELDS + (JS_FUNCTION_FIELDS if probe == "js" else ()) + (COUPLING_FUNCTION_FIELDS if coupling else ())
    compare_lists("functions", want["functions"], got["functions"],
                  lambda f: pick(f, fields), report)
    compare_lists("regions", want["regions"], got["regions"],
                  lambda r: pick(r, REGION_FIELDS + (COUPLING_REGION_FIELDS if coupling else ())), report)
    if redundancy and probe == "python":
        compare_lists("test_value_findings", want.get("test_value_findings", []),
                      got.get("test_value_findings") or [], lambda f: dump(f), report)
    for key in ("clone_groups", "clone_groups_normalized") if redundancy else ():
        if json.dumps(want[key]) != json.dumps(got.get(key)):
            report.append(f"{key}: {len(want[key])} expected, {len(got.get(key) or [])} got"
                          + ("" if sorted(map(json.dumps, want[key])) != sorted(map(json.dumps, got.get(key) or []))
                             else " (same groups, different order)"))
    if want["source_sha256"] != got["source_sha256"]:
        report.append("source_sha256 differs")
    if probe == "js" and sorted(want.get("fallbacks", [])) != sorted(got.get("fallbacks", [])):
        report.append(f"fallbacks differ: {want.get('fallbacks')} vs {got.get('fallbacks')}")
    compare_lists("errors", sorted(want["errors"], key=dump), sorted(got["errors"], key=dump),
                  error_key, report)
    want_opts, got_opts = want.get("measurement_options"), got.get("measurement_options")
    if want_opts is not None and portable(want_opts) != portable(got_opts or {}):
        report.append(f"measurement_options differ: {portable(want_opts)} vs {portable(got_opts or {})}")
    for key in ("status", "scope_complete", "inventory_summary"):
        if key in want and want[key] != got.get(key):
            report.append(f"{key} differs: {want[key]!r} vs {got.get(key)!r}")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--only", action="append", default=[])
    parser.add_argument("--binary", type=Path, default=REPO / "target" / "release" / "emi")
    parser.add_argument("--tree", type=Path, help="compare a live oracle run on this directory")
    parser.add_argument("--probe", choices=("python", "js"), default="python")
    parser.add_argument("--exclude", action="append", default=[],
                        help="glob excluded on both sides (live mode)")
    args = parser.parse_args()
    if args.tree:
        return live(args)

    spec = json.loads((REPO / "corpus" / "repos.json").read_text(encoding="utf-8"))
    failures = 0
    for entry in spec["repos"]:
        if args.only and entry["name"] not in args.only:
            continue
        golden = json.loads(gzip.decompress(
            (REPO / "corpus" / "golden" / f"{entry['name']}.json.gz").read_bytes()))
        with tempfile.TemporaryDirectory(prefix="emi-parity-") as tmp:
            if "local" in entry:
                root = REPO / entry["local"] / entry["scope"]
            else:
                materialize(fetch(entry), entry["commit"], Path(tmp))
                root = Path(tmp) / entry["scope"]
            got = run_rust(args.binary, root, entry)
        report = compare(golden, got, entry["probe"])
        n = len(golden["measurement"]["functions"])
        status = "PASS" if not report else "FAIL"
        failures += bool(report)
        print(f"{status} {entry['name']:13} {n:6} functions")
        for line in report:
            print(f"     {line}")
    entries = {e["name"]: e for e in spec["repos"]}
    for bt in spec.get("backtests", []):
        if args.only and bt["name"] not in args.only:
            continue
        report = check_backtest(args.binary, bt, entries[bt["name"]])
        failures += bool(report)
        print(f"{'PASS' if not report else 'FAIL'} backtest-{bt['name']} {bt['commits']} pairs")
        for line in report:
            print(f"     {line}")
    return 1 if failures else 0


# Line breaks str.splitlines() honors beyond \n (docs/parity.md, D1).
EXOTIC_BREAKS = "\r\x0b\x0c\x1c\x1d\x1e\x85\u2028\u2029"
JS_SUFFIXES = (".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts")


def d1_files(tree: Path) -> list:
    """JS files the oracle cannot measure: exotic line breaks crash js_probe."""
    found = []
    for path in tree.rglob("*"):
        rel = path.relative_to(tree)
        if (path.suffix.lower() in JS_SUFFIXES and "node_modules" not in rel.parts[:-1]
                and path.is_file()):
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            if any(c in text for c in EXOTIC_BREAKS[1:]):
                found.append(rel.as_posix())
    return sorted(found)


def live(args) -> int:
    """Run the oracle and the Rust binary on the same tree (record mode)."""
    sys.path.insert(0, str(ORACLE))
    if args.probe == "python":
        import python_probe as probe
    else:
        import js_probe as probe
        skipped = d1_files(args.tree)
        if skipped:
            print(f"     excluding {len(skipped)} D1 file(s) the oracle cannot measure: {skipped[:3]}")
        args.exclude = list(args.exclude) + skipped
    want = probe.collect(str(args.tree), on_syntax_error="record", exclude_globs=args.exclude)
    entry = {"probe": args.probe, "options": {"on_syntax_error": "record"},
             "exclude": args.exclude}
    got = run_rust(args.binary, args.tree, entry)
    report = compare({"measurement": want}, got, args.probe)
    print(f"{'PASS' if not report else 'FAIL'} {args.tree} {len(want['functions'])} functions")
    for line in report:
        print(f"     {line}")
    return 1 if report else 0


if __name__ == "__main__":
    sys.exit(main())
