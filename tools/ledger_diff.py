#!/usr/bin/env python3
"""Differential fuzz of the calculator and gate against the oracle.

Generates random change documents (exact, unknown and partial evidence,
regions, unchanged components, incomplete scope, unverified identity,
history exposure) plus corrupted variants, then requires `emi review`
to print byte-identical JSON to the oracle's `review()` + `guardrail()`
under several gate policies, or to refuse exactly the inputs the oracle
refuses, with the same message.

    python3 tools/ledger_diff.py [--cases N] [--seed S] [--binary PATH]
    python3 tools/ledger_diff.py --record corpus/golden/ledger.jsonl

`--record` writes the oracle's answers (sha256 of the printed JSON, or the
refusal message) for a fixed case set, which `cargo test` replays without
Python (crates/emi-ledger/tests/golden.rs).
"""
import argparse
import copy
import hashlib
import json
import os
import random
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORACLE = Path(os.environ.get("EMI_ORACLE", Path.home() / "Projects" / "emi-v0.4"))
sys.path.insert(0, str(ORACLE))
import emi  # noqa: E402
import emi_contracts  # noqa: E402


def mass(rng, sloc, integer=False):
    """Exact, unknown, or partial evidence for a component of `sloc` lines."""
    roll = rng.random()
    if roll < 0.15:
        return None, 0.0, sloc
    if roll < 0.35 and sloc > 1:
        unknown = rng.randint(1, sloc)
        known = rng.randint(0, sloc - unknown) if integer else round(rng.uniform(0, sloc - unknown), rng.choice([0, 2, 6]))
        return {"known_mass": known, "unknown_sloc": unknown}, known, unknown
    if integer:
        value = rng.randint(0, sloc)
    else:
        value = rng.choice([0, 0.0, round(rng.uniform(0, sloc), rng.choice([0, 1, 3, 9])), rng.uniform(0, sloc)])
    return value, value, 0


def with_regions(rng, sloc):
    """E/C evidence for a component built from regions plus a residual."""
    regions, used = [], 0
    for i in range(rng.randint(1, 4)):
        rsloc = rng.randint(1, max(1, (sloc - used) // 2 or 1))
        if used + rsloc > sloc:
            break
        used += rsloc
        reg = {"id": f"r{i}", "sloc": rsloc, "sha": rng.choice(["a", "b", "c"]), "in_cycle": rng.random() < 0.2}
        regions.append(reg)
    known = {"E": [], "C": []}
    unknown = {"E": 0, "C": 0}
    for reg in regions:
        for dim in ("E", "C"):
            value, k, u = mass(rng, reg["sloc"])
            reg[dim] = value
            known[dim].append(k)
            unknown[dim] += u
    rest = sloc - used
    evidence = {}
    for dim in ("E", "C"):
        ru = rng.randint(0, rest) if rest else 0
        rk = float(rng.randint(0, rest - ru)) if rest - ru else 0.0
        total_known = sum(known[dim]) + rk
        total_unknown = unknown[dim] + ru
        evidence[dim] = ({"known_mass": total_known, "unknown_sloc": total_unknown}
                         if total_unknown else total_known)
    return regions, evidence


def component(rng, ident):
    sloc = rng.randint(1, rng.choice([5, 60, 400]))
    item = {"id": ident, "sloc": sloc, "burden": {}}
    dims = ["E", "V", "C"]
    if rng.random() < 0.2:
        rng.shuffle(dims)
    region_evidence = None
    if rng.random() < 0.35:
        item["regions"], region_evidence = with_regions(rng, sloc)
    for dim in dims:
        if region_evidence and dim in region_evidence:
            item["burden"][dim] = region_evidence[dim]
        else:
            item["burden"][dim] = mass(rng, sloc, integer=(dim == "V"))[0]
    if rng.random() < 0.3:
        item["churn_lines"] = rng.randint(0, 500)
    if rng.random() < 0.2:
        item["cohesion_fragmentation"] = rng.random()
    return item


def mutate_regions(rng, item):
    """Keep some regions byte-identical so the region delta path runs."""
    if "regions" not in item:
        return item
    other = copy.deepcopy(item)
    if rng.random() < 0.5:
        other["burden"]["V"] = rng.randint(0, other["sloc"])
    return other


def change(rng):
    ids = [f"c{i}" for i in range(rng.randint(1, 9))]
    before, after, unchanged = [], [], []
    for ident in ids:
        roll = rng.random()
        if roll < 0.15:
            before.append(component(rng, ident))
        elif roll < 0.3:
            after.append(component(rng, ident))
        elif roll < 0.5:
            item = component(rng, ident)
            before.append(item)
            after.append(copy.deepcopy(item))
            if rng.random() < 0.7:
                unchanged.append(ident)
        elif roll < 0.6:
            item = component(rng, ident)
            before.append(item)
            after.append(mutate_regions(rng, item))
        else:
            before.append(component(rng, ident))
            after.append(component(rng, ident))
    if not before and not after:
        before.append(component(rng, "c0"))
    complete = rng.random() < 0.85
    data = {
        "profile": "emi-0.3",
        "measurement_profile_before": "py-probe-x",
        "measurement_profile_after": "py-probe-x",
        "identity_verified": rng.random() < 0.9,
        "before": before,
        "after": after,
        "scope": {"complete": complete,
                  "expected_components": sorted({c["id"] for c in before + after}),
                  "unchanged_components": unchanged},
    }
    if rng.random() < 0.5:
        opts = {"probe": "python", "exclude": ["x"]}
        data["measurement_options_before"] = opts
        data["measurement_options_after"] = copy.deepcopy(opts)
    if rng.random() < 0.3:
        data["history_days"] = rng.choice([90, 30, 0])
    if not complete and rng.random() < 0.7:
        data["collection_errors"] = {"before": [], "after": [{"file": "x.py", "error": "boom"}]}
    if rng.random() < 0.3:
        data["measurement_contract_id"] = "abc"
        data["region_diagnostics"] = {"before": [], "after": []}
    return data


CORRUPTIONS = [
    lambda d, r: d.update(profile="emi-0.2"),
    lambda d, r: d.update(measurement_profile_after="other"),
    lambda d, r: d.update(identity_verified="yes"),
    lambda d, r: d["scope"].update(complete=None),
    lambda d, r: d["scope"]["expected_components"].append("ghost"),
    lambda d, r: d["scope"].update(unchanged_components=["ghost"]),
    lambda d, r: d.update(history_days=91),
    lambda d, r: d.update(history_days=12.5),
    lambda d, r: d.update(measurement_options_after={"probe": "js"}),
    lambda d, r: d.update(measurement_options_before=None),
    lambda d, r: d.update(collection_errors={"before": [{"nofile": 1}], "after": []}),
    lambda d, r: (d["scope"].update(complete=True),
                  d.update(collection_errors={"before": [{"file": "a"}], "after": []})),
]

ITEM_CORRUPTIONS = [
    lambda c, r: c.update(sloc=0),
    lambda c, r: c.update(sloc=3.0),
    lambda c, r: c.update(sloc=True),
    lambda c, r: c.update(id=" "),
    lambda c, r: c["burden"].pop("E"),
    lambda c, r: c["burden"].update(V=0.5),
    lambda c, r: c["burden"].update(E=-1),
    lambda c, r: c["burden"].update(E=c["sloc"] + 1e-10),
    lambda c, r: c["burden"].update(C=c["sloc"] + 1),
    lambda c, r: c["burden"].update(C={"known_mass": 1}),
    lambda c, r: c["burden"].update(C={"known_mass": 0, "unknown_sloc": 1.0}),
    lambda c, r: c["burden"].update(E=True),
    lambda c, r: c["burden"].update(E="3"),
    lambda c, r: c.update(churn_lines=-1),
    lambda c, r: c.update(churn_lines=2.5),
    lambda c, r: c.update(cohesion_fragmentation=1.5),
    lambda c, r: c.update(regions="x"),
    lambda c, r: c.update(regions=[{"id": "r", "sloc": c["sloc"] + 1, "sha": "a", "in_cycle": False, "E": 0, "C": 0}]),
    lambda c, r: c.update(regions=[{"id": "r", "sloc": 1, "sha": "", "in_cycle": False, "E": 0, "C": 0}]),
    lambda c, r: c.update(regions=[{"id": "r", "sloc": 1, "sha": "a", "in_cycle": 0, "E": 0, "C": 0}]),
    lambda c, r: c.update(regions=[{"id": "r", "sloc": 1, "sha": "a", "in_cycle": False, "E": 0}]),
    lambda c, r: c.update(regions=[{"id": "r", "sloc": c["sloc"], "sha": "a", "in_cycle": False, "E": 0, "C": 0}]),
]


def corrupt(rng, data):
    data = copy.deepcopy(data)
    items = data["before"] + data["after"]
    if items and rng.random() < 0.6:
        rng.choice(ITEM_CORRUPTIONS)(rng.choice(items), rng)
    else:
        rng.choice(CORRUPTIONS)(data, rng)
    return data


POLICIES = [
    [],
    ["--guardrail"],
    ["--guardrail", "--allow-growth", "0"],
    ["--guardrail", "--allow-growth", "2.5", "--allow-headroom", "50", "--max-evidence", "2"],
    ["--guardrail", "--compact", "--allow-growth", "0", "--max-evidence", "0"],
]


def oracle(data, argv):
    """What the oracle CLI prints for `emi.py INPUT argv`, and its exit code."""
    try:
        report = emi.review(copy.deepcopy(data))
    except (ValueError, KeyError, TypeError, OverflowError) as exc:
        return None, str(exc), 2
    allow = 10.0
    headroom = None
    max_evidence = 5
    if "--allow-growth" in argv:
        allow = float(argv[argv.index("--allow-growth") + 1])
    if "--allow-headroom" in argv:
        headroom = float(argv[argv.index("--allow-headroom") + 1])
    if "--max-evidence" in argv:
        max_evidence = int(argv[argv.index("--max-evidence") + 1])
    code = 0
    if "--guardrail" in argv:
        gate = emi.guardrail(report, allow, headroom, max_evidence=max_evidence)
        report["guardrail"] = gate
        if gate["decision"] == "block":
            code = 2
        if not report.get("scope", {}).get("complete", True):
            code = 2
        if "--compact" in argv:
            eff = headroom if headroom is not None else allow
            return json.dumps(emi.compact_verdict(report, gate, allow, eff), indent=2, allow_nan=False), None, code
    return json.dumps(report, indent=2, allow_nan=False), None, code


def rust(binary, path, argv):
    done = subprocess.run([str(binary), "review", str(path), *argv], capture_output=True, text=True, check=False)
    return done.stdout.rstrip("\n"), done.stderr, done.returncode


def check(binary, data, path, argv):
    want, error, want_code = oracle(data, argv)
    got, stderr, code = rust(binary, path, argv)
    if error is not None:
        if code != 2:
            return f"oracle refused ({error}) but emi exited {code}"
        if f": {error}; pass a build_change" not in stderr:
            return f"refusal differs:\n  oracle: {error}\n  emi:    {stderr.strip()}"
        return None
    if code != want_code:
        return f"exit {code} != oracle {want_code}: {stderr.strip()}"
    if got != want:
        for i, (a, b) in enumerate(zip(want.splitlines(), got.splitlines())):
            if a != b:
                return f"output differs at line {i + 1}:\n  oracle: {a}\n  emi:    {b}"
        return f"output length differs ({len(want)} vs {len(got)})"
    return None


def record(path: Path, cases: int, seed: int) -> int:
    rng = random.Random(seed)
    with path.open("w", encoding="utf-8") as out:
        for _ in range(cases):
            data = change(rng)
            bad = rng.random() < 0.3
            if bad:
                data = corrupt(rng, data)
            text = json.dumps(data, separators=(",", ":"))
            runs = []
            for argv in (POLICIES if not bad else POLICIES[:1]):
                printed, error, code = oracle(json.loads(text), argv)
                run = {"argv": argv, "code": code}
                if error is not None:
                    run["error"] = error
                else:
                    run["sha256"] = hashlib.sha256(printed.encode()).hexdigest()
                runs.append(run)
            out.write(json.dumps({"input": text, "runs": runs}, separators=(",", ":")) + "\n")
    print(f"recorded {cases} cases to {path}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cases", type=int, default=400)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--binary", type=Path, default=HERE.parent / "target" / "release" / "emi")
    parser.add_argument("--record", type=Path, default=None)
    args = parser.parse_args()
    if args.record:
        return record(args.record, args.cases, args.seed)
    rng = random.Random(args.seed)
    failures, refusals, runs = 0, 0, 0
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "change.json"
        for case in range(args.cases):
            data = change(rng)
            bad = rng.random() < 0.3
            if bad:
                data = corrupt(rng, data)
            path.write_text(json.dumps(data), encoding="utf-8")
            for argv in (POLICIES if not bad else POLICIES[:1]):
                runs += 1
                problem = check(args.binary, data, path, argv)
                if problem:
                    failures += 1
                    print(f"FAIL case {case} {' '.join(argv)}: {problem}")
                    if failures > 10:
                        return 1
            try:
                emi_contracts.validate_change(data)
                emi.review(copy.deepcopy(data))
            except Exception:  # noqa: BLE001
                refusals += 1
    print(f"{runs} runs over {args.cases} cases ({refusals} refused by the oracle): {failures} failures")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
