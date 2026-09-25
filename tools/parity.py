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

# Fields compared per record kind, by phase. Extend as phases land.
FUNCTION_FIELDS = ("file", "function", "line", "sloc", "cc", "erosion_mass")
JS_FUNCTION_FIELDS = ("lines", "erosion_exact")
REGION_FIELDS = ("file", "id", "sloc", "sha", "E")


def pick(record: dict, fields) -> tuple:
    return tuple(json.dumps(record.get(f), sort_keys=True) for f in fields)


def git(repo: Path, *args: str, stdin: bytes | None = None) -> bytes:
    return subprocess.run(["git", "-C", str(repo), *args], input=stdin,
                          capture_output=True, check=True).stdout


def fetch(entry: dict) -> Path:
    """Shallow-fetch the pinned commit into the cache (reused across runs)."""
    repo = CACHE / entry["name"]
    if not (repo / ".git").is_dir():
        repo.mkdir(parents=True, exist_ok=True)
        git(repo, "init", "-q")
        git(repo, "remote", "add", "origin", f"https://github.com/{entry['repository']}.git")
    probe = subprocess.run(["git", "-C", str(repo), "cat-file", "-e", entry["commit"] + "^{commit}"],
                           capture_output=True)
    if probe.returncode != 0:
        git(repo, "fetch", "-q", "--depth=1", "origin", entry["commit"])
    return repo


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


def compare(golden: dict, got: dict, probe: str) -> list:
    want = golden["measurement"]
    report: list = []
    compare_lists("components", want["components"], got["components"],
                  lambda c: (c["id"], c["sloc"], json.dumps(c["burden"]["E"])), report)
    fields = FUNCTION_FIELDS + (JS_FUNCTION_FIELDS if probe == "js" else ())
    compare_lists("functions", want["functions"], got["functions"],
                  lambda f: pick(f, fields), report)
    compare_lists("regions", want["regions"], got["regions"],
                  lambda r: pick(r, REGION_FIELDS), report)
    if want["source_sha256"] != got["source_sha256"]:
        report.append("source_sha256 differs")
    if probe == "js" and sorted(want.get("fallbacks", [])) != sorted(got.get("fallbacks", [])):
        report.append(f"fallbacks differ: {want.get('fallbacks')} vs {got.get('fallbacks')}")
    want_err = sorted((e["file"], e.get("error_lines") and tuple(e["error_lines"]))
                      for e in want["errors"] if e.get("error") == "SyntaxError")
    got_err = sorted((e["file"], e.get("error_lines") and tuple(e["error_lines"]))
                     for e in got["errors"] if e.get("error") == "SyntaxError")
    if want_err != got_err:
        report.append(f"syntax errors differ: {want_err} vs {got_err}")
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
