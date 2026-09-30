"""Designed variants for `emi task holdout --vary`.

Reads one JSON request (argv[1]): the visible test file's source, the trusted
reference's path, a seed, the most cases to write and the timeouts. Derives
new inputs from the visible `assert f(args) == expected` cases, asks the
reference for each expected value, and prints one JSON result. Nothing is
invented: a case is kept only when the reference answered and the answer
survives a repr round trip. Deterministic for a given seed (PYTHONHASHSEED is
fixed by the caller, pools and candidates are sorted before any draw).
"""

import ast
import copy
import json
import math
import os
import random
import runpy
import signal
import sys
import time

MAX_REPR = 10_000
DROPS = ("reference_error", "timeout", "unstable_repr", "duplicate", "unparsed_visible")


class CaseTimeout(BaseException):
    """Not an Exception: the reference cannot swallow it by accident."""


def _on_alarm(signum, frame):
    raise CaseTimeout()


def dotted(node):
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        base = dotted(node.value)
        return base and f"{base}.{node.attr}"
    return None


class Finder(ast.NodeVisitor):
    def __init__(self):
        self.stack = []
        self.asserts = []
        self.calls = []

    def visit_FunctionDef(self, node):
        self.stack.append(node)
        self.generic_visit(node)
        self.stack.pop()

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_Assert(self, node):
        self.asserts.append((node, list(self.stack)))
        self.generic_visit(node)

    def visit_Call(self, node):
        self.calls.append(node)
        self.generic_visit(node)


def entry_point(name, stack, calls):
    """The function an assert really tests: a `candidate` parameter of the
    enclosing helper stands for what the helper's callers pass it."""
    for fn in reversed(stack):
        params = [a.arg for a in fn.args.args]
        if name not in params:
            continue
        at = params.index(name)
        passed = {
            dotted(c.args[at])
            for c in calls
            if dotted(c.func) == fn.name and len(c.args) > at
        }
        passed.discard(None)
        return passed.pop() if len(passed) == 1 else None
    return name


def visible_case(node, stack, calls):
    """(entry point, literal argument tuple) of `assert f(args) == x`."""
    test = node.test
    if not (
        isinstance(test, ast.Compare)
        and len(test.ops) == 1
        and isinstance(test.ops[0], ast.Eq)
        and isinstance(test.left, ast.Call)
    ):
        return None
    call = test.left
    if call.keywords or not call.args or any(isinstance(a, ast.Starred) for a in call.args):
        return None
    name = dotted(call.func)
    name = name and entry_point(name, stack, calls)
    if not name:
        return None
    try:
        return name, tuple(ast.literal_eval(a) for a in call.args)
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
        return None


def stable_repr(value):
    """repr(value) when it reads back as an equal Python literal."""
    text = repr(value)
    if len(text) > MAX_REPR:
        return None
    try:
        return text if ast.literal_eval(text) == value else None
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
        return None


def distinct(values, original):
    seen = {repr(original)}
    out = []
    for v in values:
        if repr(v) not in seen:
            seen.add(repr(v))
            out.append(v)
    return out


def sequence_mutations(items, make, rng, depth):
    out = []
    if items:
        out += [make(items[:-1]), make(items[: len(items) // 2])]
        out.append(make(items + [rng.choice(items)]))
        out.append(make(items[::-1]))
        shuffled = items[:]
        rng.shuffle(shuffled)
        out.append(make(shuffled))
    if len(items) >= 2:
        i, j = rng.sample(range(len(items)), 2)
        swapped = items[:]
        swapped[i], swapped[j] = swapped[j], swapped[i]
        out.append(make(swapped))
    if depth < 2 and items and not isinstance(items, (str, bytes)):
        i = rng.randrange(len(items))
        inner = mutations(items[i], rng, depth + 1)
        if inner:
            changed = items[:]
            changed[i] = rng.choice(inner)
            out.append(make(changed))
    return out


def mutations(v, rng, depth=0):
    """Nearby values of one literal: the small edits a special-cased
    implementation is least likely to have hard-coded."""
    if isinstance(v, bool):
        return [not v]
    if isinstance(v, int):
        out = [v + 1, v - 1, v + 2, v - 2, 0, -v, 2 * v]
    elif isinstance(v, float):
        out = [v + 0.1, v - 0.1, v * 1.01, v * 0.99, -v, 0.0] if math.isfinite(v) else []
    elif isinstance(v, str):
        out = sequence_mutations(list(v), "".join, rng, depth)
    elif isinstance(v, bytes):
        out = sequence_mutations(list(v), bytes, rng, depth)
    elif isinstance(v, (list, tuple)):
        out = sequence_mutations(list(v), type(v), rng, depth)
    elif isinstance(v, (set, frozenset)):
        items = sorted(v, key=repr)
        out = [type(v)(items[:i] + items[i + 1 :]) for i in range(min(len(items), 3))]
    elif isinstance(v, dict):
        keys = list(v)
        out = [{k: v[k] for k in keys if k != drop} for drop in keys[:3]]
    else:
        out = []
    return distinct(out, v)


def pools_of(cases):
    """The values seen at each argument position, per entry point and arity."""
    pools = {}
    for name, args in cases:
        pool = pools.setdefault((name, len(args)), [{} for _ in args])
        for slot, value in zip(pool, args):
            slot.setdefault(repr(value), value)
    return {
        key: [[slot[r] for r in sorted(slot)] for slot in pool]
        for key, pool in pools.items()
    }


def pick_value(own, pool, rng):
    options = mutations(own, rng)
    if pool and (rng.random() < 0.5 or not options):
        other = rng.choice(pool)
        nearby = mutations(other, rng) if rng.random() < 0.5 else []
        return rng.choice(nearby) if nearby else other
    return rng.choice(options)


def make_variant(cases, pools, rng):
    name, args = rng.choice(cases)
    pool = pools[(name, len(args))]
    changed = set(rng.sample(range(len(args)), rng.randint(1, len(args))))
    return name, tuple(
        pick_value(a, pool[i], rng) if i in changed else a for i, a in enumerate(args)
    )


def candidates(cases, count, rng, dropped):
    """Distinct new inputs, in a seeded order; repeats and inputs equal to a
    visible case are counted, not silently skipped."""
    pools = pools_of(cases)
    seen = {(n, repr(a)) for n, a in cases}
    found = []
    for _ in range(max(200, count * 20)):
        if len(found) >= count * 3:
            break
        name, args = make_variant(cases, pools, rng)
        key = (name, repr(args))
        if key in seen:
            dropped["duplicate"] += 1
            continue
        seen.add(key)
        if stable_repr(args) is None:
            dropped["unstable_repr"] += 1
            continue
        found.append((name, args))
    found.sort(key=lambda c: (c[0], repr(c[1])))
    rng.shuffle(found)
    return found


def ask(fn, args, seconds):
    """('ok', result), or why the reference gave none."""
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        return "ok", fn(*copy.deepcopy(args))
    except CaseTimeout:
        return "timeout", None
    except (Exception, SystemExit):
        return "reference_error", None
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


def load_reference(path):
    quiet, loud = open(os.devnull, "w"), sys.stdout
    sys.stdout = quiet
    try:
        return runpy.run_path(path, run_name="emi_reference")
    except BaseException as e:
        sys.exit(f"the reference could not be loaded: {type(e).__name__}: {e}")
    finally:
        sys.stdout = loud


def collect(tree):
    finder = Finder()
    finder.visit(tree)
    found = [visible_case(n, stack, finder.calls) for n, stack in finder.asserts]
    cases = [c for c in found if c]
    return cases, len(found) - len(cases)


def write_cases(req, cases, rng, dropped):
    namespace = load_reference(req["reference"])
    order = candidates(cases, req["count"], rng, dropped)
    out, deadline, stopped = [], time.monotonic() + req["total_timeout"], False
    signal.signal(signal.SIGALRM, _on_alarm)
    sys.stdout = open(os.devnull, "w")
    try:
        for name, args in order:
            if len(out) >= req["count"]:
                break
            if time.monotonic() > deadline:
                stopped = True
                break
            fn = namespace.get(name.rsplit(".", 1)[-1])
            state, result = ("reference_error", None) if not callable(fn) else ask(fn, args, req["case_timeout"])
            expected = stable_repr(result) if state == "ok" else None
            if state == "ok" and expected is None:
                state = "unstable_repr"
            if state != "ok":
                dropped[state] += 1
                continue
            out.append({"call": name, "args": ", ".join(map(repr, args)), "expected": expected})
    finally:
        sys.stdout = sys.__stdout__
    return out, stopped


def main():
    req = json.loads(sys.argv[1])
    tree = ast.parse(req["source"])
    cases, unparsed = collect(tree)
    dropped = dict.fromkeys(DROPS, 0)
    dropped["unparsed_visible"] = unparsed
    imports = [ast.unparse(n) for n in tree.body if isinstance(n, (ast.Import, ast.ImportFrom))]
    written, stopped = [], False
    if cases:
        written, stopped = write_cases(req, cases, random.Random(req["seed"]), dropped)
    print(json.dumps({"imports": imports, "cases": written, "dropped": dropped, "stopped_early": stopped}))


main()
